//! Analysis services (S11): the worker subprocess (protocol V1), the one
//! analysis slot shared by every entry point (analysis_coordinator.py), the
//! "manual > rekordbox > own" value rule (value_sources.py), saving results
//! (ingestion_repository.py, analysis_job_service._save) and the durable job
//! runner (analysis_job_service.py).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::time::Clock;
use plumdeck_db::array_codec::{pack_f32, pack_u8_waveform};
use plumdeck_db::duckdb::types::{TimeUnit, Value as Db};
use plumdeck_db::duckdb::{params_from_iter, Connection, OptionalExt};
use plumdeck_db::handle::DbHandle;
use plumdeck_db::jobs_sqlite::{AnalysisJobQueue, QueueItem};
use plumdeck_db::DbError;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const SUPPORTED_EXTENSIONS: [&str; 7] = [".mp3", ".m4a", ".flac", ".wav", ".ogg", ".aac", ".aiff"];
pub const EMBEDDING_MODEL: &str = "msd-musicnn-1:musicnn-16khz-v1";
pub const WORKER_TIMEOUT: Duration = Duration::from_secs(570);
const TIMBRE_FIELDS: [&str; 4] = ["energy", "danceability", "brightness", "noisiness"];
const PERSISTED_KEYS: [&str; 9] = ["analysis_components", "playback_grid", "playback_grid_version", "analysis_bpm", "analysis_key", "bpm_confidence", "key_strength", "bpm_raw", "audio_sha256"];

/// Component versions counted as current: this analyzer's, and Python's
/// (rows analysed before the switch are not re-analysed).
/// Component versions that count as current (older analyzers whose output
/// is still compatible included).
pub fn current_versions(feature: &str) -> &'static [&'static str] {
    match feature {
        "rhythm" => &["rust-attacks-v1", "rhythm-attacks-v4", "numpy-attacks-v2", "librosa-attacks-v2"],
        "key" => &["rust-chroma-v1", "key-v1", "librosa-key-v1"],
        "timbre" => &["rust-timbre-v1", "timbre-v1", "librosa-timbre-v1"],
        "waveform" => &["waveform-v1"],
        "embedding" => &["musicnn-16khz-v1"],
        _ => &[],
    }
}

fn current_version(feature: &str, version: Option<&str>) -> bool {
    version.is_some_and(|version| current_versions(feature).contains(&version))
}

pub fn ts(at: NaiveDateTime) -> Db {
    Db::Timestamp(TimeUnit::Microsecond, at.and_utc().timestamp_micros())
}

pub fn py_json(value: &Value) -> String {
    let parsed = plumdeck_core::compat::pyjson_loads::loads(&value.to_string()).unwrap_or(PyValue::None);
    dumps(&parsed, &DumpOptions::default()).unwrap_or_default()
}

pub fn sha256_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest).ok()?;
    Some(digest.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ------------------------------------------------------------------ worker

fn worker_exe() -> PathBuf {
    std::env::var_os("PLUMDECK_ANALYSIS_WORKER_EXE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).unwrap_or_else(|| PathBuf::from("plumdeck-backend"))
}

/// Run one operation in a fresh `--analysis-worker` process with a hard
/// deadline; progress events go to `on_progress`.
pub fn run_worker(operation: &str, args: Vec<Value>, timeout: Duration, cancel: Option<&AtomicBool>, on_progress: &mut dyn FnMut(Value)) -> Result<Value, String> {
    let mut child = Command::new(worker_exe())
        .arg("--analysis-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Analysis worker could not start: {error}"))?;
    let request = json!({"version": 1, "operation": operation, "args": args});
    {
        let mut stdin = child.stdin.take().ok_or("Analysis worker closed its request pipe during startup")?;
        writeln!(stdin, "{request}").map_err(|_| "Analysis worker closed its request pipe during startup".to_owned())?;
    }
    let stdout = child.stdout.take().expect("piped");
    let (sender, receiver) = std::sync::mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(Some(line)).is_err() {
                return;
            }
        }
        let _ = sender.send(None);
    });
    let deadline = Instant::now() + timeout;
    let outcome = loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            break Err("Analysis cancelled".to_owned());
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(format!("Analysis timed out after {} seconds", timeout.as_secs()));
        }
        match receiver.recv_timeout((deadline - now).min(Duration::from_millis(100))) {
            Ok(Some(line)) => {
                let Some(payload) = line.trim_end().strip_prefix(plumdeck_analysis::PROTOCOL_PREFIX) else { continue };
                let Ok(message) = serde_json::from_str::<Value>(payload) else { break Err("Analysis worker returned an invalid protocol message".into()) };
                match message["type"].as_str() {
                    Some("progress") => on_progress(message["event"].clone()),
                    Some("result") if message["ok"].as_bool() == Some(true) => break Ok(message["value"].clone()),
                    Some("result") => {
                        let error = &message["error"];
                        break Err(format!("{}: {}", error["type"].as_str().unwrap_or("RuntimeError"), error["message"].as_str().unwrap_or("Analysis failed")));
                    }
                    _ => break Err("Analysis worker returned an unknown protocol message".into()),
                }
            }
            Ok(None) => break Err(format!("Analysis worker exited (code {:?}) without a result", child.try_wait().ok().flatten().and_then(|s| s.code()))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Err("Analysis worker exited without a result".into()),
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

// ------------------------------------------------------------------ coordinator

/// One native analysis at a time; foreground requests outrank background.
#[derive(Default)]
pub struct Coordinator {
    state: Mutex<CoordinatorState>,
    changed: Condvar,
}

#[derive(Default)]
struct CoordinatorState {
    holder: Option<(String, String, bool, Arc<AtomicBool>)>,
    foreground_waiting: usize,
}

impl Coordinator {
    pub fn acquire(&self, owner: &str, background: bool) -> Option<String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if background && state.foreground_waiting > 0 {
            return None;
        }
        if let Some((_, _, held_background, preempt)) = &state.holder {
            if *held_background && !background {
                preempt.store(true, Ordering::SeqCst);
            }
            return None;
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        state.holder = Some((token.clone(), owner.to_owned(), background, Arc::new(AtomicBool::new(false))));
        Some(token)
    }

    /// Block until the slot is free (foreground).
    pub fn acquire_wait(&self, owner: &str) -> String {
        {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).foreground_waiting += 1;
        }
        let token = loop {
            if let Some(token) = self.acquire(owner, false) {
                break token;
            }
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let _ = self.changed.wait_timeout(state, Duration::from_millis(250));
        };
        self.state.lock().unwrap_or_else(|e| e.into_inner()).foreground_waiting -= 1;
        token
    }

    pub fn preemption(&self, token: &str) -> Arc<AtomicBool> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match &state.holder {
            Some((held, _, _, flag)) if held == token => flag.clone(),
            _ => Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn release(&self, token: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.holder.as_ref().is_some_and(|(held, ..)| held == token) {
            state.holder = None;
        }
        self.changed.notify_all();
    }

    pub fn owner(&self) -> Option<String> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).holder.as_ref().map(|(_, owner, ..)| owner.clone())
    }
}

// ------------------------------------------------------------------ value sources

const CAMELOT_TO_KEY: [(&str, &str); 24] = [
    ("1A", "Ab minor"), ("1B", "B major"), ("2A", "Eb minor"), ("2B", "F# major"), ("3A", "Bb minor"), ("3B", "C# major"),
    ("4A", "F minor"), ("4B", "Ab major"), ("5A", "C minor"), ("5B", "Eb major"), ("6A", "G minor"), ("6B", "Bb major"),
    ("7A", "D minor"), ("7B", "F major"), ("8A", "A minor"), ("8B", "C major"), ("9A", "E minor"), ("9B", "G major"),
    ("10A", "B minor"), ("10B", "D major"), ("11A", "F# minor"), ("11B", "A major"), ("12A", "C# minor"), ("12B", "E major"),
];

/// `key_name`: any accepted spelling as stored ('F minor').
pub fn key_name(key: Option<&str>) -> Option<String> {
    let code = plumdeck_key::normalize_key(key)?;
    CAMELOT_TO_KEY.iter().find(|(c, _)| *c == code).map(|(_, name)| (*name).to_owned())
}

#[derive(Clone, Debug, Default)]
pub struct RekordboxValues {
    pub readable: bool,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub keys: bool,
    pub analysis_path: Option<String>,
}

impl RekordboxValues {
    fn value(&self, field: &str) -> Option<Value> {
        if field == "bpm" {
            self.bpm.map(|b| json!(b))
        } else {
            self.key.clone().map(Value::String)
        }
    }

    fn enabled(&self, field: &str) -> bool {
        field == "bpm" || self.keys
    }
}

/// `value_sources.lookup`: rekordbox's BPM (and key) for a file; never fails.
pub fn lookup(filepath: &str, keys: bool) -> RekordboxValues {
    let Some(path) = plumdeck_rekordbox::locate::default_master_db_path().filter(|p| p.is_file()) else { return RekordboxValues { keys, ..Default::default() } };
    let Ok(db) = plumdeck_rekordbox::MasterDb::open(&path) else { return RekordboxValues { keys, ..Default::default() } };
    let Ok(found) = db.contents_by_paths(&[filepath]) else { return RekordboxValues { keys, ..Default::default() } };
    let Some(entry) = found.into_values().next() else { return RekordboxValues { readable: true, keys, ..Default::default() } };
    let bpm = entry.bpm_x100.map(|b| b as f64 / 100.0).filter(|b| b.is_finite() && *b > 0.0);
    RekordboxValues { readable: true, bpm, key: if keys { key_name(entry.key_name.as_deref()) } else { None }, keys, analysis_path: entry.analysis_data_path.clone() }
}

/// `key_source_enabled`: the `rekordbox_key_source` setting (default off).
pub fn key_source_enabled(conn: &Connection) -> bool {
    let value: Option<Option<String>> = conn.query_row("SELECT value FROM settings WHERE key='rekordbox_key_source'", [], |r| r.get(0)).optional().ok().flatten();
    matches!(value.flatten().map(|v| v.trim().to_lowercase()).as_deref(), Some("1" | "true" | "on" | "yes"))
}

fn valid(field: &str, value: &Value) -> bool {
    if field == "bpm" {
        value.as_f64().is_some_and(|b| b.is_finite() && b > 0.0) && !value.is_boolean()
    } else {
        value.as_str().is_some_and(|k| !k.trim().is_empty() && k.trim().to_lowercase() != "unknown")
    }
}

/// `stored_sources`.
fn stored_sources(extra: &Value) -> Map<String, Value> {
    let mut sources: Map<String, Value> = extra["analysis_components"]["sources"]
        .as_object()
        .map(|m| m.iter().filter(|(k, v)| ["bpm", "key"].contains(&k.as_str()) && v.is_string()).map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    if extra.get("bpm_authority").is_some_and(|v| !v.is_null() && v != false) && !sources.contains_key("bpm") {
        sources.insert("bpm".into(), json!("rekordbox"));
    }
    sources
}

/// `choose`: (value, source) for one field.
fn choose(field: &str, incoming: &Value, incoming_source: Option<&str>, stored: &Value, stored_source: Option<&str>, rekordbox: &RekordboxValues, saved_own: &Value) -> Option<(Value, String)> {
    if stored_source == Some("manual") && valid(field, stored) {
        return Some((stored.clone(), "manual".into()));
    }
    if !rekordbox.enabled(field) && stored_source == Some("rekordbox") {
        if valid(field, incoming) && incoming_source != Some("rekordbox") {
            return Some((incoming.clone(), incoming_source.unwrap_or("own").into()));
        }
        if valid(field, saved_own) {
            return Some((saved_own.clone(), "own".into()));
        }
    }
    if let Some(external) = rekordbox.value(field) {
        return Some((external, "rekordbox".into()));
    }
    if incoming_source == Some("rekordbox") && valid(field, incoming) {
        return Some((incoming.clone(), "rekordbox".into()));
    }
    if stored_source == Some("rekordbox") && valid(field, stored) {
        return Some((stored.clone(), "rekordbox".into()));
    }
    if valid(field, incoming) {
        return Some((incoming.clone(), incoming_source.unwrap_or("own").into()));
    }
    if valid(field, stored) {
        return Some((stored.clone(), stored_source.unwrap_or("own").into()));
    }
    None
}

fn scale_of(key: &str) -> String {
    let parts: Vec<&str> = key.split_whitespace().collect();
    if parts.len() == 2 {
        parts[1].to_lowercase()
    } else {
        String::new()
    }
}

fn with_sources(extra: &mut Map<String, Value>, sources: &Map<String, Value>) {
    let mut merged = stored_sources(&Value::Object(extra.clone()));
    merged.extend(sources.clone());
    let mut components = extra.get("analysis_components").and_then(Value::as_object).cloned().unwrap_or_default();
    if !merged.is_empty() {
        components.insert("sources".into(), Value::Object(merged));
    }
    extra.insert("analysis_components".into(), Value::Object(components));
    if sources.contains_key("bpm") {
        extra.remove("bpm_authority");
    }
}

fn persisted(extra: &Value) -> Map<String, Value> {
    PERSISTED_KEYS.iter().filter_map(|k| extra.get(*k).map(|v| ((*k).to_owned(), v.clone()))).collect()
}

// ------------------------------------------------------------------ completeness

fn unit(value: &Value) -> bool {
    value.as_f64().is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)) && !value.is_boolean()
}

/// `has_measured_timbre`.
pub fn measured_timbre(values: &Value, extra: &Value) -> bool {
    let fields: Vec<&Value> = TIMBRE_FIELDS.iter().map(|f| &values[*f]).collect();
    fields.iter().all(|v| unit(v)) && (fields.iter().filter(|v| v.as_f64() == Some(0.0)).count() < 3 || !extra["analysis_components"]["timbre"].is_null())
}

/// `has_completed_analysis_result`.
pub fn completed_result(result: &Value) -> bool {
    let positive = |v: &Value| v.as_f64().is_some_and(|x| x.is_finite() && x > 0.0) && !v.is_boolean();
    let peaks_ok = result["features_extra"]["waveform_peaks"].as_array().is_some_and(|p| !p.is_empty() && p.iter().all(|x| x.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0)));
    positive(&result["bpm"]) && positive(&result["duration"]) && measured_timbre(result, &result["features_extra"]) && peaks_ok
}

/// SQL of the "analyzed" rule (completed_analysis_condition, minus the
/// light-profile branch which this analyzer never writes).
pub const COMPLETED_SQL: &str = "(coalesce(tracks.bpm > 0 AND isfinite(tracks.bpm) AND tracks.duration > 0 AND isfinite(tracks.duration) \
    AND tracks.energy BETWEEN 0 AND 1 AND tracks.danceability BETWEEN 0 AND 1 AND tracks.brightness BETWEEN 0 AND 1 AND tracks.noisiness BETWEEN 0 AND 1 \
    AND EXISTS (SELECT 1 FROM track_analyses a WHERE a.track_id = tracks.id AND octet_length(a.waveform_u8) > 0), false))";

// ------------------------------------------------------------------ saving

fn number_or_null(value: &Value) -> Db {
    value.as_f64().filter(|x| x.is_finite()).map_or(Db::Null, |x| Db::Float(x as f32))
}

fn text_value(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn refresh_saved_grid(conn: &Connection, track_id: i64, grid: &Value, now: NaiveDateTime) -> Result<(), DbError> {
    if grid.is_null() || grid["source"] != "analysis" {
        return Ok(());
    }
    let stored: Option<Option<String>> = conn.query_row("SELECT beat_grid_json FROM track_performance_metadata WHERE track_id=?", [track_id], |r| r.get(0)).optional()?;
    let Some(Some(old)) = stored else { return Ok(()) };
    let Ok(old) = serde_json::from_str::<Value>(&old) else { return Ok(()) };
    if old["source"] != "analysis" {
        return Ok(());
    }
    let parsed = plumdeck_core::compat::pyjson_loads::loads(&grid.to_string()).unwrap_or(PyValue::None);
    let Ok(beat_grid) = plumdeck_domain::performance::BeatGrid::parse(&parsed) else { return Ok(()) };
    let serialized = dumps(&beat_grid.json(), &DumpOptions::starlette()).unwrap_or_default();
    if serde_json::from_str::<Value>(&serialized).ok().as_ref() != Some(&old) {
        conn.execute(
            "UPDATE track_performance_metadata SET beat_grid_json=?, revision=revision+1, updated_at=? WHERE track_id=?",
            params_from_iter([Db::Text(serialized), ts(now), Db::BigInt(track_id)]),
        )?;
    }
    Ok(())
}

/// `IngestionRepository.save_track`: insert or update the track row, its
/// analysis row, embedding and lyrics from one analysis (or metadata) result.
pub fn save_track(conn: &Connection, result: &Value, now: NaiveDateTime) -> Result<(i64, bool), DbError> {
    let filepath = result["filepath"].as_str().unwrap_or_default().to_owned();
    let extras = result.get("features_extra").cloned().unwrap_or(json!({}));
    let is_analysis = extras.get("analysis_components").is_some();
    let audio_hash = if is_analysis { sha256_file(Path::new(&filepath)) } else { None };
    let existing: Option<(i64, Option<f64>, Option<f64>, Option<String>)> = conn
        .query_row("SELECT id, bpm, duration, key FROM tracks WHERE filepath=? LIMIT 1", [&filepath], |r| Ok((r.get(0)?, r.get::<_, Option<f32>>(1)?.map(f64::from), r.get::<_, Option<f32>>(2)?.map(f64::from), r.get(3)?)))
        .optional()?;
    let existing_fx: Value = match &existing {
        Some((id, ..)) => conn
            .query_row("SELECT features_extra_json FROM track_analyses WHERE track_id=?", [*id], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(json!({})),
        None => json!({}),
    };
    let duration_changed = match (&existing, result["duration"].as_f64()) {
        (Some((_, _, Some(before), _)), Some(after)) if *before > 0.0 && after > 0.0 => (before - after).abs() > 1.0,
        _ => false,
    };
    let audio_changed = match &existing {
        None => false,
        Some((id, ..)) if is_analysis => {
            if duration_changed || audio_hash.is_none() {
                true
            } else if existing_fx["audio_sha256"].as_str() == audio_hash.as_deref() {
                false
            } else {
                let media: Option<Option<String>> = conn.query_row("SELECT sha256 FROM track_media WHERE track_id=?", [*id], |r| r.get(0)).optional()?;
                media.flatten().as_deref() != audio_hash.as_deref()
            }
        }
        Some(_) => false,
    };
    let mut settled = result.clone();
    let mut sources = Map::new();
    let mut own = Map::new();
    if is_analysis {
        let incoming_sources = extras["analysis_components"]["sources"].as_object().cloned().unwrap_or_default();
        let stored = stored_sources(&existing_fx);
        let rekordbox = lookup(&filepath, key_source_enabled(conn));
        for field in ["bpm", "key"] {
            if result.get(field).is_none() {
                continue;
            }
            let incoming_source = incoming_sources.get(field).and_then(Value::as_str).unwrap_or("own").to_owned();
            let stored_value = match (&existing, field) {
                (Some((_, bpm, ..)), "bpm") => bpm.map_or(Value::Null, |b| json!(b)),
                (Some((_, _, _, key)), _) => key.clone().map_or(Value::Null, Value::String),
                _ => Value::Null,
            };
            let Some((value, source)) = choose(field, &result[field], Some(&incoming_source), &stored_value, stored.get(field).and_then(Value::as_str), &rekordbox, &existing_fx[format!("analysis_{field}")]) else { continue };
            if field == "key" {
                let scale = scale_of(value.as_str().unwrap_or(""));
                settled["scale"] = json!(if scale.is_empty() { result["scale"].as_str().unwrap_or("").to_owned() } else { scale });
            }
            settled[field] = value.clone();
            sources.insert(field.into(), json!(source));
            if source == "own" {
                continue;
            }
            if incoming_source == "own" && valid(field, &result[field]) {
                own.insert(field.into(), result[field].clone());
            }
        }
    }
    let s = |key: &str| settled[key].as_str().map(str::to_owned);
    let track_id = match &existing {
        Some((id, ..)) => {
            let mut sets: Vec<String> = Vec::new();
            let mut params: Vec<Db> = Vec::new();
            for key in ["title", "artist", "album", "genre", "key", "scale"] {
                if let Some(value) = s(key).filter(|v| !v.is_empty() && v.to_lowercase() != "unknown") {
                    if settled.get(key).is_some() {
                        sets.push(format!("{key}=?"));
                        params.push(Db::Text(value));
                    }
                }
            }
            if let Some(year) = settled["year"].as_i64().filter(|y| *y > 0) {
                sets.push("year=?".into());
                params.push(Db::BigInt(year));
            }
            if let Some(level) = s("analysis_level").filter(|l| l == "light" || l == "full") {
                sets.push("analysis_level=?".into());
                params.push(Db::Text(level));
            }
            for key in ["bpm", "duration", "energy", "danceability", "brightness", "contrast", "noisiness", "loudness", "loudness_range", "spectral_flux", "spectral_rolloff"] {
                if let Some(value) = settled.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && !settled[key].is_boolean()) {
                    if ["bpm", "duration"].contains(&key) && value <= 0.0 {
                        continue;
                    }
                    sets.push(format!("{key}=?"));
                    params.push(Db::Float(value as f32));
                }
            }
            if !sets.is_empty() {
                params.push(Db::BigInt(*id));
                conn.execute(&format!("UPDATE tracks SET {} WHERE id=?", sets.join(", ")), params_from_iter(params))?;
            }
            *id
        }
        None => {
            let text_or = |key: &str, fallback: &str| Db::Text(s(key).filter(|v| !v.is_empty()).unwrap_or_else(|| fallback.to_owned()));
            let bpm = settled["bpm"].as_f64().filter(|b| *b > 0.0).map_or(Db::Null, |b| Db::Float(b as f32));
            let values = vec![
                Db::Text(filepath.clone()),
                text_or("title", "Unknown"),
                text_or("artist", "Unknown"),
                text_or("album", ""),
                text_or("genre", ""),
                settled["year"].as_i64().map_or(Db::Null, Db::BigInt),
                bpm,
                text_or("key", ""),
                text_or("scale", ""),
                number_or_null(&settled["duration"]),
                text_value(&settled["analysis_level"]).map_or(Db::Null, Db::Text),
                Db::Float(settled["energy"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["danceability"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["brightness"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["contrast"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["noisiness"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["loudness"].as_f64().unwrap_or(-60.0) as f32),
                Db::Float(settled["loudness_range"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["spectral_flux"].as_f64().unwrap_or(0.0) as f32),
                Db::Float(settled["spectral_rolloff"].as_f64().unwrap_or(0.0) as f32),
                Db::Boolean(false),
                ts(now),
            ];
            conn.query_row(
                "INSERT INTO tracks (filepath,title,artist,album,genre,year,bpm,key,scale,duration,analysis_level,energy,danceability,brightness,contrast,\
                 noisiness,loudness,loudness_range,spectral_flux,spectral_rolloff,is_genre_verified,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) RETURNING id",
                params_from_iter(values),
                |r| r.get::<_, i64>(0),
            )?
        }
    };
    let analysis_exists = conn.query_row("SELECT 1 FROM track_analyses WHERE track_id=?", [track_id], |_| Ok(())).optional()?.is_some();
    if !analysis_exists {
        conn.execute("INSERT INTO track_analyses (track_id, features_extra_json) VALUES (?, '{}')", [track_id])?;
    }
    if extras.as_object().is_some_and(|m| !m.is_empty()) {
        let mut fx = persisted(&extras);
        let mut recorded = stored_sources(&existing_fx);
        recorded.extend(sources.clone());
        if !recorded.is_empty() {
            with_sources(&mut fx, &recorded);
        }
        if existing.is_some() && !audio_changed {
            let stored_components = existing_fx["analysis_components"].as_object().cloned();
            if let (Some(stored_components), Some(components)) = (stored_components, fx.get("analysis_components").and_then(Value::as_object).cloned()) {
                let mut kept = components.clone();
                let mut carried = vec!["embedding"];
                if extras.get("beat_positions").is_none() {
                    carried.push("rhythm");
                }
                for name in &carried {
                    if !kept.contains_key(*name) && stored_components.get(*name).is_some_and(|v| !v.is_null()) {
                        kept.insert((*name).into(), stored_components[*name].clone());
                    }
                }
                if carried.contains(&"rhythm") && kept.get("rhythm") == stored_components.get("rhythm") {
                    for key in ["playback_grid", "playback_grid_version"] {
                        if let Some(value) = existing_fx.get(key) {
                            fx.entry(key).or_insert(value.clone());
                        }
                    }
                }
                fx.insert("analysis_components".into(), Value::Object(kept));
            }
        }
        if let Some(hash) = &audio_hash {
            fx.insert("audio_sha256".into(), json!(hash));
        }
        for field in ["bpm", "key"] {
            let kept = format!("analysis_{field}");
            if let Some(value) = own.get(field) {
                fx.insert(kept, value.clone());
            } else if recorded.get(field).and_then(Value::as_str).is_some_and(|s| s != "own") {
                if let Some(value) = existing_fx.get(&kept) {
                    fx.insert(kept, value.clone());
                }
            }
        }
        conn.execute("UPDATE track_analyses SET features_extra_json=? WHERE track_id=?", params_from_iter([Db::Text(py_json(&Value::Object(fx))), Db::BigInt(track_id)]))?;
    }
    if let Some(peaks) = extras["waveform_peaks"].as_array().filter(|p| !p.is_empty()) {
        let values: Vec<f64> = peaks.iter().filter_map(Value::as_f64).collect();
        if let Ok(Some(blob)) = pack_u8_waveform(Some(&values), values.len()) {
            conn.execute("UPDATE track_analyses SET waveform_u8=? WHERE track_id=?", params_from_iter([Db::Blob(blob), Db::BigInt(track_id)]))?;
        }
    }
    let beats: Option<Vec<f64>> = extras["beat_positions"].as_array().filter(|b| !b.is_empty()).map(|b| b.iter().filter_map(Value::as_f64).collect());
    if let Some(beats) = &beats {
        if let Some(blob) = pack_f32(Some(beats)) {
            conn.execute("UPDATE track_analyses SET beats_f32=? WHERE track_id=?", params_from_iter([Db::Blob(blob), Db::BigInt(track_id)]))?;
        }
    } else if existing.is_some() && audio_changed {
        conn.execute("UPDATE track_analyses SET beats_f32=NULL WHERE track_id=?", [track_id])?;
    }
    if (existing.is_some() && audio_changed) || beats.is_some() {
        conn.execute("DELETE FROM track_grid_candidates WHERE track_id=? AND source='analysis'", [track_id])?;
        refresh_saved_grid(conn, track_id, &extras["playback_grid"], now)?;
    }
    if existing.is_some() && audio_changed && result.get("embedding").is_none() && is_analysis {
        conn.execute("DELETE FROM track_embeddings WHERE track_id=?", [track_id])?;
    }
    if let Some(lyrics) = result["lyrics"].as_str().filter(|l| !l.trim().is_empty()) {
        conn.execute(
            "INSERT INTO lyrics (track_id, content, updated_at) VALUES (?, ?, ?) ON CONFLICT (track_id) DO UPDATE SET content=excluded.content, updated_at=excluded.updated_at",
            params_from_iter([Db::BigInt(track_id), Db::Text(lyrics.to_owned()), ts(now)]),
        )?;
    }
    Ok((track_id, existing.is_some()))
}

/// `AnalysisJobService._save`: one component result for an existing track.
fn save_components(conn: &Connection, item: &QueueItem, result: &Value, features: &[String], now: NaiveDateTime) -> Result<(), String> {
    fn db(e: impl std::fmt::Display) -> String {
        e.to_string()
    }
    let row: Option<(String, Option<f32>, Option<String>)> =
        conn.query_row("SELECT filepath, bpm, key FROM tracks WHERE id=?", [item.track_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(db)?;
    let Some((filepath, bpm, key)) = row.filter(|(p, ..)| Some(p) == item.filepath.as_ref()) else { return Err("Track removed or path changed during analysis".into()) };
    let extra_text: Option<Option<String>> = conn.query_row("SELECT features_extra_json FROM track_analyses WHERE track_id=?", [item.track_id], |r| r.get(0)).optional().map_err(db)?;
    if extra_text.is_none() {
        conn.execute("INSERT INTO track_analyses (track_id, features_extra_json) VALUES (?, '{}')", [item.track_id]).map_err(db)?;
    }
    let mut extra: Map<String, Value> = extra_text.flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut update: Map<String, Value> = result["features_extra"].as_object().cloned().unwrap_or_default();
    let stored = stored_sources(&Value::Object(extra.clone()));
    let mut rekordbox: Option<RekordboxValues> = None;
    let mut sources = Map::new();
    let mut sets: Vec<(String, Db)> = Vec::new();
    for feature in features {
        let names: &[&str] = match feature.as_str() {
            "rhythm" => &["bpm"],
            "key" => &["key"],
            "timbre" => &TIMBRE_FIELDS,
            _ => &[],
        };
        for name in names {
            let value = &result[*name];
            if value.as_str().is_none() && !value.as_f64().is_some_and(f64::is_finite) {
                return Err(format!("Invalid {name} result"));
            }
            if !["bpm", "key"].contains(name) {
                sets.push(((*name).into(), Db::Float(value.as_f64().unwrap_or(0.0) as f32)));
                continue;
            }
            let rb = rekordbox.get_or_insert_with(|| lookup(&filepath, key_source_enabled(conn)));
            let stored_value = if *name == "bpm" { bpm.map_or(Value::Null, |b| json!(f64::from(b))) } else { key.clone().map_or(Value::Null, Value::String) };
            let Some((chosen, source)) = choose(name, value, Some("own"), &stored_value, stored.get(*name).and_then(Value::as_str), rb, &extra.get(&format!("analysis_{name}")).cloned().unwrap_or(Value::Null)) else { continue };
            sources.insert((*name).into(), json!(source));
            if *name == "bpm" {
                sets.push(("bpm".into(), Db::Float(chosen.as_f64().unwrap_or(0.0) as f32)));
            } else {
                let text = chosen.as_str().unwrap_or("").to_owned();
                let scale = scale_of(&text);
                sets.push(("key".into(), Db::Text(text)));
                sets.push(("scale".into(), Db::Text(if scale.is_empty() { result["scale"].as_str().unwrap_or("").into() } else { scale })));
            }
            if source == "own" {
                extra.remove(&format!("analysis_{name}"));
            } else {
                update.insert(format!("analysis_{name}"), value.clone());
            }
        }
    }
    if !sets.is_empty() {
        let assignments: Vec<String> = sets.iter().map(|(k, _)| format!("{k}=?")).collect();
        let mut params: Vec<Db> = sets.into_iter().map(|(_, v)| v).collect();
        params.push(Db::BigInt(item.track_id));
        conn.execute(&format!("UPDATE tracks SET {} WHERE id=?", assignments.join(", ")), params_from_iter(params)).map_err(db)?;
    }
    let mut components = extra.get("analysis_components").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(Value::Object(new)) = update.remove("analysis_components") {
        components.extend(new);
    }
    if features.iter().any(|f| f == "rhythm") {
        let beats: Vec<f64> = update.remove("beat_positions").and_then(|b| b.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect())).unwrap_or_default();
        conn.execute("UPDATE track_analyses SET beats_f32=? WHERE track_id=?", params_from_iter([pack_f32(Some(&beats)).map_or(Db::Null, Db::Blob), Db::BigInt(item.track_id)])).map_err(db)?;
        conn.execute("DELETE FROM track_grid_candidates WHERE track_id=? AND source='analysis'", [item.track_id]).map_err(db)?;
        refresh_saved_grid(conn, item.track_id, update.get("playback_grid").unwrap_or(&Value::Null), now).map_err(db)?;
    }
    if features.iter().any(|f| f == "waveform") {
        let peaks: Vec<f64> = update.remove("waveform_peaks").and_then(|b| b.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect())).unwrap_or_default();
        if let Ok(Some(blob)) = pack_u8_waveform(Some(&peaks), peaks.len()) {
            conn.execute("UPDATE track_analyses SET waveform_u8=? WHERE track_id=?", params_from_iter([Db::Blob(blob), Db::BigInt(item.track_id)])).map_err(db)?;
        }
    }
    extra.extend(persisted(&Value::Object(update)));
    extra.insert("analysis_components".into(), Value::Object(components));
    if !sources.is_empty() {
        with_sources(&mut extra, &sources);
    }
    conn.execute("UPDATE track_analyses SET features_extra_json=? WHERE track_id=?", params_from_iter([Db::Text(py_json(&Value::Object(extra))), Db::BigInt(item.track_id)])).map_err(db)?;
    if features.iter().any(|f| f == "embedding") {
        let vector: Vec<f64> = result["embedding"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        if vector.len() != 200 || vector.iter().any(|x| !x.is_finite()) || vector.iter().all(|x| *x == 0.0) {
            return Err("Expected a finite, nonzero 200-dimensional embedding".into());
        }
        conn.execute(
            "INSERT INTO track_embeddings (track_id, embedding_json, model_name, updated_at) VALUES (?,?,?,?) ON CONFLICT (track_id) DO UPDATE SET \
             embedding_json=excluded.embedding_json, model_name=excluded.model_name, updated_at=excluded.updated_at",
            params_from_iter([Db::BigInt(item.track_id), Db::Text(py_json(&json!(vector))), Db::Text(result["embedding_model"].as_str().unwrap_or(EMBEDDING_MODEL).into()), ts(now)]),
        )
        .map_err(db)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ jobs

/// `AnalysisJobService`.
pub struct Jobs {
    db: DbHandle,
    clock: Arc<dyn Clock>,
    pub queue: AnalysisJobQueue,
    pub coordinator: Arc<Coordinator>,
    running: Mutex<Option<RunningJob>>,
    pub ingest_running: Arc<AtomicBool>,
}

struct RunningJob {
    id: String,
    background: bool,
    stop: Arc<AtomicBool>,
}

const COMPONENTS: [&str; 5] = ["embedding", "rhythm", "key", "timbre", "waveform"];

fn validate_features(features: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for feature in features.unwrap_or_else(|| vec!["embedding".into()]) {
        if !out.contains(&feature) {
            out.push(feature);
        }
    }
    if out.is_empty() || out.iter().any(|f| !COMPONENTS.contains(&f.as_str())) {
        return Err("features must contain embedding, rhythm, key, timbre or waveform".into());
    }
    Ok(out)
}

fn embedding_valid(model: Option<&str>, vector: Option<&str>) -> bool {
    model == Some(EMBEDDING_MODEL)
        && vector.and_then(|v| serde_json::from_str::<Vec<f64>>(v).ok()).is_some_and(|v| v.len() == 200 && v.iter().all(|x| x.is_finite()) && v.iter().any(|x| *x != 0.0))
}

fn outdated(features: &[String], extra: &Value, embedding: Option<(&Option<String>, &Option<String>)>) -> Vec<String> {
    features
        .iter()
        .filter(|feature| {
            if feature.as_str() == "embedding" {
                !embedding.is_some_and(|(model, vector)| embedding_valid(model.as_deref(), vector.as_deref()))
            } else {
                !current_version(feature, extra["analysis_components"][feature.as_str()].as_str())
            }
        })
        .cloned()
        .collect()
}

pub struct Selection {
    pub features: Vec<String>,
    pub candidates: Vec<Value>,
    pub selected: Vec<Value>,
    pub rekordbox_grids: usize,
}

impl Jobs {
    pub fn new(db: DbHandle, clock: Arc<dyn Clock>) -> Arc<Self> {
        let queue = AnalysisJobQueue::for_library(&db, clock.clone());
        Arc::new(Self { db, clock, queue, coordinator: Arc::new(Coordinator::default()), running: Mutex::new(None), ingest_running: Arc::new(AtomicBool::new(false)) })
    }

    fn now(&self) -> NaiveDateTime {
        self.clock.now_naive()
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn is_background_running(&self) -> bool {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.background)
    }

    fn select(&self, track_ids: Option<Vec<i64>>, genres: Option<Vec<String>>, features: Option<Vec<String>>, only_outdated: bool, limit: Option<i64>) -> Result<Selection, String> {
        let features = validate_features(features)?;
        if limit.is_some_and(|l| !(1..=100_000).contains(&l)) {
            return Err("limit must be between 1 and 100000".into());
        }
        let conn = self.db.lease().map_err(|e| e.to_string())?;
        let mut sql = "SELECT tracks.id, tracks.filepath, tracks.title, tracks.genre, track_analyses.features_extra_json, track_embeddings.model_name, track_embeddings.embedding_json \
                       FROM tracks LEFT JOIN track_analyses ON tracks.id = track_analyses.track_id LEFT JOIN track_embeddings ON tracks.id = track_embeddings.track_id WHERE true"
            .to_owned();
        let mut params: Vec<Db> = Vec::new();
        if let Some(ids) = &track_ids {
            sql.push_str(&format!(" AND tracks.id IN ({})", if ids.is_empty() { "NULL".into() } else { ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",") }));
        }
        if let Some(genres) = genres.as_ref().filter(|g| !g.is_empty()) {
            sql.push_str(&format!(" AND tracks.genre IN ({})", vec!["?"; genres.len()].join(",")));
            params.extend(genres.iter().map(|g| Db::Text(g.clone())));
        }
        sql.push_str(" ORDER BY tracks.id");
        type Row = (i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);
        let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows: Vec<Row> = statement
            .query_map(params_from_iter(params), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let priority: BTreeSet<i64> = conn.prepare("SELECT track_id FROM setlist_tracks").and_then(|mut s| s.query_map([], |r| r.get(0))?.collect()).unwrap_or_default();
        let keys = key_source_enabled(&conn);
        drop(statement);
        drop(conn);
        let rhythm_only = features == ["rhythm"];
        let mut rekordbox_grids = 0;
        let mut candidates = Vec::new();
        for (id, path, title, genre, extra, model, vector) in rows {
            let extra: Value = extra.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
            let mut needed = if only_outdated { outdated(&features, &extra, Some((&model, &vector))) } else { features.clone() };
            if needed.iter().any(|f| f == "rhythm" || f == "key") {
                let rb = lookup(&path, keys);
                let has_grid = rb.bpm.is_some() && rb.analysis_path.is_some();
                if rhythm_only && has_grid {
                    rekordbox_grids += 1;
                    continue;
                }
                needed.retain(|f| !(f == "key" && keys && rb.key.is_some()) && !(f == "rhythm" && has_grid));
            }
            candidates.push(json!({"id": id, "filepath": path, "title": title, "genre": genre, "features": needed}));
        }
        let mut eligible: Vec<Value> = candidates.iter().filter(|c| c["features"].as_array().is_some_and(|f| !f.is_empty())).cloned().collect();
        eligible.sort_by_key(|c| {
            let id = c["id"].as_i64().unwrap_or(0);
            (!priority.contains(&id), id)
        });
        if let Some(limit) = limit {
            eligible.truncate(limit as usize);
        }
        Ok(Selection { features, candidates, selected: eligible, rekordbox_grids })
    }

    pub fn plan(&self, track_ids: Option<Vec<i64>>, genres: Option<Vec<String>>, features: Option<Vec<String>>, only_outdated: bool, limit: Option<i64>) -> Result<Value, String> {
        let s = self.select(track_ids, genres, features, only_outdated, limit)?;
        let grid_only = s.features == ["rhythm"];
        let versions: Map<String, Value> = s.features.iter().map(|f| (f.clone(), json!(component_version(f)))).collect();
        let mut preserves = vec!["metadata", "genres", "lyrics", "audio_files", "unselected_features"];
        if grid_only {
            preserves.push("manual_and_rekordbox_bpm");
        }
        Ok(json!({
            "features": s.features, "versions": versions, "matched_tracks": s.candidates.len(),
            "already_current": s.candidates.iter().filter(|c| c["features"].as_array().is_some_and(|f| f.is_empty())).count(),
            "selected_tracks": s.selected.len(), "sample": s.selected.iter().take(20).collect::<Vec<_>>(),
            "rekordbox_grid_tracks": s.rekordbox_grids, "preserves": preserves, "priority": "tracks used in setlists first", "resumable": true,
        }))
    }

    fn check_available(&self, workers: i64) -> Result<(), String> {
        if !(1..=4).contains(&workers) {
            return Err("workers must be between 1 and 4".into());
        }
        if self.is_running() && !self.is_background_running() {
            return Err("An analysis job is already running; pause it first".into());
        }
        if self.ingest_running.load(Ordering::SeqCst) {
            return Err("Library import is running; wait for it to finish".into());
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(self: &Arc<Self>, track_ids: Option<Vec<i64>>, genres: Option<Vec<String>>, features: Option<Vec<String>>, only_outdated: bool, limit: Option<i64>, workers: i64) -> Result<Value, String> {
        self.check_available(workers)?;
        let s = self.select(track_ids, genres, features, only_outdated, limit)?;
        let items: Vec<QueueItem> = s.selected.iter().map(|t| QueueItem { track_id: t["id"].as_i64().unwrap_or(0), filepath: t["filepath"].as_str().map(str::to_owned) }).collect();
        let config = json!({"features": s.features, "only_outdated": only_outdated, "workers": workers});
        if self.is_background_running() {
            let id = self.queue.create_with_status(&config, &items, "queued").map_err(|e| e.to_string())?;
            self.yield_background();
            return self.queue.get(Some(&id)).map_err(|e| e.to_string());
        }
        let id = self.queue.create(&config, &items).map_err(|e| e.to_string())?;
        self.launch(&id)?;
        self.queue.get(Some(&id)).map_err(|e| e.to_string())
    }

    pub fn pause(&self, job_id: &str) -> Result<Value, String> {
        let job = self.queue.get(Some(job_id)).map_err(|_| "Analysis job not found".to_owned())?;
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = running.as_ref().filter(|j| j.id == job_id) {
            current.stop.store(true, Ordering::SeqCst);
            self.queue.update_job(job_id, "pausing", None, None).map_err(|e| e.to_string())?;
        } else if job["status"] == "queued" {
            self.queue.update_job(job_id, "paused", None, None).map_err(|e| e.to_string())?;
        }
        drop(running);
        self.queue.get(Some(job_id)).map_err(|e| e.to_string())
    }

    pub fn resume(self: &Arc<Self>, job_id: &str, workers: Option<i64>, retry_failed: bool) -> Result<Value, String> {
        let job = self.queue.get(Some(job_id)).map_err(|_| "Analysis job not found".to_owned())?;
        let workers = workers.unwrap_or_else(|| job["config"]["workers"].as_i64().unwrap_or(1));
        if self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|j| j.id == job_id) {
            return Err("This analysis job is already running".into());
        }
        self.check_available(workers)?;
        self.queue.prepare_resume(job_id, workers, retry_failed).map_err(|e| e.to_string())?;
        if self.is_background_running() {
            self.queue.update_job(job_id, "queued", None, None).map_err(|e| e.to_string())?;
            if !job["config"]["background"].as_bool().unwrap_or(false) {
                self.yield_background();
            }
        } else {
            self.launch(job_id)?;
        }
        self.queue.get(Some(job_id)).map_err(|e| e.to_string())
    }

    pub fn status(&self, job_id: Option<&str>) -> Result<Value, String> {
        let id = match job_id {
            Some(id) => Some(id.to_owned()),
            None => {
                let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
                match running.as_ref().filter(|j| !j.background) {
                    Some(job) => Some(job.id.clone()),
                    None => self.queue.next_queued(Some(false)).ok().flatten().or_else(|| self.queue.latest_id(Some(false)).ok().flatten()),
                }
            }
        };
        match id {
            Some(id) => self.queue.get(Some(&id)).map_err(|_| "Analysis job not found".to_owned()),
            None => Ok(json!({"status": "idle"})),
        }
    }

    fn yield_background(&self) {
        if let Some(job) = self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|j| j.background) {
            job.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Start the next queued job if nothing runs.
    pub fn launch_next(self: &Arc<Self>) {
        if self.is_running() {
            return;
        }
        if let Ok(Some(id)) = self.queue.next_queued(None) {
            let _ = self.launch(&id);
        }
    }

    fn launch(self: &Arc<Self>, job_id: &str) -> Result<(), String> {
        let job = self.queue.get(Some(job_id)).map_err(|e| e.to_string())?;
        let background = job["config"]["background"].as_bool().unwrap_or(false);
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if running.is_some() {
                return Err("An analysis job is already running; pause it first".into());
            }
            *running = Some(RunningJob { id: job_id.to_owned(), background, stop: stop.clone() });
        }
        self.queue.update_job(job_id, "running", None, None).map_err(|e| e.to_string())?;
        let me = self.clone();
        let id = job_id.to_owned();
        std::thread::Builder::new()
            .name("plumdeck-analysis-job".into())
            .spawn(move || {
                me.run(&id, &stop, background);
                *me.running.lock().unwrap_or_else(|e| e.into_inner()) = None;
                me.launch_next();
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn prepare_item(&self, item: &QueueItem, config: &Value) -> Result<Vec<String>, String> {
        let conn = self.db.lease().map_err(|e| e.to_string())?;
        let row: Option<(String, Option<String>, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT tracks.filepath, a.features_extra_json, e.model_name, e.embedding_json FROM tracks LEFT JOIN track_analyses a ON a.track_id=tracks.id LEFT JOIN track_embeddings e ON e.track_id=tracks.id WHERE tracks.id=?",
                [item.track_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((path, extra, model, vector)) = row.filter(|(p, ..)| Some(p) == item.filepath.as_ref()) else { return Err("Track removed or path changed since this job was created".into()) };
        let features: Vec<String> = config["features"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let extra: Value = extra.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        let mut needed = if config["only_outdated"].as_bool().unwrap_or(true) { outdated(&features, &extra, Some((&model, &vector))) } else { features.clone() };
        if features != ["rhythm"] && needed.iter().any(|f| f == "rhythm" || f == "key") {
            let keys = key_source_enabled(&conn);
            let rb = lookup(&path, keys);
            needed.retain(|f| !(f == "key" && keys && rb.key.is_some()) && !(f == "rhythm" && rb.bpm.is_some() && rb.analysis_path.is_some()));
        }
        Ok(needed)
    }

    fn run(&self, job_id: &str, stop: &AtomicBool, background: bool) {
        let started = Instant::now();
        let job = self.queue.get(Some(job_id)).unwrap_or(json!({}));
        let config = job["config"].clone();
        let elapsed = job["elapsed"].as_f64().unwrap_or(0.0);
        let token = if background {
            None
        } else {
            loop {
                if stop.load(Ordering::SeqCst) {
                    let _ = self.queue.update_job(job_id, "paused", None, None);
                    return;
                }
                if let Some(token) = self.coordinator.acquire("選択解析", false) {
                    break Some(token);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        while !stop.load(Ordering::SeqCst) {
            let slot = match &token {
                Some(_) => None,
                None => match self.coordinator.acquire("音色比較の計算", true) {
                    Some(t) => Some(t),
                    None => {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                },
            };
            let cancel = slot.as_ref().map(|t| self.coordinator.preemption(t));
            let claimed = self.queue.claim(job_id);
            let Ok(Some(item)) = claimed else {
                if let Some(t) = &slot {
                    self.coordinator.release(t);
                }
                break;
            };
            match self.prepare_item(&item, &config) {
                Ok(features) if features.is_empty() => {
                    let _ = self.queue.finish(job_id, item.track_id, "skipped", 0.0, None);
                }
                Ok(features) => {
                    let path = item.filepath.clone().unwrap_or_default();
                    let outcome = run_worker("components", vec![json!(path), json!(features)], WORKER_TIMEOUT, cancel.as_deref(), &mut |_| {});
                    let saved = outcome.and_then(|value| {
                        let result = value.get(0).cloned().unwrap_or(Value::Null);
                        let seconds = value.get(1).and_then(Value::as_f64).unwrap_or(0.0);
                        let now = self.now();
                        self.db.write(|conn| Ok(save_components(conn, &item, &result, &features, now))).map_err(|e| e.to_string())??;
                        Ok(seconds)
                    });
                    match saved {
                        Ok(seconds) => {
                            let _ = self.queue.finish(job_id, item.track_id, "completed", seconds, None);
                        }
                        Err(_) if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) => {
                            let _ = self.queue.finish(job_id, item.track_id, "pending", 0.0, None);
                        }
                        Err(error) => {
                            let _ = self.queue.finish(job_id, item.track_id, "failed", 0.0, Some(&error));
                        }
                    }
                }
                Err(error) => {
                    let _ = self.queue.finish(job_id, item.track_id, "failed", 0.0, Some(&error));
                }
            }
            if let Some(t) = &slot {
                self.coordinator.release(t);
            }
            let state = if stop.load(Ordering::SeqCst) { "pausing" } else { "running" };
            let _ = self.queue.update_job(job_id, state, Some(elapsed + started.elapsed().as_secs_f64()), None);
        }
        let latest = self.queue.get(Some(job_id)).unwrap_or(json!({}));
        let remaining = latest["remaining"].as_i64().unwrap_or(0);
        let mut status = if remaining > 0 { "paused" } else if latest["counts"]["failed"].as_i64().unwrap_or(0) > 0 { "completed_with_errors" } else { "completed" };
        if background && remaining > 0 && stop.load(Ordering::SeqCst) {
            status = "queued";
        }
        let _ = self.queue.update_job(job_id, status, Some(elapsed + started.elapsed().as_secs_f64()), None);
        if let Some(t) = &token {
            self.coordinator.release(t);
        }
    }

    /// `enqueue_embedding`: an automatic (background) embedding job for the
    /// given analyzed tracks whose vector is missing or outdated; tracks
    /// already pending in such a job are not added twice.
    pub fn enqueue_embedding(self: &Arc<Self>, track_ids: Option<&[i64]>, filepaths: Option<&[String]>) -> Result<Value, String> {
        if track_ids.is_none_or(<[i64]>::is_empty) && filepaths.is_none_or(<[String]>::is_empty) {
            return Ok(json!({"status": "idle"}));
        }
        let conn = self.db.lease().map_err(|e| e.to_string())?;
        let mut sql = format!(
            "SELECT tracks.id, tracks.filepath, track_embeddings.model_name, track_embeddings.embedding_json FROM tracks \
             LEFT JOIN track_analyses ON tracks.id = track_analyses.track_id LEFT JOIN track_embeddings ON tracks.id = track_embeddings.track_id WHERE {COMPLETED_SQL}"
        );
        let mut params: Vec<Db> = Vec::new();
        if let Some(ids) = track_ids {
            sql.push_str(&format!(" AND tracks.id IN ({})", if ids.is_empty() { "NULL".into() } else { ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",") }));
        }
        if let Some(paths) = filepaths {
            sql.push_str(&format!(" AND tracks.filepath IN ({})", if paths.is_empty() { "NULL".into() } else { vec!["?"; paths.len()].join(",") }));
            params.extend(paths.iter().map(|p| Db::Text(p.clone())));
        }
        type Row = (i64, String, Option<String>, Option<String>);
        let rows: Vec<Row> = conn
            .prepare(&sql)
            .and_then(|mut s| s.query_map(params_from_iter(params), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect())
            .map_err(|e| e.to_string())?;
        drop(conn);
        let outstanding: BTreeSet<(i64, Option<String>)> = self.queue.outstanding_embeddings().map_err(|e| e.to_string())?.into_iter().collect();
        let items: Vec<QueueItem> = rows
            .into_iter()
            .filter(|(_, _, model, vector)| !embedding_valid(model.as_deref(), vector.as_deref()))
            .filter(|(id, path, _, _)| !outstanding.contains(&(*id, Some(path.clone()))))
            .map(|(id, path, _, _)| QueueItem { track_id: id, filepath: Some(path) })
            .collect();
        if items.is_empty() {
            return Ok(json!({"status": "idle"}));
        }
        let config = json!({"features": ["embedding"], "only_outdated": true, "workers": 1, "background": true});
        let job_id = self.queue.create_with_status(&config, &items, "queued").map_err(|e| e.to_string())?;
        self.launch_next();
        self.queue.get(Some(&job_id)).map_err(|e| e.to_string())
    }

    /// `enqueue_missing_embeddings`: the startup backfill of analyzed tracks
    /// without a vector of the current model. Never fails.
    pub fn enqueue_missing_embeddings(self: &Arc<Self>) -> Value {
        let ids: Result<Vec<i64>, String> = self.db.lease().map_err(|e| e.to_string()).and_then(|conn| {
            conn.prepare(&plumdeck_db::repos::analyzed::pending_embedding_ids_sql())
                .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                .map_err(|e| e.to_string())
        });
        match ids.and_then(|ids| if ids.is_empty() { Ok(json!({"status": "idle"})) } else { self.enqueue_embedding(Some(&ids), None) }) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("WARNING: Embedding backfill was not queued: {error}");
                json!({"status": "idle"})
            }
        }
    }
}

fn component_version(feature: &str) -> &'static str {
    match feature {
        "rhythm" => plumdeck_analysis::RHYTHM_VERSION,
        "key" => plumdeck_analysis::KEY_VERSION,
        "timbre" => plumdeck_analysis::TIMBRE_VERSION,
        "waveform" => plumdeck_analysis::WAVEFORM_VERSION,
        _ => "musicnn-16khz-v1",
    }
}

// ------------------------------------------------------------------ ingest helpers

fn supported(path: &Path) -> bool {
    let name = path.to_string_lossy().to_lowercase();
    SUPPORTED_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

fn expand_user(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => PathBuf::from(format!("{home}{rest}")),
        _ => PathBuf::from(path),
    }
}

/// `expand_targets`: files of the targets (directories walked).
pub fn expand_targets(targets: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for target in targets {
        let path = expand_user(target);
        if path.is_file() {
            if supported(&path) {
                out.push(path.to_string_lossy().into_owned());
            }
        } else if path.is_dir() {
            let mut stack = vec![path];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
                entries.sort();
                for entry in entries {
                    if entry.is_dir() {
                        stack.push(entry);
                    } else if supported(&entry) {
                        out.push(entry.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    out
}

/// Tracks that already meet the analyzed rule, by NFC path.
pub fn completed_paths(conn: &Connection) -> Result<HashMap<String, (i64, Option<String>, Option<String>, Option<String>)>, DbError> {
    let mut statement = conn.prepare(&format!("SELECT tracks.id, tracks.filepath, tracks.title, tracks.artist, tracks.album FROM tracks WHERE {COMPLETED_SQL}"))?;
    let rows = statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, path, title, artist, album) = row?;
        out.insert(plumdeck_core::compat::text::nfc(&path), (id, title, artist, album));
    }
    Ok(out)
}

/// Whether the file's tags differ from the stored title/artist/album.
pub fn metadata_changed(path: &str, title: &Option<String>, artist: &Option<String>, album: &Option<String>) -> bool {
    let info = plumdeck_media::tags::basic_info(Path::new(path));
    let differs = |tag: &Option<String>, stored: &Option<String>| tag.as_deref().map(str::trim).filter(|t| !t.is_empty()).is_some_and(|t| Some(t) != stored.as_deref());
    differs(&info.title, title) || differs(&info.artist, artist) || differs(&info.album, album)
}

/// Ordered map helper for counting states.
pub fn summary(items: &[Value]) -> Value {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for item in items {
        *counts.entry(item["state"].as_str().unwrap_or("").to_owned()).or_default() += 1;
    }
    json!(counts)
}
