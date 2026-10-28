//! Shared helpers: synthesized sources, WAV/AIFF writers, external encoders
//! (only when installed) and alignment metrics.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub const RATE: u32 = 44_100;
/// Source length: 3 s.
pub const LEN: usize = 3 * RATE as usize;
/// The click: a single full-scale-ish impulse at this frame.
pub const CLICK: usize = 22_050;
/// Noise starts this many frames after the click.
pub const NOISE_GAP: usize = 200;

/// The test source, stereo, already quantised to 16 bits so lossless
/// round trips are exact. Silence, a click at [`CLICK`], then shaped noise
/// with a slow sine (different per channel), silence for the last 0.25 s.
pub fn source() -> Vec<[i16; 2]> {
    let mut state: u32 = 0x1234_5678;
    let mut lp = [0.0f64; 2];
    (0..LEN)
        .map(|n| {
            let mut frame = [0i16; 2];
            if n == CLICK {
                return [26_000, 26_000];
            }
            if n >= CLICK + NOISE_GAP && n < LEN - RATE as usize / 4 {
                for (c, slot) in frame.iter_mut().enumerate() {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    let white = f64::from(state) / f64::from(u32::MAX) * 2.0 - 1.0;
                    lp[c] = 0.6 * lp[c] + 0.4 * white;
                    let tone = (n as f64 / f64::from(RATE) * std::f64::consts::TAU * (220.0 + 110.0 * c as f64)).sin();
                    *slot = ((lp[c] * 0.25 + tone * 0.2) * 32767.0) as i16;
                }
            }
            frame
        })
        .collect()
}

pub fn to_f32_mono(src: &[[i16; 2]]) -> Vec<f32> {
    src.iter().map(|f| (f32::from(f[0]) + f32::from(f[1])) / 2.0 / 32768.0).collect()
}

pub fn to_f32_interleaved(src: &[[i16; 2]]) -> Vec<f32> {
    src.iter().flat_map(|f| [f32::from(f[0]) / 32768.0, f32::from(f[1]) / 32768.0]).collect()
}

/// Writes a WAV file. `format`: 1 = PCM (bits 16/24), 3 = float (32).
pub fn write_wav(path: &Path, rate: u32, channels: u16, bits: u16, format: u16, interleaved: &[f32]) {
    let block = channels * bits / 8;
    let mut data = Vec::with_capacity(interleaved.len() * usize::from(bits / 8));
    for &s in interleaved {
        match (format, bits) {
            (3, 32) => data.extend_from_slice(&s.to_le_bytes()),
            (1, 16) => data.extend_from_slice(&((f64::from(s) * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes()),
            (1, 24) => {
                let v = (f64::from(s) * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                data.extend_from_slice(&v.to_le_bytes()[..3]);
            }
            _ => panic!("unsupported test format"),
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).unwrap();
}

pub fn write_source_wav(path: &Path) {
    write_wav(path, RATE, 2, 16, 1, &to_f32_interleaved(&source()));
}

pub fn ffmpeg() -> Option<PathBuf> {
    find_tool("ffmpeg")
}

pub fn find_tool(name: &str) -> Option<PathBuf> {
    let candidates = [format!("/opt/homebrew/bin/{name}"), format!("/usr/local/bin/{name}"), format!("/usr/bin/{name}")];
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Runs a tool; returns false (with a note) when it fails.
pub fn run(tool: &Path, args: &[&str]) -> bool {
    match Command::new(tool).args(args).output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("{} failed: {}", tool.display(), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""));
            false
        }
        Err(e) => {
            eprintln!("{} not runnable: {e}", tool.display());
            false
        }
    }
}

/// Decodes with ffmpeg (its default gapless handling) to f32 stereo at the
/// file's own rate. Black-box reference only.
pub fn ffmpeg_decode(ffmpeg: &Path, input: &Path, channels: usize) -> Option<Vec<f32>> {
    let out = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-f", "f32le", "-ac", &channels.to_string(), "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

pub fn mono_of(interleaved: &[f32], channels: usize) -> Vec<f32> {
    interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
}

/// Lag (in frames) that best aligns `test` to `reference`: `test[n + lag]
/// ~ reference[n]`, searched in `-max_lag..=max_lag` over `window` frames
/// starting at `start` in the reference.
pub fn best_lag(reference: &[f32], test: &[f32], start: usize, window: usize, max_lag: i64) -> i64 {
    let mut best = (f64::MIN, 0i64);
    for lag in -max_lag..=max_lag {
        let mut acc = 0.0f64;
        for n in start..start + window {
            let t = n as i64 + lag;
            if t < 0 || t as usize >= test.len() || n >= reference.len() {
                continue;
            }
            acc += f64::from(reference[n]) * f64::from(test[t as usize]);
        }
        if acc > best.0 {
            best = (acc, lag);
        }
    }
    best.1
}

/// Max absolute error and Pearson correlation over the common length.
pub fn compare(a: &[f32], b: &[f32]) -> (f32, f64) {
    let n = a.len().min(b.len());
    let max_err = a[..n].iter().zip(&b[..n]).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    let (ma, mb) = (mean(&a[..n]), mean(&b[..n]));
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a[..n].iter().zip(&b[..n]) {
        let (dx, dy) = (f64::from(*x) - ma, f64::from(*y) - mb);
        sab += dx * dy;
        saa += dx * dx;
        sbb += dy * dy;
    }
    (max_err, sab / (saa.sqrt() * sbb.sqrt()).max(1e-30))
}

fn mean(x: &[f32]) -> f64 {
    x.iter().map(|v| f64::from(*v)).sum::<f64>() / x.len().max(1) as f64
}

/// Index of the largest absolute sample in `range`.
pub fn peak_index(x: &[f32], range: std::ops::Range<usize>) -> usize {
    let end = range.end.min(x.len());
    (range.start..end).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap_or(range.start)
}
