//! The similarity embedding: MusiCNN (msd-musicnn-1) on three 3-second
//! windows at 16 kHz, averaged (backend/domain/services/analysis/portable.py
//! `_extract_light_embedding`, the background embedding job's profile).
//!
//! The model is the frozen TensorFlow graph the app already shipped, run by
//! tract. Its input is Essentia's TensorflowInputMusiCNN bands, computed as
//! the NumPy port did: a centred 512-sample symmetric Hann, hop 256, Slaney
//! area-normalized 96 mel power bands and log10(1 + 10000 x).

use std::sync::OnceLock;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde_json::{json, Value};
use tract_tensorflow::prelude::*;
use tract_tensorflow::tract_hir::internal::*;
use tract_tensorflow::tract_hir::ops::array::{Pad, PadMode};
use tract_tensorflow::tract_hir::ops::konst::Const;

pub const SAMPLE_RATE: usize = 16_000;
pub const PIPELINE_VERSION: &str = "musicnn-16khz-v1";
pub const MODEL: &str = "msd-musicnn-1:musicnn-16khz-v1";
pub const DIM: usize = 200;
const FRAME: usize = 512;
const HOP: usize = 256;
const BANDS: usize = 96;
const PATCH: usize = 187;
const WINDOW_SECONDS: f64 = 3.0;

static GRAPH: &[u8] = include_bytes!("../models/msd-musicnn-1.pb");

type Plan = TypedRunnableModel<TypedModel>;

/// The graph's `Pad` nodes take I32 paddings, which tract's TensorFlow `Pad`
/// cannot type; they are constant, so each becomes a core `Pad`.
fn load() -> TractResult<Plan> {
    let mut model = tract_tensorflow::tensorflow().model_for_read(&mut std::io::Cursor::new(GRAPH))?;
    let pads: Vec<usize> = model.nodes().iter().filter(|n| n.op().name() == "Pad").map(|n| n.id).collect();
    for id in pads {
        let source = model.node(id).inputs[1].node;
        let paddings = model.node(source).op_as::<Const>().context("constant paddings")?.val().clone();
        let view = paddings.to_array_view::<i32>()?;
        let pairs = (0..view.shape()[0]).map(|row| (view[[row, 0]].max(0) as usize, view[[row, 1]].max(0) as usize)).collect();
        let node = model.node_mut(id);
        node.op = Box::new(Pad::new(pairs, PadMode::Constant(rctensor0(0f32))));
        node.inputs.truncate(1);
    }
    let input = model.node_by_name("model/Placeholder")?.id;
    model.set_input_outlets(&[OutletId::new(input, 0)])?;
    model.set_input_fact(0, f32::fact([1, PATCH, BANDS]).into())?;
    let output = model.node_by_name("model/dense/BiasAdd")?.id;
    model.set_output_outlets(&[OutletId::new(output, 0)])?;
    model.into_optimized()?.into_runnable()
}

fn plan() -> Result<&'static Plan, String> {
    static PLAN: OnceLock<Result<Plan, String>> = OnceLock::new();
    PLAN.get_or_init(|| load().map_err(|error| format!("MusiCNN model is unavailable: {error}"))).as_ref().map_err(Clone::clone)
}

/// `musicnn_mel_filters`: Slaney's piecewise mel scale, 96 triangles over
/// the 257 rFFT bins of a 512-point frame at 16 kHz (float32 weights).
fn mel_filters() -> Vec<Vec<f32>> {
    let step = 27.0 / 6.4f64.ln();
    let to_mel = |hz: f64| if hz >= 1000.0 { 15.0 + (hz.max(1.0) / 1000.0).ln() * step } else { hz / (200.0 / 3.0) };
    let top = to_mel(8000.0);
    let hz: Vec<f64> = (0..BANDS + 2)
        .map(|i| {
            let mel = top * i as f64 / (BANDS + 1) as f64;
            if mel >= 15.0 { 1000.0 * ((mel - 15.0) / step).exp() } else { (200.0 / 3.0) * mel }
        })
        .collect();
    let bins: Vec<f64> = (0..=FRAME / 2).map(|k| k as f64 * SAMPLE_RATE as f64 / FRAME as f64).collect();
    (0..BANDS)
        .map(|band| {
            let (lower, upper) = (hz[band + 1] - hz[band], hz[band + 2] - hz[band + 1]);
            let norm = (2.0 / (hz[band + 2] - hz[band])) as f32;
            bins.iter()
                .map(|bin| {
                    let rising = -(hz[band] - bin) / lower;
                    let falling = (hz[band + 2] - bin) / upper;
                    (rising.min(falling).max(0.0) as f32) * norm
                })
                .collect()
        })
        .collect()
}

/// `musicnn_bands(audio)`: frames x 96 log-mel bands.
pub fn bands(audio: &[f32]) -> Vec<[f32; BANDS]> {
    static FILTERS: OnceLock<Vec<Vec<f32>>> = OnceLock::new();
    let filters = FILTERS.get_or_init(mel_filters);
    let tail = HOP + (HOP - audio.len() % HOP) % HOP;
    let mut padded = vec![0f32; HOP];
    padded.extend_from_slice(audio);
    padded.extend(std::iter::repeat_n(0f32, tail));
    let hann: Vec<f64> = (0..FRAME).map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (FRAME - 1) as f64).cos()).collect();
    let fft = FftPlanner::<f64>::new().plan_fft_forward(FRAME);
    let mut buffer = vec![Complex::new(0.0, 0.0); FRAME];
    let frames = if padded.len() >= FRAME { (padded.len() - FRAME) / HOP + 1 } else { 0 };
    (0..frames)
        .map(|index| {
            let start = index * HOP;
            for (slot, (sample, weight)) in buffer.iter_mut().zip(padded[start..start + FRAME].iter().zip(&hann)) {
                *slot = Complex::new(f64::from(*sample) * weight, 0.0);
            }
            fft.process(&mut buffer);
            let power: Vec<f64> = buffer[..=FRAME / 2].iter().map(|c| c.norm_sqr()).collect();
            let mut row = [0f32; BANDS];
            for (band, weights) in filters.iter().enumerate() {
                let energy: f64 = power.iter().zip(weights).map(|(p, w)| p * f64::from(*w)).sum();
                row[band] = (1.0 + 10_000.0 * energy).log10() as f32;
            }
            row
        })
        .collect()
}

/// One 187-frame patch: wrapped when the window is short (`np.pad(...,
/// mode="wrap")`), else its first 187 frames.
fn patch(mut rows: Vec<[f32; BANDS]>) -> Result<Tensor, String> {
    if rows.is_empty() {
        return Err("MusiCNN window is empty".into());
    }
    let original = rows.len();
    let mut index = 0;
    while rows.len() < PATCH {
        rows.push(rows[index % original]);
        index += 1;
    }
    rows.truncate(PATCH);
    let flat: Vec<f32> = rows.iter().flat_map(|row| row.iter().copied()).collect();
    tract_ndarray::Array3::from_shape_vec((1, PATCH, BANDS), flat).map(Tensor::from).map_err(|e| e.to_string())
}

/// The window starts: 20%, 50% and 80% of the track, 3 s each, deduplicated
/// and rounded to milliseconds as Python's `round(x, 3)` does.
pub fn window_starts(duration: f64) -> Vec<f64> {
    let mut starts: Vec<f64> = [0.2, 0.5, 0.8]
        .iter()
        .map(|fraction| {
            let start = (duration * fraction - 1.5).min(duration - WINDOW_SECONDS).max(0.0);
            (start * 1000.0).round() / 1000.0
        })
        .collect();
    starts.sort_by(f64::total_cmp);
    starts.dedup();
    starts
}

/// `_extract_light_embedding` over 16 kHz mono PCM of the whole track: the
/// `embedding`, `embedding_model` and `features_extra` keys of a result.
pub fn embed(audio: &[f32], progress: &mut dyn FnMut(&str, String)) -> Result<Value, String> {
    progress("embedding", "類似曲検索用のモデルを準備しています".into());
    let plan = plan()?;
    let duration = audio.len() as f64 / SAMPLE_RATE as f64;
    let starts = window_starts(duration);
    let mut sum = vec![0f64; DIM];
    for (index, start) in starts.iter().enumerate() {
        progress("embedding", format!("類似曲検索用の特徴を解析しています（{} / {}区間）", index + 1, starts.len()));
        let from = ((start * SAMPLE_RATE as f64) as usize).min(audio.len());
        let to = (from + (WINDOW_SECONDS * SAMPLE_RATE as f64) as usize).min(audio.len());
        let input = patch(bands(&audio[from..to]))?;
        let output = plan.run(tvec!(input.into())).map_err(|e| format!("MusiCNN inference failed: {e}"))?;
        let vector = output[0].to_array_view::<f32>().map_err(|e| e.to_string())?;
        if vector.len() != DIM {
            return Err("MusiCNN produced an invalid embedding".into());
        }
        for (total, value) in sum.iter_mut().zip(vector.iter()) {
            *total += f64::from(*value);
        }
    }
    let mean: Vec<f64> = sum.iter().map(|total| (*total / starts.len() as f64) as f32 as f64).collect();
    if mean.iter().any(|x| !x.is_finite()) || mean.iter().all(|x| *x == 0.0) {
        return Err("MusiCNN produced an invalid embedding".into());
    }
    Ok(json!({
        "embedding": mean,
        "embedding_model": MODEL,
        "features_extra": {
            "embedding_sample_rate": SAMPLE_RATE,
            "embedding_pipeline_version": PIPELINE_VERSION,
            "embedding_windows_seconds": starts,
            "embedding_patch_count": starts.len(),
            "embedding_window_seconds": 3,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_bands_have_the_model_input_shape() {
        let filters = mel_filters();
        assert_eq!(filters.len(), BANDS);
        assert!(filters.iter().all(|row| row.len() == FRAME / 2 + 1 && row.iter().any(|w| *w > 0.0)));
        let tone: Vec<f32> = (0..SAMPLE_RATE * 3).map(|n| (2.0 * std::f32::consts::PI * 440.0 * n as f32 / SAMPLE_RATE as f32).sin() * 0.5).collect();
        let rows = bands(&tone);
        // NumPy's musicnn_bands on the same tone: 189 frames, these values.
        assert_eq!(rows.len(), 189);
        let expected = [(0, 0.000404847902), (5, 0.008259457536), (20, 0.05024429038), (40, 1.80281294888e-05), (95, 8.0375969e-08)];
        for (band, value) in expected {
            assert!((f64::from(rows[10][band]) - value).abs() <= value * 1e-3 + 1e-9, "band {band}: {}", rows[10][band]);
        }
        let total: f64 = rows.iter().flatten().map(|x| f64::from(*x)).sum();
        assert!((total - 5687.787109375).abs() < 0.05, "{total}");
        assert_eq!(window_starts(100.0), vec![18.5, 48.5, 78.5]);
        assert_eq!(window_starts(2.0), vec![0.0]);
    }

    #[test]
    fn runs_the_model_on_a_tone() {
        let tone: Vec<f32> = (0..SAMPLE_RATE * 10).map(|n| (2.0 * std::f32::consts::PI * 220.0 * n as f32 / SAMPLE_RATE as f32).sin() * 0.3).collect();
        let result = embed(&tone, &mut |_, _| {}).unwrap();
        assert_eq!(result["embedding"].as_array().unwrap().len(), DIM);
        // The graph itself: tract against TensorFlow 2 on a fixed patch.
        let x: Tensor = tract_ndarray::Array3::<f32>::from_shape_fn((1, PATCH, BANDS), |(_, t, m)| ((t * 7 + m * 3) % 11) as f32 * 0.1).into();
        let y = plan().unwrap().run(tvec!(x.into())).unwrap();
        let y = y[0].to_array_view::<f32>().unwrap();
        for (got, want) in y.iter().zip([-1.2488277f32, -0.15493739, -0.32652974, -1.7907926, 2.5323057]) {
            assert!((got - want).abs() < 1e-4, "{got} vs {want}");
        }
        assert_eq!(result["features_extra"]["embedding_patch_count"], 3);
    }
}
