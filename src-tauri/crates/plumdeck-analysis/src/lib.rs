//! plumdeck's own track analysis and the analysis worker protocol V1
//! (W1-08; backend/domain/services/analysis/worker_cli.py).
//!
//! One decode at 11.025 kHz mono feeds the rhythm (whole track), key and
//! timbre (a centred 30-second window) and the 500-point overview waveform.
//! The `embedding` component decodes separately at 16 kHz for MusiCNN
//! (see [`musicnn`]).

pub mod dsp;
pub mod musicnn;

use std::io::{BufRead, Write};
#[allow(unused_imports)]
use std::io::Read as _;
use std::path::Path;

use serde_json::{json, Map, Value};

pub const PROTOCOL_PREFIX: &str = "PLUMDECK_ANALYSIS_WORKER_V1:";
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// Component versions this analyzer writes (they differ from Essentia's on
/// purpose: a stored value says which implementation produced it).
pub const RHYTHM_VERSION: &str = "rust-attacks-v1";
pub const KEY_VERSION: &str = "rust-chroma-v1";
pub const TIMBRE_VERSION: &str = "rust-timbre-v1";
pub const WAVEFORM_VERSION: &str = "waveform-v1";

/// Progress events (`report(stage, label)`).
pub type Progress<'a> = &'a mut dyn FnMut(&str, String);

fn decode(path: &str) -> Result<Vec<f32>, String> {
    let pcm = plumdeck_decode::decode_mono(Path::new(path), plumdeck_decode::Basis::Canonical, Some(dsp::SAMPLE_RATE as u32), (dsp::MAX_SECONDS + 1) as f64)
        .map_err(|error| format!("音声をデコードできません: {error}"))?;
    if pcm.samples.len() > dsp::MAX_SECONDS * dsp::SAMPLE_RATE {
        return Err("Audio analysis supports tracks up to 30 minutes".into());
    }
    if pcm.samples.is_empty() || pcm.samples.iter().any(|x| !x.is_finite()) {
        return Err("Audio is empty or contains non-finite samples".into());
    }
    Ok(pcm.samples)
}

fn window(audio: &[f32]) -> (&[f32], f64) {
    let size = dsp::SAMPLE_RATE * dsp::WINDOW_SECONDS;
    let start = audio.len().saturating_sub(size) / 2;
    (&audio[start..(start + size).min(audio.len())], start as f64 / dsp::SAMPLE_RATE as f64)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

struct Rhythm {
    bpm: f64,
    ticks: Vec<f64>,
    confidence: f64,
}

fn rhythm(audio: &[f32], hint: Option<f64>, progress: Progress<'_>) -> Result<Rhythm, String> {
    progress("beat_grid", "全曲のビート位置を解析しています".into());
    let (bpm, ticks, confidence) = dsp::track_beats(audio, hint, &mut |done, total| {
        progress("beat_grid", format!("全曲のビートを解析しています（{done} / {total}秒）"));
    })?;
    if ticks.len() < 2 || ticks.iter().any(|t| !t.is_finite()) {
        return Err("The rhythm analyzer found no usable beats".into());
    }
    Ok(Rhythm { bpm, ticks, confidence })
}

fn rhythm_extra(rhythm: &Rhythm, extra: &mut Map<String, Value>) -> Result<(), String> {
    extra.insert("bpm_confidence".into(), json!(round2(rhythm.confidence)));
    extra.insert("playback_grid".into(), dsp::playback_grid(&rhythm.ticks, rhythm.bpm, Some(rhythm.confidence))?.unwrap_or(Value::Null));
    extra.insert("playback_grid_version".into(), json!(dsp::GRID_VERSION));
    extra.insert("beat_positions".into(), json!(rhythm.ticks));
    Ok(())
}

/// `analyze_track_file`: the new-track result (`_format_result` shape).
pub fn analyze_track(path: &str, skip_waveform: bool, external_lyrics: Option<&str>, skip: &[String], progress: Progress<'_>) -> Result<Value, String> {
    progress("metadata", "曲名・長さなどの音源情報を読み取っています".into());
    let info = plumdeck_media::tags::basic_info(Path::new(path));
    progress("decode", "全曲の音源を読み込んでいます（11.025kHz）".into());
    let audio = decode(path)?;
    let (excerpt, start) = window(&audio);
    let mut extra = Map::new();
    let mut components = Map::new();
    let (bpm_hint, _, tonic, scale, _) = dsp::rhythm_and_key(excerpt)?;
    let rhythm_skipped = skip.iter().any(|s| s == "rhythm");
    let key_skipped = skip.iter().any(|s| s == "key");
    let mut danceability = 0.0;
    let mut bpm = 0.0;
    if !rhythm_skipped {
        let found = rhythm(&audio, Some(bpm_hint), progress)?;
        rhythm_extra(&found, &mut extra)?;
        danceability = found.confidence.clamp(0.0, 1.0);
        bpm = round2(found.bpm);
        components.insert("rhythm".into(), json!(RHYTHM_VERSION));
    }
    if !key_skipped {
        components.insert("key".into(), json!(KEY_VERSION));
    }
    progress("timbre", "音量・明るさなどの基本情報を計算しています".into());
    let (energy, brightness, noisiness) = dsp::timbre(excerpt, dsp::SAMPLE_RATE);
    components.insert("timbre".into(), json!(TIMBRE_VERSION));
    if !skip_waveform {
        progress("waveform", "波形データを作成しています".into());
        extra.insert("waveform_peaks".into(), json!(dsp::waveform_peaks(&audio, 500)));
    }
    extra.insert("analysis_components".into(), Value::Object(components));
    extra.insert("analysis_level".into(), json!("full"));
    extra.insert("analysis_profile".into(), json!("full"));
    extra.insert("analysis_window_start_seconds".into(), json!((start * 1000.0).round() / 1000.0));
    extra.insert("analysis_sample_rate".into(), json!(dsp::SAMPLE_RATE));
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let trimmed = |value: &Option<String>, fallback: &str| value.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(fallback).to_owned();
    let year = info.year.as_deref().map(str::trim).map(|y| y.chars().take(4).collect::<String>()).filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit())).and_then(|y| y.parse::<i64>().ok());
    Ok(json!({
        "filepath": path,
        "title": trimmed(&info.title, &stem),
        "artist": trimmed(&info.artist, "Unknown"),
        "album": trimmed(&info.album, "Unknown"),
        "genre": trimmed(&info.genre, "Unknown"),
        "year": year,
        "lyrics": external_lyrics,
        "duration": info.duration.unwrap_or(0.0),
        "bpm": bpm,
        "key": if key_skipped { String::new() } else { format!("{tonic} {scale}") },
        "scale": if key_skipped { "" } else { scale },
        "energy": round2(energy),
        "danceability": round2(danceability),
        "brightness": round2(brightness),
        "noisiness": round2(noisiness),
        "analysis_level": "full",
        "features_extra": extra,
    }))
}

/// `analyze_selected`: only the requested components, from one decode.
pub fn analyze_selected(path: &str, features: &[String], progress: Progress<'_>) -> Result<Value, String> {
    const KNOWN: [&str; 5] = ["embedding", "rhythm", "key", "timbre", "waveform"];
    if features.is_empty() || features.iter().any(|f| !KNOWN.contains(&f.as_str())) {
        return Err("Unknown or empty analysis components".into());
    }
    let mut result = Map::new();
    let mut extra = Map::new();
    let mut components = Map::new();
    if features.iter().any(|f| f == "embedding") {
        progress("decode", "類似曲検索用に音源を読み込んでいます（16kHz）".into());
        let pcm = plumdeck_decode::decode_mono(Path::new(path), plumdeck_decode::Basis::Canonical, Some(musicnn::SAMPLE_RATE as u32), (dsp::MAX_SECONDS + 1) as f64)
            .map_err(|error| format!("音声をデコードできません: {error}"))?;
        if pcm.samples.is_empty() || pcm.samples.iter().any(|x| !x.is_finite()) {
            return Err("Audio is empty or contains non-finite samples".into());
        }
        let mut embedding = musicnn::embed(&pcm.samples, &mut *progress)?;
        if let Some(Value::Object(more)) = embedding.get_mut("features_extra").map(Value::take) {
            extra.extend(more);
        }
        result.insert("embedding".into(), embedding["embedding"].take());
        result.insert("embedding_model".into(), embedding["embedding_model"].take());
        components.insert("embedding".into(), json!(musicnn::PIPELINE_VERSION));
        if features.len() == 1 {
            extra.insert("analysis_components".into(), Value::Object(components));
            result.insert("features_extra".into(), Value::Object(extra));
            return Ok(Value::Object(result));
        }
    }
    let audio = decode(path)?;
    let (excerpt, _) = window(&audio);
    let (bpm_hint, _, tonic, scale, _) = dsp::rhythm_and_key(excerpt)?;
    let mut confidence = None;
    if features.iter().any(|f| f == "rhythm") {
        let found = rhythm(&audio, Some(bpm_hint), progress)?;
        result.insert("bpm".into(), json!(round2(found.bpm)));
        rhythm_extra(&found, &mut extra)?;
        extra.insert("bpm_confidence".into(), json!(found.confidence));
        confidence = Some(found.confidence);
        components.insert("rhythm".into(), json!(RHYTHM_VERSION));
    }
    if features.iter().any(|f| f == "key") {
        result.insert("key".into(), json!(format!("{tonic} {scale}")));
        result.insert("scale".into(), json!(scale));
        components.insert("key".into(), json!(KEY_VERSION));
    }
    if features.iter().any(|f| f == "waveform") {
        extra.insert("waveform_peaks".into(), json!(dsp::waveform_peaks(&audio, 500)));
        components.insert("waveform".into(), json!(WAVEFORM_VERSION));
    }
    if features.iter().any(|f| f == "timbre") {
        let (energy, brightness, noisiness) = dsp::timbre(excerpt, dsp::SAMPLE_RATE);
        let dance = match confidence {
            Some(c) => c,
            None => dsp::track_beats(&audio, Some(bpm_hint), &mut |_, _| {}).map(|(_, _, c)| c).unwrap_or(0.0),
        };
        result.insert("energy".into(), json!(round2(energy)));
        result.insert("danceability".into(), json!(round2(dance.clamp(0.0, 1.0))));
        result.insert("brightness".into(), json!(round2(brightness)));
        result.insert("noisiness".into(), json!(round2(noisiness)));
        components.insert("timbre".into(), json!(TIMBRE_VERSION));
    }
    extra.insert("analysis_components".into(), Value::Object(components));
    result.insert("features_extra".into(), Value::Object(extra));
    Ok(Value::Object(result))
}

/// `rhythm_grid._extract`: `{bpm, ticks, confidence}`.
pub fn rhythm_grid(path: &str, progress: Progress<'_>) -> Result<Value, String> {
    let audio = decode(path)?;
    if audio.len() < 5 * dsp::SAMPLE_RATE {
        return Err("Grid analysis needs at least five seconds of valid audio".into());
    }
    if audio.iter().map(|x| x.abs()).fold(0.0f32, f32::max) < 1e-6 {
        return Err("No audible signal available for beat analysis".into());
    }
    let found = rhythm(&audio, None, progress)?;
    Ok(json!({"bpm": found.bpm, "ticks": found.ticks, "confidence": found.confidence}))
}

/// `analyze_components(filepath, features)`: (result, seconds), the file
/// checked unchanged and stamped like Python.
pub fn analyze_components(path: &str, features: &[String], progress: Progress<'_>) -> Result<Value, String> {
    let before = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let mut result = if features == ["rhythm"] {
        let grid = rhythm_grid(path, progress)?;
        let ticks: Vec<f64> = grid["ticks"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        let bpm = grid["bpm"].as_f64().unwrap_or(0.0);
        let confidence = grid["confidence"].as_f64().unwrap_or(0.0);
        json!({"bpm": bpm, "features_extra": {
            "beat_positions": ticks, "bpm_confidence": confidence,
            "playback_grid": dsp::playback_grid(&ticks, bpm, Some(confidence))?.unwrap_or(Value::Null),
            "playback_grid_version": dsp::GRID_VERSION, "analysis_components": {"rhythm": RHYTHM_VERSION},
        }})
    } else {
        analyze_selected(path, features, progress)?
    };
    let after = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = |m: &std::fs::Metadata| (m.len(), m.modified().ok());
    if stamp(&before) != stamp(&after) {
        return Err("Audio file changed during analysis; retry this track".into());
    }
    let mtime_ns = after.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
    result["features_extra"]["analysis_source"] = json!({"size": after.len(), "mtime_ns": mtime_ns});
    Ok(json!([result, started.elapsed().as_secs_f64()]))
}

/// Run one worker operation with its positional arguments.
pub fn run_operation(operation: &str, args: &[Value], progress: Progress<'_>) -> Result<Value, (String, String)> {
    let text = |index: usize| args.get(index).and_then(Value::as_str).map(str::to_owned);
    let error = |message: String| ("RuntimeError".to_owned(), message);
    match operation {
        "track" => {
            progress("initializing", "解析ワーカーを初期化しています".into());
            let path = text(0).ok_or_else(|| ("TypeError".to_owned(), "filepath is required".to_owned()))?;
            let skip_waveform = args.get(3).and_then(Value::as_bool).unwrap_or(false);
            let lyrics = text(4);
            let skip: Vec<String> = args.get(6).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            // Python answered None for a failed analysis; keep that contract.
            Ok(analyze_track(&path, skip_waveform, lyrics.as_deref(), &skip, progress).unwrap_or(Value::Null))
        }
        "components" => {
            let path = text(0).ok_or_else(|| ("TypeError".to_owned(), "filepath is required".to_owned()))?;
            let features: Vec<String> = args.get(1).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            analyze_components(&path, &features, progress).map_err(error)
        }
        "rhythm-grid" => rhythm_grid(&text(0).unwrap_or_default(), progress).map_err(error),
        "probe" => {
            progress("probe", "解析ワーカーとの通信を確認しています".into());
            if let Some(delay) = args.get(1).and_then(Value::as_f64).filter(|d| *d > 0.0) {
                std::thread::sleep(std::time::Duration::from_secs_f64(delay));
            }
            Ok(args.first().cloned().unwrap_or(Value::Null))
        }
        other => Err(("ValueError".into(), format!("Unknown analysis worker operation: {other}"))),
    }
}

fn send(out: &mut dyn Write, message: &Value) {
    let _ = writeln!(out, "{PROTOCOL_PREFIX}{}", serde_json::to_string(message).unwrap_or_default());
    let _ = out.flush();
}

/// `--analysis-worker`: read one request line, run it, write V1 messages.
/// Returns the process exit code.
pub fn run_worker(input: &mut dyn BufRead, out: &mut dyn Write) -> i32 {
    let out = std::cell::RefCell::new(out);
    let mut raw = Vec::new();
    let read = std::io::Read::take(&mut *input, (MAX_REQUEST_BYTES + 1) as u64).read_until(b'\n', &mut raw);
    let request: Result<Value, (String, String)> = match read {
        Ok(_) if raw.is_empty() || raw.len() > MAX_REQUEST_BYTES || !raw.ends_with(b"\n") => Err(("ValueError".into(), "Analysis worker request is missing or too large".into())),
        Ok(_) => serde_json::from_slice::<Value>(&raw).map_err(|e| ("JSONDecodeError".into(), e.to_string())),
        Err(e) => Err(("OSError".into(), e.to_string())),
    };
    let result = request.and_then(|request| {
        if request["version"] != 1 || !request["args"].is_array() {
            return Err(("ValueError".into(), "Unsupported analysis worker request".into()));
        }
        send(*out.borrow_mut(), &json!({"type": "progress", "event": {"stage": "worker_ready", "label": "解析ワーカーを開始しました"}}));
        let operation = request["operation"].as_str().unwrap_or("").to_owned();
        let args = request["args"].as_array().cloned().unwrap_or_default();
        let mut progress = |stage: &str, label: String| {
            send(*out.borrow_mut(), &json!({"type": "progress", "event": {"stage": stage, "label": label}}));
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_operation(&operation, &args, &mut progress)))
            .unwrap_or_else(|_| Err(("PanicError".into(), "the analysis worker panicked".into())))
    });
    let mut out = out.into_inner();
    match result {
        Ok(value) => {
            send(out, &json!({"type": "result", "ok": true, "value": value}));
            0
        }
        Err((kind, message)) => {
            send(&mut out, &json!({"type": "result", "ok": false, "error": {"type": kind, "message": message}}));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_a_probe_and_rejects_unknown_operations() {
        let mut out = Vec::new();
        let code = run_worker(&mut "{\"version\":1,\"operation\":\"probe\",\"args\":[42]}\n".as_bytes(), &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, 0);
        assert!(text.lines().last().unwrap().ends_with("{\"type\":\"result\",\"ok\":true,\"value\":42}"));
        let mut out = Vec::new();
        assert_eq!(run_worker(&mut "{\"version\":1,\"operation\":\"x\",\"args\":[]}\n".as_bytes(), &mut out), 1);
        let mut out = Vec::new();
        assert_eq!(run_worker(&mut "".as_bytes(), &mut out), 1);
    }
}
