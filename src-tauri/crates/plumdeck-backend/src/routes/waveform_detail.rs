//! The Play deck's detailed waveform (backend/api/routers/waveform_detail.py
//! over app/services/waveform_detail_service.py; S7): FFmpeg decodes to
//! mono 24 kHz with three band splits, peaks per 1/300 s, cached on disk.
//!
//! The cache is a plain binary file in its own folder (the Python cache is
//! NumPy `.npz`); both are disposable (deviation D-20).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::tracks::{get_track, BigInt};
use sha2::{Digest, Sha256};

use crate::query::Params;
use crate::response::{dict, float, int, text, PyJson};
use crate::routes::play::find_ffmpeg;
use crate::{AppState, HttpError};

const SAMPLE_RATE: usize = 24_000;
const BINS_PER_SECOND: usize = 300;
const SAMPLES_PER_BIN: usize = SAMPLE_RATE / BINS_PER_SECOND;
const MAX_SECONDS: usize = 3600;
const CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_MAX_FILES: usize = 128;
const MAGIC: &[u8; 8] = b"PDWFD\x01\0\0";

struct Failure(u16, &'static str);

fn cache_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches/plumdeck"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|h| PathBuf::from(h).join("plumdeck").join("plumdeck").join("Cache"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|p| p.join("plumdeck"))
    };
    base.unwrap_or_else(std::env::temp_dir).join("waveform-detail-rs-v1")
}

/// (values as rows of [full, low, mid, high], samples).
fn decode(path: &Path) -> Result<(Vec<[u8; 4]>, usize), Failure> {
    let decoder = find_ffmpeg().ok_or(Failure(503, "詳細波形には FFmpeg が必要です。FFmpeg をインストールしてください。"))?;
    let graph = "[0:a:0]aformat=sample_fmts=flt:channel_layouts=mono,aresample=24000,asplit=4[full][lo][mi][hi];\
                 [lo]lowpass=f=250[low];[mi]highpass=f=250,lowpass=f=2500[mid];[hi]highpass=f=2500[high];\
                 [full][low][mid][high]join=inputs=4:channel_layout=quad:map=0.0-FL|1.0-FR|2.0-BL|3.0-BR[out]";
    let mut child = Command::new(decoder)
        .args(["-nostdin", "-v", "error", "-threads", "1", "-i"])
        .arg(path)
        .args(["-filter_complex_threads", "1", "-filter_complex", graph, "-map", "[out]", "-t", &(MAX_SECONDS + 1).to_string()])
        .args(["-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure(503, "波形デコーダーを起動できませんでした。"))?;
    let started = std::time::Instant::now();
    let mut stdout = child.stdout.take().expect("piped");
    let frame = 16;
    let bin_bytes = SAMPLES_PER_BIN * frame;
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; bin_bytes * BINS_PER_SECOND];
    let mut values: Vec<[f32; 4]> = Vec::new();
    let mut samples = 0usize;
    let peak = |audio: &[u8]| {
        let mut out = [0f32; 4];
        for sample in audio.chunks_exact(frame) {
            for (channel, slot) in out.iter_mut().enumerate() {
                let x = f32::from_le_bytes(sample[channel * 4..channel * 4 + 4].try_into().expect("4 bytes")).abs();
                if x > *slot || x.is_nan() {
                    *slot = x;
                }
            }
        }
        out
    };
    let result = loop {
        if started.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            break Err(Failure(504, "詳細波形の生成がタイムアウトしました。"));
        }
        let read = match stdout.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(_) => break Ok(()),
        };
        pending.extend_from_slice(&chunk[..read]);
        let count = pending.len() / bin_bytes * bin_bytes;
        if count == 0 {
            continue;
        }
        samples += count / frame;
        if samples > MAX_SECONDS * SAMPLE_RATE {
            let _ = child.kill();
            break Err(Failure(422, "詳細波形は60分以内の音源に対応しています。"));
        }
        values.extend(pending[..count].chunks_exact(bin_bytes).map(peak));
        pending.drain(..count);
    };
    let status = child.wait();
    result?;
    let whole = pending.len() / frame * frame;
    if whole > 0 {
        samples += whole / frame;
        values.push(peak(&pending[..whole]));
    }
    if !status.map(|s| s.success()).unwrap_or(false) {
        return Err(Failure(422, "音源をデコードできません。ファイル形式または破損を確認してください。"));
    }
    if values.is_empty() || samples == 0 {
        return Err(Failure(422, "音源に再生可能な音声がありません。"));
    }
    if values.iter().flatten().any(|x| !x.is_finite()) {
        return Err(Failure(422, "音源に無効な音声サンプルがあります。"));
    }
    let quantize = |x: f32| (x.clamp(0.0, 1.0) as f64 * 255.0).round_ties_even() as u8;
    Ok((values.into_iter().map(|row| row.map(quantize)).collect(), samples))
}

fn read_cache(path: &Path) -> Option<(Vec<[u8; 4]>, usize)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 16 || &bytes[..8] != MAGIC || (bytes.len() - 16) % 4 != 0 {
        return None;
    }
    let samples = u64::from_le_bytes(bytes[8..16].try_into().ok()?) as usize;
    let values: Vec<[u8; 4]> = bytes[16..].chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    (samples > 0 && !values.is_empty()).then_some((values, samples))
}

fn write_cache(directory: &Path, target: &Path, values: &[[u8; 4]], samples: usize) {
    let mut bytes = Vec::with_capacity(16 + values.len() * 4);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(samples as u64).to_le_bytes());
    bytes.extend(values.iter().flatten());
    let temporary = directory.join(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(&temporary, target);
        prune(directory);
    }
    let _ = std::fs::remove_file(&temporary);
}

fn prune(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "bin"))
        .filter_map(|e| e.metadata().ok().map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path())))
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    let mut used = 0;
    for (index, (_, size, path)) in files.into_iter().enumerate() {
        used += size;
        if index >= CACHE_MAX_FILES || used > CACHE_MAX_BYTES {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// `_downsample`: the peak of each of `bins` groups.
fn downsample(values: &[[u8; 4]], bins: usize) -> Vec<[u8; 4]> {
    if bins == 0 || bins >= values.len() {
        return values.to_vec();
    }
    let step = values.len() as f64 / bins as f64;
    let edges: Vec<usize> = (0..=bins).map(|i| if i == bins { values.len() } else { (i as f64 * step) as usize }).collect();
    edges
        .windows(2)
        .map(|w| {
            let (start, stop) = (w[0], w[1].max(w[0] + 1));
            values[start..stop].iter().fold([0u8; 4], |acc, row| [acc[0].max(row[0]), acc[1].max(row[1]), acc[2].max(row[2]), acc[3].max(row[3])])
        })
        .collect()
}

fn payload(values: &[[u8; 4]], samples: usize, bins: Option<usize>) -> PyValue {
    let duration_ms = samples as f64 / SAMPLE_RATE as f64 * 1000.0;
    let reduced = match bins {
        Some(bins) if bins > 0 && bins < values.len() => Some(downsample(values, bins)),
        _ => None,
    };
    let rows = reduced.as_deref().unwrap_or(values);
    let density = match &reduced {
        None => int(BINS_PER_SECOND as i64),
        Some(rows) => float(rows.len() as f64 / (duration_ms / 1000.0)),
    };
    let column = |index: usize| PyValue::List(rows.iter().map(|row| PyValue::Int(row[index].into())).collect());
    dict([
        ("bins_per_second", density),
        ("duration_ms", float(duration_ms)),
        ("amplitude_scale", int(255)),
        ("source", text("decoded-audio")),
        ("bands_hz", PyValue::List(vec![int(250), int(2500)])),
        ("peaks", column(0)),
        ("low", column(1)),
        ("mid", column(2)),
        ("high", column(3)),
    ])
}

fn locks() -> &'static Vec<Mutex<()>> {
    static LOCKS: OnceLock<Vec<Mutex<()>>> = OnceLock::new();
    LOCKS.get_or_init(|| (0..32).map(|_| Mutex::new(())).collect())
}

fn waveform_detail(filepath: &str, bins: Option<usize>) -> Result<PyValue, Failure> {
    let missing = Failure(404, "音源ファイルが見つからないか、読み取れません。");
    let source = std::fs::canonicalize(filepath).map_err(|_| Failure(404, "音源ファイルが見つからないか、読み取れません。"))?;
    let stat = std::fs::metadata(&source).map_err(|_| Failure(404, "音源ファイルが見つからないか、読み取れません。"))?;
    if !stat.is_file() {
        return Err(missing);
    }
    let mtime = stat.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let identity = format!("v1:{}:{}:{mtime}", source.display(), stat.len());
    let digest = hex(&Sha256::digest(identity.as_bytes()));
    let directory = cache_dir();
    std::fs::create_dir_all(&directory).map_err(|_| Failure(503, "波形キャッシュに書き込めません。"))?;
    let target = directory.join(format!("{digest}.bin"));
    let lock = &locks()[usize::from_str_radix(&digest[..2], 16).unwrap_or(0) % 32];
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((values, samples)) = read_cache(&target) {
        return Ok(payload(&values, samples, bins));
    }
    let (values, samples) = decode(&source)?;
    let updated = std::fs::metadata(&source).map_err(|_| Failure(409, "音源が変更されました。詳細波形を再読み込みしてください。"))?;
    let updated_mtime = updated.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    if (updated.len(), updated_mtime) != (stat.len(), mtime) {
        return Err(Failure(409, "音源が変更されました。詳細波形を再読み込みしてください。"));
    }
    write_cache(&directory, &target, &values, samples);
    Ok(payload(&values, samples, bins))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `GET /api/play/tracks/{track_id}/waveform-detail`.
pub async fn get_waveform_detail(State(state): State<AppState>, UrlPath(track_id): UrlPath<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let id = params.path_int("track_id", &track_id);
    let bins = params.opt_int("bins");
    if let Some(bins) = bins {
        if !(16..=20000).contains(&bins) {
            let _ = params.bounded_int("bins", bins, 16, Some(20000));
        }
    }
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.unwrap_or_default();
    let db = state.db.clone();
    let track = match state.blocking(move || get_track(&*db.lease()?, BigInt(id))).await {
        Ok(Some(track)) => track,
        Ok(None) => return HttpError(ApiError::new(404, "Track not found")).into_response(),
        Err(response) => return response,
    };
    let bins = bins.map(|b| b as usize);
    match state.blocking_result(move || waveform_detail(&track.filepath, bins)).await {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(Failure(status, detail))) => HttpError(ApiError::new(status, detail)).into_response(),
        Err(response) => response,
    }
}
