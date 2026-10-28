//! Decodes a sample of real files, read-only. Ignored by default; run with
//!
//! ```sh
//! PLUMDECK_DECODE_LIST=/path/to/sample.tsv \
//! cargo test -p plumdeck-decode --test real_library -- --ignored --nocapture
//! ```
//!
//! - `PLUMDECK_DECODE_LIST` (required): one file per line, optionally
//!   followed by a tab and a reference duration in seconds (for example from
//!   a read-only query of the library DB:
//!   `SELECT filepath, duration FROM tracks`, opened with `read_only=True`).
//! - `PLUMDECK_DECODE_LIMIT`: decode at most this many files (default all).
//! - `PLUMDECK_DECODE_PROBE_ONLY=1`: probe without decoding.
//! - `PLUMDECK_DECODE_THREADS`: worker threads (default 4).
//!
//! Debug builds of symphonia are slow; for a large sample add
//! `--config 'profile.dev.package."*".opt-level=3'` (dependencies only).
//!
//! Files are only opened for reading. Only aggregates are printed: no
//! titles or paths.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use plumdeck_decode::{decode_to_pages, probe_path, Basis, DecodeError, DecodeOptions, Unsupported};

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[((v.len() - 1) as f64 * p).round() as usize])
}

fn error_kind(e: &DecodeError) -> String {
    match e {
        DecodeError::Io(io) => format!("io:{:?}", io.kind()),
        DecodeError::Unsupported(Unsupported::HeAac { ps, implicit }) => format!("unsupported:he-aac(ps={ps},implicit={implicit})"),
        DecodeError::Unsupported(Unsupported::Opus) => "unsupported:opus".into(),
        DecodeError::Unsupported(Unsupported::Container) => "unsupported:container".into(),
        DecodeError::Unsupported(Unsupported::Codec(c)) => format!("unsupported:codec:{c}"),
        DecodeError::Unsupported(Unsupported::Other(m)) => format!("unsupported:{m}"),
        DecodeError::Malformed(m) => format!("malformed:{}", m.split(':').next().unwrap_or(m)),
        DecodeError::NoAudioTrack => "no-audio-track".into(),
    }
}

fn bump(map: &mut BTreeMap<String, u32>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

/// What one file produced.
#[derive(Default)]
struct Outcome {
    ext: String,
    probe_error: Option<String>,
    codec: String,
    case: String,
    priming: String,
    lame_family: Option<String>,
    decode_error: Option<String>,
    packet_errors: u64,
    seconds: f64,
    raw_vs_hint: Option<i64>,
    canonical_vs_declared: Option<i64>,
    reference_diff_ms: Option<f64>,
}

fn run_one(line: &str, probe_only: bool) -> Outcome {
    let mut parts = line.split('\t');
    let path = Path::new(parts.next().unwrap_or_default());
    let reference: Option<f64> = parts.next().and_then(|d| d.trim().parse().ok());
    let mut o = Outcome { ext: path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase(), ..Outcome::default() };
    let info = match probe_path(path) {
        Ok(i) => i,
        Err(e) => {
            o.probe_error = Some(error_kind(&e));
            return o;
        }
    };
    o.codec = format!("{}/{}", info.container.as_str(), info.codec.as_str());
    o.case = format!("{} ({})", info.timing_case.label(), info.timing_case.as_str());
    o.priming = format!("{:?}", info.priming_source);
    if let Some(l) = info.mp3.as_ref().and_then(|m| m.lame.as_ref()) {
        let family: String = l.encoder.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        o.lame_family = Some(if family.is_empty() { "(other)".to_owned() } else { family });
    }
    if probe_only {
        return o;
    }
    let opts = DecodeOptions { basis: Basis::Canonical, max_seconds: 4.0 * 3600.0, ..DecodeOptions::default() };
    let d = match decode_to_pages(path, &opts) {
        Ok(d) => d,
        Err(e) => {
            o.decode_error = Some(error_kind(&e));
            return o;
        }
    };
    o.packet_errors = d.decode_errors;
    let rate = f64::from(d.sample_rate());
    o.seconds = d.frames() as f64 / rate;
    if let (Some(hint), true) = (info.frames_hint, info.duration_exact) {
        o.raw_vs_hint = Some(d.raw_frames as i64 - hint as i64);
    }
    o.canonical_vs_declared = info.valid_frames.map(|v| d.frames() as i64 - v as i64);
    o.reference_diff_ms = reference.filter(|r| *r > 0.0).map(|r| (o.seconds - r).abs() * 1000.0);
    o
}

#[test]
#[ignore = "needs a list of real audio files (PLUMDECK_DECODE_LIST)"]
fn real_library_sample() {
    let list = std::env::var("PLUMDECK_DECODE_LIST").expect("PLUMDECK_DECODE_LIST");
    let limit: usize = std::env::var("PLUMDECK_DECODE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let threads: usize = std::env::var("PLUMDECK_DECODE_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(4).max(1);
    let probe_only = std::env::var("PLUMDECK_DECODE_PROBE_ONLY").is_ok_and(|v| v == "1");
    let text = std::fs::read_to_string(&list).expect("read list");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(limit).collect();

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(lines.len()));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(line) = lines.get(i) else { break };
                let o = run_one(line, probe_only);
                results.lock().unwrap().push(o);
            });
        }
    });
    let elapsed = started.elapsed().as_secs_f64();
    let results = results.into_inner().unwrap();

    let mut by_ext: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_codec: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_case: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_priming: BTreeMap<String, u32> = BTreeMap::new();
    let mut lame_encoders: BTreeMap<String, u32> = BTreeMap::new();
    let mut failures: BTreeMap<String, u32> = BTreeMap::new();
    let mut failures_by_ext: BTreeMap<String, u32> = BTreeMap::new();
    let mut mismatch: BTreeMap<String, u32> = BTreeMap::new();
    let mut dur_diff_ms = Vec::new();
    let mut raw_vs_hint = Vec::new();
    let (mut probed, mut decoded, mut packet_error_files) = (0u32, 0u32, 0u32);
    let mut audio_seconds = 0.0f64;
    for o in &results {
        bump(&mut by_ext, o.ext.clone());
        if let Some(e) = &o.probe_error {
            bump(&mut failures, format!("probe:{e}"));
            bump(&mut failures_by_ext, o.ext.clone());
            continue;
        }
        probed += 1;
        bump(&mut by_codec, o.codec.clone());
        bump(&mut by_case, o.case.clone());
        bump(&mut by_priming, o.priming.clone());
        if let Some(f) = &o.lame_family {
            bump(&mut lame_encoders, f.clone());
        }
        if probe_only {
            continue;
        }
        if let Some(e) = &o.decode_error {
            bump(&mut failures, format!("decode:{e} [{}]", o.codec));
            bump(&mut failures_by_ext, o.ext.clone());
            continue;
        }
        decoded += 1;
        audio_seconds += o.seconds;
        if o.packet_errors > 0 {
            packet_error_files += 1;
        }
        if let Some(diff) = o.raw_vs_hint {
            raw_vs_hint.push(diff.unsigned_abs() as f64);
            if diff != 0 {
                bump(&mut mismatch, format!("decoder_output!=header:{}", o.case));
            }
        }
        if o.canonical_vs_declared.is_some_and(|d| d != 0) {
            bump(&mut mismatch, format!("canonical!=declared:{}", o.case));
        }
        if let Some(ms) = o.reference_diff_ms {
            dur_diff_ms.push(ms);
        }
    }

    let files = results.len();
    println!("files={files} probed={probed} decoded={decoded} files_with_packet_errors={packet_error_files}");
    println!("decoded audio {audio_seconds:.0} s in {elapsed:.1} s wall with {threads} threads");
    println!("by extension: {by_ext:?}");
    println!("by container/codec: {by_codec:?}");
    println!("by timing case: {by_case:?}");
    println!("by priming source: {by_priming:?}");
    println!("LAME-tag encoder families: {lame_encoders:?}");
    println!("failures: {failures:?}");
    println!("failures by extension: {failures_by_ext:?}");
    println!("length mismatches: {mismatch:?}");
    let n = dur_diff_ms.len();
    println!(
        "|canonical duration - reference| ms over {n}: median {:?} p95 {:?} max {:?}",
        median(&mut dur_diff_ms.clone()),
        percentile(&mut dur_diff_ms.clone(), 0.95),
        percentile(&mut dur_diff_ms, 1.0)
    );
    println!(
        "|decoder_output frames - header frames| over {}: median {:?} max {:?}",
        raw_vs_hint.len(),
        median(&mut raw_vs_hint.clone()),
        percentile(&mut raw_vs_hint, 1.0)
    );
    assert!(files >= 1);
}
