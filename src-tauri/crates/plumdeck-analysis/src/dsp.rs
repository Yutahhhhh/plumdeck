//! The light DSP at 11.025 kHz (light_dsp.py, light_grid.py,
//! beat_alignment.py, beat_grid.py): onset flux, autocorrelation tempo,
//! chroma key, a dynamic-programming beat tracker, attack alignment and the
//! playback grid fit. Numbers follow the NumPy code step by step.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const SAMPLE_RATE: usize = 11_025;
pub const WINDOW_SECONDS: usize = 30;
pub const FRAME_SIZE: usize = 2048;
pub const HOP_SIZE: usize = 128;
pub const MAX_SECONDS: usize = 1800;
pub const GRID_VERSION: i64 = 2;

pub const NORM_ENERGY: (f64, f64) = (0.0, 0.4);
pub const NORM_BRIGHTNESS: (f64, f64) = (500.0, 5000.0);
pub const NORM_NOISINESS: (f64, f64) = (0.0, 0.15);

const NOTES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// `np.hanning(n)`: the symmetric Hann window.
pub fn hanning(n: usize) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()).collect()
}

struct Rfft {
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    buffer: Vec<Complex<f64>>,
}

impl Rfft {
    fn new(size: usize) -> Self {
        Self { fft: FftPlanner::new().plan_fft_forward(size), window: hanning(size), buffer: vec![Complex::default(); size] }
    }

    /// `|rfft(frame * hann)|` into `out` (size/2 + 1 bins).
    fn magnitudes(&mut self, frame: &[f32], out: &mut [f64]) {
        for (slot, (x, w)) in self.buffer.iter_mut().zip(frame.iter().zip(&self.window)) {
            *slot = Complex::new(f64::from(*x) * w, 0.0);
        }
        self.fft.process(&mut self.buffer);
        for (o, c) in out.iter_mut().zip(&self.buffer) {
            *o = c.norm();
        }
    }
}

/// Spectral flux per frame: mean over bins of the rise of `log1p|X|`.
fn onset_envelope(audio: &[f32], size: usize, frames: usize, mut tonal: Option<&mut [f64]>) -> Vec<f64> {
    let bins = size / 2 + 1;
    let mut rfft = Rfft::new(size);
    let mut magnitude = vec![0.0; bins];
    let mut previous: Option<Vec<f64>> = None;
    let mut onset = Vec::with_capacity(frames);
    for index in 0..frames {
        let start = index * HOP_SIZE;
        rfft.magnitudes(&audio[start..start + size], &mut magnitude);
        if let Some(tonal) = tonal.as_deref_mut() {
            for (t, m) in tonal.iter_mut().zip(&magnitude) {
                *t += m * m;
            }
        }
        let compressed: Vec<f64> = magnitude.iter().map(|m| m.ln_1p()).collect();
        let before = previous.as_ref().unwrap_or(&compressed);
        let rise: f64 = compressed.iter().zip(before).map(|(c, b)| (c - b).max(0.0)).sum();
        onset.push(rise / bins as f64);
        previous = Some(compressed);
    }
    onset
}

/// `np.median`.
pub fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Linear autocorrelation `r[0..n)` via a zero-padded FFT (as `_tempo`).
fn autocorrelation(envelope: &[f64]) -> Vec<f64> {
    let n = envelope.len();
    let size = (2 * n - 1).next_power_of_two();
    let mut planner = FftPlanner::new();
    let forward = planner.plan_fft_forward(size);
    let inverse = planner.plan_fft_inverse(size);
    let mut buffer: Vec<Complex<f64>> = envelope.iter().map(|x| Complex::new(*x, 0.0)).chain(std::iter::repeat(Complex::default())).take(size).collect();
    forward.process(&mut buffer);
    for value in &mut buffer {
        *value = Complex::new(value.norm_sqr(), 0.0);
    }
    inverse.process(&mut buffer);
    buffer.iter().take(n).map(|c| c.re / size as f64).collect()
}

/// `_tempo`: (bpm, confidence) of an onset envelope.
pub fn tempo(onset: &[f64]) -> (f64, f64) {
    let max = onset.iter().cloned().fold(f64::MIN, f64::max);
    if onset.len() < 4 || max < 1e-5 {
        return (0.0, 0.0);
    }
    let middle = median(onset);
    let envelope: Vec<f64> = onset.iter().map(|x| (x - middle).max(0.0)).collect();
    let correlation = autocorrelation(&envelope);
    if correlation[0] <= 1e-12 {
        return (0.0, 0.0);
    }
    let rate = SAMPLE_RATE as f64 / HOP_SIZE as f64;
    let first = ((rate * 60.0 / 240.0) as usize).max(1);
    let last = (envelope.len() - 2).min((rate * 60.0 / 60.0) as usize);
    if last <= first {
        return (0.0, 0.0);
    }
    let peaks: Vec<usize> = (first..=last).filter(|&lag| correlation[lag] >= correlation[lag - 1] && correlation[lag] >= correlation[lag + 1]).collect();
    if peaks.is_empty() {
        return (0.0, 0.0);
    }
    let best = peaks.iter().map(|&lag| correlation[lag]).fold(f64::MIN, f64::max);
    let lag = *peaks.iter().find(|&&lag| correlation[lag] >= 0.98 * best).expect("a contender");
    let (left, peak, right) = (correlation[lag - 1], correlation[lag], correlation[lag + 1]);
    let denominator = left - 2.0 * peak + right;
    let offset = if denominator < 0.0 { (0.5 * (left - right) / denominator).clamp(-0.5, 0.5) } else { 0.0 };
    let confidence = (peak / correlation[0]).clamp(0.0, 1.0);
    (60.0 * rate / (lag as f64 + offset), confidence)
}

/// `np.rint`: round half to even.
fn rint(x: f64) -> f64 {
    let rounded = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        rounded
    }
}

/// `_key`: (tonic, "major"|"minor", strength) of a summed power spectrum.
pub fn key(power: &[f64]) -> (&'static str, &'static str, f64) {
    let bin_hz = SAMPLE_RATE as f64 / FRAME_SIZE as f64;
    let mut chroma = [0.0f64; 12];
    let logs: Vec<f64> = power.iter().map(|p| p.max(1e-20).ln()).collect();
    for k in 1..power.len() - 1 {
        let frequency = k as f64 * bin_hz;
        if !(power[k] > power[k - 1] && power[k] >= power[k + 1]) || !(65.0..=4000.0).contains(&frequency) {
            continue;
        }
        let denominator = logs[k - 1] - 2.0 * logs[k] + logs[k + 1];
        let offset = if denominator.abs() > 1e-12 { 0.5 * (logs[k - 1] - logs[k + 1]) / denominator } else { 0.0 };
        let pitch = 69.0 + 12.0 * ((k as f64 + offset.clamp(-0.5, 0.5)) * bin_hz / 440.0).log2();
        chroma[(rint(pitch) as i64).rem_euclid(12) as usize] += power[k].sqrt();
    }
    let mean = chroma.iter().sum::<f64>() / 12.0;
    let centered: Vec<f64> = chroma.iter().map(|c| c - mean).collect();
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mut scores = Vec::with_capacity(24);
    for profile in [MAJOR, MINOR] {
        let profile_mean = profile.iter().sum::<f64>() / 12.0;
        let profile: Vec<f64> = profile.iter().map(|p| p - profile_mean).collect();
        let denominator = (norm(&centered) * norm(&profile)).max(1e-12);
        for pitch in 0..12 {
            let dot: f64 = (0..12).map(|i| centered[i] * profile[(i + 12 - pitch) % 12]).sum();
            scores.push(dot / denominator);
        }
    }
    let mut best = 0;
    for (index, score) in scores.iter().enumerate() {
        if *score > scores[best] {
            best = index;
        }
    }
    (NOTES[best % 12], if best < 12 { "major" } else { "minor" }, scores[best].max(0.0))
}

/// `rhythm_and_key`: (bpm, confidence, tonic, scale, strength) of an excerpt.
pub fn rhythm_and_key(audio: &[f32]) -> Result<(f64, f64, &'static str, &'static str, f64), String> {
    if audio.is_empty() || audio.iter().any(|x| !x.is_finite()) {
        return Err("Audio is empty or contains non-finite samples".into());
    }
    if audio.len() > SAMPLE_RATE * WINDOW_SECONDS {
        return Err("Light DSP requires a bounded excerpt".into());
    }
    let mut padded = audio.to_vec();
    if padded.len() < FRAME_SIZE {
        padded.resize(FRAME_SIZE, 0.0);
    }
    let frames = (padded.len() - FRAME_SIZE) / HOP_SIZE + 1;
    let mut tonal = vec![0.0; FRAME_SIZE / 2 + 1];
    let onset = onset_envelope(&padded, FRAME_SIZE, frames, Some(&mut tonal));
    let (bpm, confidence) = tempo(&onset);
    let (tonic, scale, strength) = key(&tonal);
    Ok((bpm, confidence, tonic, scale, strength))
}

/// `align_beats`: move each tick to a nearby attack.
pub fn align_beats(audio: &[f32], ticks: &[f64], sample_rate: usize) -> Vec<f64> {
    if ticks.len() < 2 {
        return ticks.to_vec();
    }
    let hop = ((sample_rate as f64 / 1000.0).round() as usize).max(1);
    let count = audio.len() / hop;
    if count < 20 {
        return ticks.to_vec();
    }
    let mut energy = vec![0.0f64; count];
    for (frame, slot) in energy.iter_mut().enumerate() {
        let mut sum = 0.0;
        for i in frame * hop..(frame + 1) * hop {
            let previous = if i == 0 { 0.0 } else { f64::from(audio[i - 1]) };
            let d = f64::from(audio[i]) - previous;
            sum += d * d;
        }
        *slot = sum / hop as f64;
    }
    let rate = sample_rate as f64 / hop as f64;
    let rise: Vec<f64> = (0..count).map(|i| (energy[i] - if i >= 5 { energy[i - 5] } else { 0.0 }).max(0.0)).collect();
    let mut aligned = ticks.to_vec();
    let mut matched = vec![false; ticks.len()];
    for (i, &tick) in ticks.iter().enumerate() {
        let before = if i > 0 { ticks[i] - ticks[i - 1] } else { f64::INFINITY };
        let after = if i + 1 < ticks.len() { ticks[i + 1] - tick } else { f64::INFINITY };
        let radius = 0.080f64.min(before.min(after) * 0.18);
        let left = (((tick - radius) * rate) as i64).max(0) as usize;
        let right = ((((tick + radius) * rate) as i64 + 1).max(0) as usize).min(count);
        if right <= left {
            continue;
        }
        let local = &rise[left..right];
        let mut peak = left;
        for (offset, value) in local.iter().enumerate() {
            if *value > rise[peak] {
                peak = left + offset;
            }
        }
        let strength = rise[peak];
        let loudest = energy[left..right].iter().cloned().fold(f64::MIN, f64::max);
        if strength < 1e-10f64.max(median(local) * 4.0).max(loudest * 0.08) {
            continue;
        }
        let mut edge = peak;
        while edge > left && rise[edge - 1] > strength * 0.15 {
            edge -= 1;
        }
        let mean = |slice: &[f64]| if slice.is_empty() { f64::NAN } else { slice.iter().sum::<f64>() / slice.len() as f64 };
        let before_energy = if edge > 0 { mean(&energy[edge.saturating_sub(12)..edge]) } else { 0.0 };
        let after_energy = mean(&energy[edge..(edge + 12).min(count)]);
        if !(after_energy > 1e-10f64.max(before_energy * 1.5)) {
            continue;
        }
        aligned[i] = edge as f64 / rate;
        matched[i] = true;
    }
    let share = matched.iter().filter(|m| **m).count() as f64 / matched.len() as f64;
    if ticks.len() >= 16 && share > 0.8 {
        let first = (0..ticks.len().min(3)).find(|&i| matched[i]).unwrap_or(0);
        let lower = ticks.len().saturating_sub(4);
        let last = (lower + 1..ticks.len()).rev().find(|&i| matched[i]).unwrap_or(ticks.len() - 1);
        let last = if ticks.len() >= 4 { last } else { ticks.len() - 1 };
        aligned = aligned[first..=last].to_vec();
    }
    aligned
}

/// `playback_grid`: a constant fit when the residuals prove one, else the beats.
pub fn playback_grid(ticks: &[f64], bpm: f64, confidence: Option<f64>) -> Result<Option<serde_json::Value>, String> {
    use serde_json::json;
    if ticks.len() < 2 || !bpm.is_finite() || !(20.0..=300.0).contains(&bpm) {
        return Ok(None);
    }
    if ticks.iter().any(|t| !t.is_finite() || *t < 0.0) || ticks.windows(2).any(|w| w[1] <= w[0]) {
        return Err("Invalid beat timestamps".into());
    }
    let mut grid = json!({
        "bpm": bpm, "first_beat_ms": ticks[0] * 1000.0, "beat_times_ms": ticks.iter().map(|t| t * 1000.0).collect::<Vec<_>>(),
        "source": "analysis", "confidence": confidence,
    });
    if ticks.len() < 16 {
        return Ok(Some(grid));
    }
    let n = ticks.len() as f64;
    let center = (n - 1.0) / 2.0;
    let mean = fsum(ticks.iter().copied()) / n;
    let period = fsum(ticks.iter().enumerate().map(|(i, t)| (i as f64 - center) * (t - mean))) / (n * (n * n - 1.0) / 12.0);
    let origin = mean - period * center;
    let mut residuals: Vec<f64> = ticks.iter().enumerate().map(|(i, t)| (t - (origin + i as f64 * period)).abs()).collect();
    residuals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = residuals[(n * 0.95).ceil() as usize - 1];
    if origin >= 0.0 && (0.2..=3.0).contains(&period) && p95 <= 0.020 && *residuals.last().expect("ticks") <= 0.050 {
        grid["bpm"] = json!(60.0 / period);
        grid["first_beat_ms"] = json!(origin * 1000.0);
        grid["beat_times_ms"] = serde_json::Value::Null;
    }
    Ok(Some(grid))
}

/// `math.fsum` (Neumaier compensation is enough for these sums).
fn fsum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut sum, mut compensation) = (0.0f64, 0.0f64);
    for value in values {
        let t = sum + value;
        if sum.abs() >= value.abs() {
            compensation += (sum - t) + value;
        } else {
            compensation += (value - t) + sum;
        }
        sum = t;
    }
    sum + compensation
}

/// `light_grid.analyze`: (bpm, ticks, confidence) of a whole track.
pub fn track_beats(audio: &[f32], bpm_hint: Option<f64>, progress: &mut dyn FnMut(usize, usize)) -> Result<(f64, Vec<f64>, f64), String> {
    if audio.len() < SAMPLE_RATE * 5 || audio.len() > SAMPLE_RATE * MAX_SECONDS || audio.iter().any(|x| !x.is_finite()) {
        return Err("ビート解析には5秒以上30分以内の音源が必要です".into());
    }
    let hint = match bpm_hint {
        Some(hint) => hint,
        None => {
            let start = audio.len().saturating_sub(SAMPLE_RATE * WINDOW_SECONDS) / 2;
            rhythm_and_key(&audio[start..(start + SAMPLE_RATE * WINDOW_SECONDS).min(audio.len())])?.0
        }
    };
    let size = 512;
    let mut padded = vec![0.0f32; size / 2];
    padded.extend_from_slice(audio);
    padded.extend(std::iter::repeat_n(0.0, size / 2));
    let frames = (padded.len() - size) / HOP_SIZE + 1;
    let onset = onset_envelope(&padded, size, frames, None);
    let rate = SAMPLE_RATE as f64 / HOP_SIZE as f64;
    let width = (24.0 * rate) as usize;
    let step = (8.0 * rate) as usize;
    let mut centers = Vec::new();
    let mut tempos = Vec::new();
    let mut confidence = Vec::new();
    let mut center = 0;
    while center < onset.len() {
        let left = (center as i64 - (width / 2) as i64).min(onset.len() as i64 - width as i64).max(0) as usize;
        let (mut bpm, strength) = tempo(&onset[left..(left + width).min(onset.len())]);
        if bpm > 0.0 && hint > 0.0 {
            while bpm < hint * 0.65 {
                bpm *= 2.0;
            }
            while bpm > hint * 1.5 {
                bpm /= 2.0;
            }
        }
        centers.push(center);
        tempos.push(bpm);
        confidence.push(strength);
        center += step;
    }
    let max_onset = onset.iter().cloned().fold(f64::MIN, f64::max);
    let valid: Vec<usize> = (0..tempos.len()).filter(|&i| tempos[i] > 0.0).collect();
    if valid.is_empty() || max_onset < 1e-5 {
        return Err("音源からビートを検出できませんでした".into());
    }
    let xs: Vec<f64> = valid.iter().map(|&i| centers[i] as f64).collect();
    let ys: Vec<f64> = valid.iter().map(|&i| 60.0 * rate / tempos[i]).collect();
    let interp = |x: f64| -> f64 {
        if x <= xs[0] {
            return ys[0];
        }
        if x >= *xs.last().expect("valid") {
            return *ys.last().expect("valid");
        }
        let j = xs.partition_point(|v| *v <= x);
        let (x0, x1, y0, y1) = (xs[j - 1], xs[j], ys[j - 1], ys[j]);
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    };
    let mean = onset.iter().sum::<f64>() / onset.len() as f64;
    let std = (onset.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / onset.len() as f64).sqrt().max(1e-8);
    let mut scores = vec![0.0f64; onset.len()];
    let mut previous = vec![-1i64; onset.len()];
    let seconds = audio.len() / SAMPLE_RATE;
    let report_every = (rate * 30.0) as usize;
    for i in 0..onset.len() {
        let period = interp(i as f64);
        let low = ((period * 0.55) as usize).max(1);
        let high = (period * 1.8) as usize;
        let mut best: Option<(f64, usize)> = None;
        for lag in low..=high.min(i) {
            let candidate = scores[i - lag] - 100.0 * (lag as f64 / period).ln().powi(2);
            if best.is_none_or(|(b, _)| candidate > b) {
                best = Some((candidate, lag));
            }
        }
        if let Some((value, lag)) = best {
            if value > 0.0 {
                previous[i] = (i - lag) as i64;
                scores[i] = value;
            }
        }
        scores[i] += onset[i] / std;
        if i > 0 && report_every > 0 && i % report_every == 0 {
            progress((i as f64 / rate) as usize, seconds);
        }
    }
    let mut end = 0;
    for (i, score) in scores.iter().enumerate() {
        if *score > scores[end] {
            end = i;
        }
    }
    let mut beats = Vec::new();
    let mut cursor = end as i64;
    while cursor >= 0 {
        beats.push(cursor as usize);
        cursor = previous[cursor as usize];
    }
    beats.reverse();
    let threshold = max_onset * 0.05;
    let first_audible = onset.iter().position(|x| *x > threshold).unwrap_or(0);
    let last_audible = onset.iter().rposition(|x| *x > threshold).unwrap_or(0);
    beats.retain(|&i| first_audible <= i && i <= last_audible);
    if beats.len() < 2 {
        return Err("音源から十分なビートを検出できませんでした".into());
    }
    let raw: Vec<f64> = beats.iter().map(|&i| i as f64 / rate).collect();
    let ticks = align_beats(audio, &raw, SAMPLE_RATE);
    let intervals: Vec<f64> = ticks.windows(2).map(|w| w[1] - w[0]).collect();
    let bpm = 60.0 / median(&intervals);
    let grid = playback_grid(&ticks, bpm, None)?.ok_or("TypeError: 'NoneType' object is not subscriptable")?;
    let fitted = grid["bpm"].as_f64().unwrap_or(bpm);
    let strengths: Vec<f64> = valid.iter().map(|&i| confidence[i]).collect();
    Ok((fitted, ticks, strengths.iter().sum::<f64>() / strengths.len() as f64))
}

/// `light_grid.waveform_peaks`: `count` normalized peaks.
pub fn waveform_peaks(audio: &[f32], count: usize) -> Vec<f64> {
    let columns = count.min(audio.len());
    if columns == 0 {
        return Vec::new();
    }
    let step = audio.len() as f64 / columns as f64;
    let edges: Vec<usize> = (0..=columns).map(|i| if i == columns { audio.len() } else { (i as f64 * step) as usize }).collect();
    let peaks: Vec<f64> = edges.windows(2).map(|w| audio[w[0]..w[1]].iter().map(|x| f64::from(x.abs())).fold(0.0, f64::max)).collect();
    let max = peaks.iter().cloned().fold(0.0, f64::max).max(1e-8);
    peaks.iter().map(|p| (p / max).clamp(0.0, 1.0)).collect()
}

/// `_extract_light_features`: energy, brightness and noisiness of a window.
pub fn timbre(audio: &[f32], sample_rate: usize) -> (f64, f64, f64) {
    let rms = (audio.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / audio.len().max(1) as f64).sqrt();
    let noisiness = if audio.len() > 1 { audio.windows(2).filter(|w| w[0].is_sign_negative() != w[1].is_sign_negative()).count() as f64 / (audio.len() - 1) as f64 } else { 0.0 };
    let fft_samples = audio.len().min(sample_rate * 10);
    let start = (audio.len() - fft_samples) / 2;
    let mut sample = audio[start..start + fft_samples].to_vec();
    let (frame, hop) = (2048, 1024);
    if sample.len() < frame {
        sample.resize(frame, 0.0);
    }
    let frames = (sample.len() - frame) / hop + 1;
    let mut rfft = Rfft::new(frame);
    let bins = frame / 2 + 1;
    let mut magnitude = vec![0.0; bins];
    let mut spectrum = vec![0.0; bins];
    for index in 0..frames {
        rfft.magnitudes(&sample[index * hop..index * hop + frame], &mut magnitude);
        for (s, m) in spectrum.iter_mut().zip(&magnitude) {
            *s += m;
        }
    }
    for s in &mut spectrum {
        *s /= frames as f64;
    }
    let total: f64 = spectrum.iter().sum();
    let centroid = if total > 0.0 { spectrum.iter().enumerate().map(|(k, s)| s * k as f64 * sample_rate as f64 / frame as f64).sum::<f64>() / total } else { 0.0 };
    (
        (rms / NORM_ENERGY.1).clamp(0.0, 1.0),
        ((centroid - NORM_BRIGHTNESS.0) / (NORM_BRIGHTNESS.1 - NORM_BRIGHTNESS.0)).clamp(0.0, 1.0),
        (noisiness / NORM_NOISINESS.1).clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicks(bpm: f64, seconds: f64) -> Vec<f32> {
        let mut audio = vec![0.0f32; (seconds * SAMPLE_RATE as f64) as usize];
        let period = 60.0 / bpm;
        let mut t = 0.5;
        while t < seconds {
            let start = (t * SAMPLE_RATE as f64) as usize;
            for i in 0..200.min(audio.len().saturating_sub(start)) {
                audio[start + i] = (0.8 * (-(i as f32) / 40.0).exp()) * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
            t += period;
        }
        audio
    }

    #[test]
    fn finds_the_tempo_of_a_click_track() {
        let audio = clicks(124.0, 30.0);
        let (bpm, ticks, _) = track_beats(&audio, None, &mut |_, _| {}).unwrap();
        assert!((bpm - 124.0).abs() < 0.5, "bpm {bpm}");
        assert!(ticks.len() > 40);
    }

    #[test]
    fn names_the_key_of_a_triad() {
        let mut audio = vec![0.0f32; SAMPLE_RATE * 10];
        for (i, x) in audio.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            *x = [220.0f32, 277.18, 329.63].iter().map(|f| (2.0 * std::f32::consts::PI * f * t).sin() * 0.2).sum();
        }
        let (_, _, tonic, scale, _) = rhythm_and_key(&audio).unwrap();
        assert_eq!((tonic, scale), ("A", "major"));
    }

    #[test]
    fn fits_a_constant_grid() {
        let ticks: Vec<f64> = (0..32).map(|i| 0.25 + i as f64 * 0.5).collect();
        let grid = playback_grid(&ticks, 120.0, None).unwrap().unwrap();
        assert!((grid["bpm"].as_f64().unwrap() - 120.0).abs() < 1e-9);
        assert!(grid["beat_times_ms"].is_null());
    }
}
