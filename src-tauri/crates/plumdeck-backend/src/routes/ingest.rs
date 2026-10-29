//! Analysis routes (S11): library import (`/api/ingest`, `/ws/ingest`),
//! grid jobs, on-request grid analysis, Play imports (`/api/workflows/play/
//! imports`, `/ws/play-imports`) and the MCP analysis tools' service calls.
//! Every analysis runs in a `--analysis-worker` subprocess (protocol V1).

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::types::Value as Db;
use plumdeck_db::duckdb::{params_from_iter, OptionalExt};
use plumdeck_db::repos::rows::{json_rows, scalar_int};
use serde_json::{json, Map, Value};

use crate::analysis::{self, completed_result, expand_targets, run_worker, save_track, ts, Jobs, WORKER_TIMEOUT};
use crate::response::PyJson;
use crate::tasks::BackgroundTask;
use crate::{server_error, AppState, HttpError};

/// The analysis services an [`AppState`] carries.
#[derive(Clone)]
pub struct AnalysisState {
    pub jobs: Arc<Jobs>,
    pub ingest: BackgroundTask,
    pub progress: Arc<Mutex<BTreeMap<String, Value>>>,
    pub imports: Arc<Mutex<()>>,
}

impl AnalysisState {
    pub fn new(db: plumdeck_db::handle::DbHandle, clock: Arc<dyn plumdeck_core::time::Clock>) -> Self {
        let mut extra = Map::new();
        extra.insert("file".into(), json!(""));
        Self { jobs: Jobs::new(db, clock), ingest: BackgroundTask::new(extra), progress: Arc::default(), imports: Arc::default() }
    }
}

fn to_py(value: &Value) -> PyValue {
    plumdeck_core::compat::pyjson_loads::loads(&value.to_string()).unwrap_or(PyValue::None)
}

fn answer(value: &Value) -> Response {
    PyJson(to_py(value)).into_response()
}

fn http(status: u16, detail: impl Into<String>) -> Response {
    HttpError(ApiError::new(status, detail.into())).into_response()
}

fn body(headers: &HeaderMap, bytes: &[u8]) -> Result<Value, Response> {
    match crate::body::read_body(headers, bytes)? {
        Some(value @ PyValue::Dict(_)) => Ok(serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(&value, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(Value::Null)),
        other => Err(crate::body::validation_response(&[crate::body::ValidationError::new(
            "model_attributes_type",
            vec![crate::body::Loc::from("body")],
            "Input should be a valid dictionary or object to extract fields from",
            other.unwrap_or(PyValue::None),
        )])),
    }
}

fn invalid(message: &str) -> Response {
    crate::body::validation_response(&[crate::body::ValidationError::new("value_error", vec![crate::body::Loc::from("body")], message, PyValue::None)])
}

fn ints(value: &Value) -> Option<Vec<i64>> {
    value.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect())
}

/// pydantic's lax bool (`True`/`"yes"`/`1`...).
fn lax_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_i64() == Some(0) || n.as_f64() == Some(0.0) => Some(false),
        Value::Number(n) if n.as_i64() == Some(1) || n.as_f64() == Some(1.0) => Some(true),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// `GridBatchRequest` (extra="forbid", StrictInt ids): the parsed
/// `(track_ids, only_outdated)` or a 422.
fn grid_request(payload: &Value) -> Result<(Option<Vec<i64>>, bool), Response> {
    let map = payload.as_object().cloned().unwrap_or_default();
    if let Some(key) = map.keys().find(|k| !["only_outdated", "track_ids"].contains(&k.as_str())) {
        return Err(invalid(&format!("{key}: Extra inputs are not permitted")));
    }
    let only_outdated = match map.get("only_outdated") {
        None => true,
        Some(value) => lax_bool(value).ok_or_else(|| invalid("only_outdated: Input should be a valid boolean"))?,
    };
    let track_ids = match map.get("track_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            if items.len() > 100_000 {
                return Err(invalid("track_ids: List should have at most 100000 items"));
            }
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                ids.push(item.as_i64().ok_or_else(|| invalid("track_ids: Input should be a valid integer"))?);
            }
            Some(ids)
        }
        Some(_) => return Err(invalid("track_ids: Input should be a valid list")),
    };
    Ok((track_ids, only_outdated))
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
}

// ------------------------------------------------------------------ one track

/// `process_track_ingestion` with `save_to_db`: `Ok(None)` when unchanged.
fn ingest_track(state: &AppState, filepath: &str, force: bool, save: bool, on_progress: &mut dyn FnMut(Value)) -> Result<Option<Value>, String> {
    ingest_track_with(state, filepath, force, save, false, on_progress)
}

/// `rekordbox_key`: take the key from rekordbox even when the
/// `rekordbox_key_source` setting is off (tracks imported from rekordbox's
/// collection take BPM, grid and key from rekordbox).
fn ingest_track_with(state: &AppState, filepath: &str, force: bool, save: bool, rekordbox_key: bool, on_progress: &mut dyn FnMut(Value)) -> Result<Option<Value>, String> {
    on_progress(json!({"stage": "checking", "label": "登録済み情報・音源の更新を確認しています"}));
    let lrc = PathBuf::from(filepath).with_extension("lrc");
    let lyrics = std::fs::read_to_string(&lrc).ok().filter(|t| !t.is_empty());
    let conn = state.db.lease().map_err(|e| e.to_string())?;
    let keys = rekordbox_key || analysis::key_source_enabled(&conn);
    let existing: Option<(i64, Option<String>, Option<String>, Option<String>, Option<f32>)> = conn
        .query_row("SELECT id, title, artist, album, duration FROM tracks WHERE filepath=? LIMIT 1", [filepath], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((id, title, artist, album, _)) = &existing {
        if !force {
            let completed = scalar_int(&conn, &format!("SELECT 1 FROM tracks WHERE tracks.id=? AND {}", analysis::COMPLETED_SQL), vec![Db::BigInt(*id)]).map_err(|e| e.to_string())?.is_some();
            if completed {
                if analysis::metadata_changed(filepath, title, artist, album) {
                    let info = plumdeck_media::tags::basic_info(FsPath::new(filepath));
                    let stem = FsPath::new(filepath).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let result = json!({
                        "filepath": filepath, "title": info.title.unwrap_or(stem), "artist": info.artist.unwrap_or_else(|| "Unknown".into()),
                        "album": info.album.unwrap_or_else(|| "Unknown".into()), "features_extra": {}, "lyrics": lyrics,
                    });
                    if save {
                        let now = state.clock.now_naive();
                        drop(conn);
                        state.db.write(|c| save_track(c, &result, now)).map_err(|e| e.to_string())?;
                    }
                    return Ok(Some(result));
                }
                if lyrics.is_some() {
                    let result = json!({"filepath": filepath, "lyrics": lyrics});
                    if save {
                        let now = state.clock.now_naive();
                        drop(conn);
                        state.db.write(|c| save_track(c, &result, now)).map_err(|e| e.to_string())?;
                    }
                    return Ok(Some(result));
                }
                return Ok(None);
            }
        }
    }
    drop(conn);
    let rekordbox = analysis::lookup(filepath, keys);
    let mut skip = Vec::new();
    if rekordbox.bpm.is_some() {
        skip.push("rhythm");
    }
    if rekordbox.key.is_some() {
        skip.push("key");
    }
    let args = vec![json!(filepath), json!(force), json!(false), json!(false), json!(lyrics), json!("full"), json!(skip)];
    let value = run_worker("track", args, WORKER_TIMEOUT, None, on_progress).map_err(|e| format!("Audio analysis failed for {}: {e}", file_name(filepath)))?;
    if value.is_null() {
        return Err(format!("Audio analysis produced no result for {}", file_name(filepath)));
    }
    let mut result = value;
    if !skip.is_empty() {
        let mut sources = Map::new();
        if let Some(bpm) = rekordbox.bpm.filter(|_| skip.contains(&"rhythm")) {
            result["bpm"] = json!(bpm);
            sources.insert("bpm".into(), json!("rekordbox"));
        }
        if let Some(key) = rekordbox.key.clone().filter(|_| skip.contains(&"key")) {
            let scale = key.split_whitespace().nth(1).unwrap_or("").to_lowercase();
            result["key"] = json!(key);
            result["scale"] = json!(scale);
            sources.insert("key".into(), json!("rekordbox"));
        }
        let components = &mut result["features_extra"]["analysis_components"];
        let mut merged = components["sources"].as_object().cloned().unwrap_or_default();
        merged.extend(sources);
        components["sources"] = Value::Object(merged);
    }
    if let Some(lyrics) = &lyrics {
        result["lyrics"] = json!(lyrics);
    }
    if let Some((_, title, artist, _, duration)) = &existing {
        for (key, cached) in [("title", title), ("artist", artist)] {
            let missing = result[key].as_str().is_none_or(|v| v.is_empty() || v.eq_ignore_ascii_case("unknown"));
            if missing {
                if let Some(cached) = cached.as_ref().filter(|c| !c.eq_ignore_ascii_case("unknown")) {
                    result[key] = json!(cached);
                }
            }
        }
        if !result["duration"].as_f64().is_some_and(|d| d > 0.0) {
            if let Some(duration) = duration.filter(|d| *d > 0.0) {
                result["duration"] = json!(f64::from(duration));
            }
        }
    }
    if !completed_result(&result) {
        return Err(format!("Audio analysis returned incomplete BPM, duration, timbre or playback data for {}", file_name(filepath)));
    }
    if save {
        on_progress(json!({"stage": "saving", "label": "解析結果をライブラリに保存しています"}));
        let now = state.clock.now_naive();
        state.db.write(|c| save_track(c, &result, now)).map_err(|e| e.to_string())?;
    }
    Ok(Some(result))
}

fn file_name(path: &str) -> String {
    FsPath::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ------------------------------------------------------------------ ingest

async fn run_ingestion(state: AppState, task: BackgroundTask, targets: Vec<String>, force: bool, profile: String) -> Result<(), String> {
    let jobs = state.analysis.jobs.clone();
    task.update(&[("type", json!("start")), ("stage", json!("音源ファイルを検索中")), ("active_files", json!({})), ("active_progress", json!({})), ("failed_files", json!([])), ("last_error", json!("")), ("queued", json!(false)), ("analysis_profile", json!(profile)), ("effective_analysis_profile", json!("full"))]);
    task.emit();
    let token = loop {
        if let Some(token) = jobs.coordinator.acquire("Explorer解析", false) {
            break token;
        }
        task.update(&[("queued", json!(true)), ("stage", json!(format!("{}の完了を待っています", jobs.coordinator.owner().unwrap_or_else(|| "別の解析".into()))))]);
        task.emit();
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    jobs.ingest_running.store(true, Ordering::SeqCst);
    let release = scopeguard(jobs.clone(), token);
    let db = state.db.clone();
    let (files, completed) = tokio::task::spawn_blocking(move || {
        let files = expand_targets(&targets);
        let completed = db.lease().ok().and_then(|conn| analysis::completed_paths(&conn).ok()).unwrap_or_default();
        (files, completed)
    })
    .await
    .map_err(|e| e.to_string())?;
    task.update(&[("stage", json!("解析が必要な音源を確認中")), ("queued", json!(false))]);
    task.emit();
    // Ready tracks (skipped or analyzed) get their optional embedding from
    // the background job once this import releases the analysis slot.
    let embedding_paths: Vec<String> = files.iter().map(|file| plumdeck_core::compat::text::nfc(file)).collect();
    let mut to_process = Vec::new();
    for file in files {
        let known = completed.get(&plumdeck_core::compat::text::nfc(&file));
        let has_lrc = PathBuf::from(&file).with_extension("lrc").exists();
        let skip = !force && known.is_some_and(|(_, title, artist, album)| !has_lrc && !analysis::metadata_changed(&file, title, artist, album));
        if !skip {
            to_process.push(file);
        }
    }
    if to_process.is_empty() {
        task.update(&[("type", json!("complete")), ("total", json!(0))]);
        task.emit();
        drop(release);
        enqueue_embeddings(&jobs, embedding_paths).await;
        return Ok(());
    }
    task.update(&[("type", json!("processing")), ("total", json!(to_process.len())), ("current", json!(0)), ("file", json!("")), ("stage", json!("音源を解析中"))]);
    task.emit();
    let workers = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1).clamp(1, 4)).unwrap_or(1);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(workers));
    let mut handles = Vec::new();
    for file in to_process {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| e.to_string())?;
        let (state, task) = (state.clone(), task.clone());
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let done = ["processed", "skipped", "errors"].iter().map(|k| task.state()[*k].as_i64().unwrap_or(0)).sum::<i64>() + 1;
            task.update(&[("file", json!(file_name(&file))), ("current", json!(done)), ("type", json!("processing"))]);
            set_detail(&task, "active_progress", &file, json!("解析ワーカーを起動しています"));
            task.emit();
            let progress_task = task.clone();
            let progress_file = file.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                ingest_track(&state, &progress_file.clone(), force, true, &mut |event| {
                    set_detail(&progress_task, "active_progress", &progress_file, event["label"].clone());
                    progress_task.emit();
                })
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match outcome {
                Ok(Some(_)) => task.add("processed", 1),
                Ok(None) => task.add("skipped", 1),
                Err(error) => {
                    task.add("errors", 1);
                    let mut failed = task.state()["details"]["failed_files"].as_array().cloned().unwrap_or_default();
                    failed.push(json!(file));
                    task.update(&[("failed_files", json!(failed)), ("last_error", json!(format!("{}: {error}", file_name(&file))))]);
                }
            }
            remove_detail(&task, "active_progress", &file);
            let current = ["processed", "skipped", "errors"].iter().map(|k| task.state()[*k].as_i64().unwrap_or(0)).sum::<i64>();
            task.update(&[("type", json!("progress")), ("current", json!(current))]);
            task.emit();
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
    task.update(&[("type", json!("complete")), ("file", json!(""))]);
    task.emit();
    drop(release);
    enqueue_embeddings(&jobs, embedding_paths).await;
    Ok(())
}

/// Queue the optional embeddings of imported paths, 200 at a time (only
/// analyzed tracks without a current vector are added).
async fn enqueue_embeddings(jobs: &Arc<Jobs>, paths: Vec<String>) {
    let jobs = jobs.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let mut unique = Vec::new();
        for path in paths {
            if !unique.contains(&path) {
                unique.push(path);
            }
        }
        for chunk in unique.chunks(200) {
            if let Err(error) = jobs.enqueue_embedding(None, Some(chunk)) {
                tracing::warn!("WARNING: could not queue optional embeddings: {error}");
            }
        }
    })
    .await;
}

fn set_detail(task: &BackgroundTask, key: &str, file: &str, value: Value) {
    let mut map = task.state()["details"][key].as_object().cloned().unwrap_or_default();
    map.insert(file.to_owned(), value);
    task.update(&[(key, Value::Object(map))]);
}

fn remove_detail(task: &BackgroundTask, key: &str, file: &str) {
    let mut map = task.state()["details"][key].as_object().cloned().unwrap_or_default();
    map.remove(file);
    task.update(&[(key, Value::Object(map))]);
}

/// Releases the analysis slot and the import flag when the ingest ends.
struct SlotGuard(Arc<Jobs>, String);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.ingest_running.store(false, Ordering::SeqCst);
        self.0.coordinator.release(&self.1);
        self.0.launch_next();
    }
}

fn scopeguard(jobs: Arc<Jobs>, token: String) -> SlotGuard {
    SlotGuard(jobs, token)
}

/// `POST /api/ingest`.
pub async fn ingest(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(targets) = strings(&payload["targets"]) else { return invalid("targets is required") };
    let force = payload["force_update"].as_bool().unwrap_or(false);
    let profile = payload["analysis_profile"].as_str().unwrap_or("auto").to_owned();
    if !["auto", "light", "full"].contains(&profile.as_str()) {
        return invalid("analysis_profile must be auto, light or full");
    }
    let jobs = &state.analysis.jobs;
    if jobs.is_running() && !jobs.is_background_running() {
        return answer(&json!({"status": "error", "message": "Audio analysis is running; pause it before importing"}));
    }
    let task = state.analysis.ingest.clone();
    if task.is_running() {
        return answer(&json!({"status": "error", "message": "Ingestion already running"}));
    }
    let worker_state = state.clone();
    let started = task.start(move |task| run_ingestion(worker_state, task, targets, force, profile));
    if !started {
        return answer(&json!({"status": "error", "message": "Ingestion already running"}));
    }
    answer(&json!({"status": "success", "message": "Ingestion started", "state": task.state()}))
}

/// `GET /api/ingest/status`.
pub async fn ingest_status(State(state): State<AppState>) -> Response {
    let task = &state.analysis.ingest;
    let mut value = task.state();
    value["is_running"] = json!(task.is_running());
    answer(&value)
}

/// `POST /api/ingest/cancel`.
pub async fn cancel_ingest(State(state): State<AppState>) -> Response {
    let task = state.analysis.ingest.clone();
    task.cancel().await;
    state.analysis.jobs.ingest_running.store(false, Ordering::SeqCst);
    answer(&json!({"status": "success", "message": "Ingestion cancelled", "state": task.state()}))
}

/// `/ws/ingest`.
pub async fn ws_ingest(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    let task = state.analysis.ingest.clone();
    upgrade.on_upgrade(move |socket| crate::routes::metadata_jobs::serve_task(socket, task))
}

// ------------------------------------------------------------------ grid jobs

fn job_answer(result: Result<Value, String>, status: u16) -> Response {
    match result {
        Ok(value) => answer(&value),
        Err(message) => http(status, message),
    }
}

/// `POST /api/grid-jobs/plan`.
pub async fn plan_grid_job(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (track_ids, only_outdated) = match grid_request(&payload) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let jobs = state.analysis.jobs.clone();
    let result = tokio::task::spawn_blocking(move || jobs.plan(track_ids, None, Some(vec!["rhythm".into()]), only_outdated, None)).await;
    match result {
        Ok(result) => job_answer(result, 422),
        Err(error) => server_error(&error),
    }
}

/// `POST /api/grid-jobs`.
pub async fn start_grid_job(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (track_ids, only_outdated) = match grid_request(&payload) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let jobs = state.analysis.jobs.clone();
    let result = tokio::task::spawn_blocking(move || jobs.start(track_ids, None, Some(vec!["rhythm".into()]), only_outdated, None, 1)).await;
    match result {
        Ok(result) => job_answer(result, 409),
        Err(error) => server_error(&error),
    }
}

/// `GET /api/grid-jobs`.
pub async fn grid_job_status(State(state): State<AppState>) -> Response {
    let jobs = state.analysis.jobs.clone();
    match tokio::task::spawn_blocking(move || jobs.status(None)).await {
        Ok(result) => job_answer(result, 422),
        Err(error) => server_error(&error),
    }
}

/// `POST /api/grid-jobs/{job_id}/{action}`.
pub async fn control_grid_job(State(state): State<AppState>, Path((job_id, action)): Path<(String, String)>) -> Response {
    let jobs = state.analysis.jobs.clone();
    let result = tokio::task::spawn_blocking(move || {
        let job = jobs.status(Some(&job_id))?;
        if job["config"]["features"] != json!(["rhythm"]) {
            return Err("この画面ではグリッドの再解析だけを操作できます".to_owned());
        }
        match action.as_str() {
            "pause" => jobs.pause(&job_id),
            "resume" | "retry" => jobs.resume(&job_id, Some(1), action == "retry"),
            _ => Err("Unknown grid job action".into()),
        }
    })
    .await;
    match result {
        Ok(result) => job_answer(result, 409),
        Err(error) => server_error(&error),
    }
}

/// `POST /api/tracks/{track_id}/grid-analysis` (`{"force": bool}`).
pub async fn grid_analysis(State(state): State<AppState>, Path(track_id): Path<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let Ok(id) = track_id.parse::<i64>() else { return invalid("track_id must be an integer") };
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let force = payload["force"].as_bool().unwrap_or(false);
    let now = state.clock.now_naive();
    let (db, jobs) = (state.db.clone(), state.analysis.jobs.clone());
    let result = tokio::task::spawn_blocking(move || -> Result<Value, (u16, String)> {
        let conn = db.lease().map_err(|e| (500, e.to_string()))?;
        let track: Option<(String, Option<f32>)> = conn.query_row("SELECT filepath, duration FROM tracks WHERE id=?", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|e| (500, e.to_string()))?;
        let Some((path, duration)) = track else { return Err((404, "Track not found".into())) };
        let fingerprint = fingerprint(&path);
        let key = format!("{fingerprint}:grid-v2");
        if !force {
            if let Ok(Some(cached)) = plumdeck_db::repos::performance::cached_grid(&conn, id, "analysis", &key) {
                if let Ok(grid) = serde_json::from_str::<Value>(&cached) {
                    return Ok(grid);
                }
            }
        }
        drop(conn);
        if !FsPath::new(&path).is_file() {
            return Err((404, "The audio file is unavailable".into()));
        }
        let token = jobs.coordinator.acquire_wait("グリッド解析");
        let outcome = run_worker("rhythm-grid", vec![json!(path)], Duration::from_secs(120), None, &mut |_| {});
        jobs.coordinator.release(&token);
        let grid = outcome.map_err(|e| (422, format!("Grid analysis failed: {e}")))?;
        if self::fingerprint(&path) != fingerprint {
            return Err((409, "The audio file changed during analysis; retry".into()));
        }
        let ticks: Vec<f64> = grid["ticks"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        let candidate = plumdeck_analysis::dsp::playback_grid(&ticks, grid["bpm"].as_f64().unwrap_or(0.0), grid["confidence"].as_f64()).map_err(|e| (422, e))?;
        let Some(candidate) = candidate else { return Err((422, "The rhythm analyzer found no usable beats".into())) };
        let parsed = plumdeck_domain::performance::BeatGrid::parse(&to_py(&candidate)).map_err(|_| (422, "Analysis returned invalid beat values".to_owned()))?;
        let duration_ms = f64::from(duration.unwrap_or(0.0)) * 1000.0;
        if duration_ms > 0.0 && parsed.last_ms() >= duration_ms {
            return Err((422, "The source grid extends beyond the catalog track duration; verify the audio duration before saving".into()));
        }
        let serialized = plumdeck_core::compat::pyjson::dumps(&parsed.json(), &plumdeck_core::compat::pyjson::DumpOptions::starlette()).unwrap_or_default();
        let provenance = json!({"algorithm": plumdeck_analysis::RHYTHM_VERSION, "sample_rate": 11025, "confidence": "raw algorithm score, not a probability", "forced": force}).to_string();
        db.write(|conn| plumdeck_db::repos::performance::cache_grid(conn, id, "analysis", &serialized, &key, &provenance, now)).map_err(|e| (500, e.to_string()))?;
        Ok(serde_json::from_str(&serialized).unwrap_or(Value::Null))
    })
    .await;
    match result {
        Ok(Ok(grid)) => crate::response::pydantic(to_py(&grid)).into_response(),
        Ok(Err((status, message))) if status == 500 => server_error(&message),
        Ok(Err((status, message))) => http(status, message),
        Err(error) => server_error(&error),
    }
}

/// `audio_fingerprint`.
fn fingerprint(path: &str) -> String {
    use sha2::Digest;
    let value = match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            format!("[{}, {}, {mtime}]", serde_json::to_string(path).unwrap_or_default(), m.len())
        }
        Err(_) => format!("[{}, \"missing\"]", serde_json::to_string(path).unwrap_or_default()),
    };
    sha2::Sha256::digest(value.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------------ Play imports

/// `import_batches.origin` of imports from rekordbox's collection.
pub const REKORDBOX_ORIGIN: &str = "rekordbox";

fn batch(state: &AppState, conn: &plumdeck_db::duckdb::Connection, id: &str) -> Result<Option<Value>, String> {
    let rows = json_rows(conn, "SELECT * FROM import_batches WHERE id=?", vec![Db::Text(id.into())]).map_err(|e| e.to_string())?;
    let Some(row) = rows.into_iter().next() else { return Ok(None) };
    let mut result: Value = serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(&row, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(json!({}));
    result["progress"] = state.analysis.progress.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned().unwrap_or(Value::Null);
    let items = json_rows(
        conn,
        "SELECT i.*,intent.state AS membership_state,intent.setlist_track_id FROM import_items i JOIN import_target_intents intent ON intent.item_id=i.id WHERE i.batch_id=? ORDER BY i.input_order",
        vec![Db::Text(id.into())],
    )
    .map_err(|e| e.to_string())?;
    let items: Vec<Value> = items.iter().map(|i| serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(i, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(Value::Null)).collect();
    result["summary"] = analysis::summary(&items);
    result["items"] = Value::Array(items);
    Ok(Some(result))
}

fn list_batches(state: &AppState, active: bool) -> Result<Vec<Value>, String> {
    let conn = state.db.lease().map_err(|e| e.to_string())?;
    let filter = if active { "WHERE b.state NOT IN ('completed','completed_with_errors','canceled')" } else { "" };
    let sql = format!(
        "SELECT b.*, count(i.id) AS total_items, count(i.id) FILTER (WHERE i.state IN ('completed','existing')) AS succeeded_items, \
         count(i.id) FILTER (WHERE i.state='failed') AS failed_items, count(i.id) FILTER (WHERE i.state='skipped') AS skipped_items, \
         count(i.id) FILTER (WHERE i.state='queued') AS queued_items, max(i.canonical_path) FILTER (WHERE i.state IN ('probing','analyzing')) AS current_file \
         FROM import_batches b LEFT JOIN import_items i ON i.batch_id=b.id {filter} GROUP BY ALL ORDER BY b.created_at DESC {}",
        if active { "" } else { "LIMIT 100" }
    );
    let rows = json_rows(&conn, &sql, vec![]).map_err(|e| e.to_string())?;
    let progress = state.analysis.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(rows
        .iter()
        .map(|r| {
            let mut value: Value = serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(r, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(json!({}));
            let id = value["id"].as_str().unwrap_or("").to_owned();
            value["progress"] = progress.get(&id).cloned().unwrap_or(Value::Null);
            value
        })
        .collect())
}

fn expand_import_paths(paths: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for raw in paths {
        let candidate = PathBuf::from(raw);
        if !candidate.is_absolute() {
            continue;
        }
        let Ok(resolved) = std::fs::canonicalize(&candidate) else { continue };
        let found: Vec<PathBuf> = if resolved.is_file() {
            vec![resolved.clone()]
        } else if resolved.is_dir() {
            let mut files: Vec<PathBuf> = expand_targets(&[resolved.to_string_lossy().into_owned()]).into_iter().map(PathBuf::from).collect();
            files.sort_by_key(|p| p.strip_prefix(&resolved).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default());
            files
        } else {
            Vec::new()
        };
        for item in found {
            let lower = item.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
            if !analysis::SUPPORTED_EXTENSIONS.contains(&lower.as_str()) {
                continue;
            }
            let real = std::fs::canonicalize(&item).unwrap_or(item);
            if !out.contains(&real) {
                out.push(real);
            }
        }
    }
    out
}

/// `POST /api/workflows/play/imports` (202).
pub async fn create_play_import(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (Some(request_id), Some(paths)) = (payload["request_id"].as_str().map(str::to_owned), strings(&payload["paths"])) else { return invalid("request_id and paths are required") };
    if request_id.is_empty() || request_id.chars().count() > 200 || paths.is_empty() || paths.len() > 2000 {
        return invalid("invalid import request");
    }
    // PlayImportRequest's Literal fields answer 422 before the service runs.
    if !payload["target"].is_object() {
        return invalid("target: Field required");
    }
    if !payload["target"]["kind"].as_str().is_some_and(|k| ["collection", "local_playlist"].contains(&k)) {
        return invalid("target.kind: Input should be 'collection' or 'local_playlist'");
    }
    if payload.get("origin").is_some_and(|o| !o.as_str().is_some_and(|o| ["native_file_drop", "file_picker"].contains(&o))) {
        return invalid("origin: Input should be 'native_file_drop' or 'file_picker'");
    }
    if payload.get("analysis_profile").is_some_and(|p| !p.as_str().is_some_and(|p| ["auto", "light", "full"].contains(&p))) {
        return invalid("analysis_profile: Input should be 'auto', 'light' or 'full'");
    }
    let kind = payload["target"]["kind"].as_str().unwrap_or("").to_owned();
    let target_id = payload["target"]["id"].as_i64();
    let origin = payload["origin"].as_str().unwrap_or("native_file_drop").to_owned();
    let profile = payload["analysis_profile"].as_str().unwrap_or("auto").to_owned();
    let request = ImportRequest { request_id, paths, kind, target_id, origin, profile };
    match start_import(&state, request).await {
        Ok(value) => (axum::http::StatusCode::ACCEPTED, PyJson(to_py(&value))).into_response(),
        Err(Ok(message)) => http(409, message),
        Err(Err(response)) => response,
    }
}

/// One Play import request (the shared import queue: drops, the file picker,
/// the Dashboard's attention fixes and rekordbox imports).
pub struct ImportRequest {
    pub request_id: String,
    pub paths: Vec<String>,
    pub kind: String,
    pub target_id: Option<i64>,
    pub origin: String,
    pub profile: String,
}

/// Records a batch (or returns the one with the same request id) and starts
/// it. `Err(Ok(message))` is a 409 for the caller to word.
pub async fn start_import(state: &AppState, request: ImportRequest) -> Result<Value, Result<String, Response>> {
    let ImportRequest { request_id, paths, kind, target_id, origin, profile } = request;
    let worker = state.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let conn = worker.db.lease().map_err(|e| e.to_string())?;
        if let Some(existing) = conn.query_row("SELECT id FROM import_batches WHERE request_id=?", [&request_id], |r| r.get::<_, String>(0)).optional().map_err(|e| e.to_string())? {
            return batch(&worker, &conn, &existing).map(|b| b.unwrap_or(Value::Null));
        }
        if !["collection", "local_playlist"].contains(&kind.as_str()) {
            return Err("この場所には音源を取り込めません".into());
        }
        if !["auto", "light", "full"].contains(&profile.as_str()) {
            return Err("解析方法が不正です".into());
        }
        let mut target_name = "Collection".to_owned();
        if kind == "local_playlist" {
            let name: Option<Option<String>> = conn.query_row("SELECT name FROM setlists WHERE id=?", [target_id.unwrap_or(-1)], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
            target_name = name.ok_or("追加先プレイリストが見つかりません")?.unwrap_or_default();
        }
        drop(conn);
        let expanded = expand_import_paths(&paths);
        if expanded.is_empty() {
            return Err("対応する読み取り可能な音源がありません".into());
        }
        let batch_id = format!("import-{}", uuid::Uuid::new_v4());
        let now = worker.clock.now_naive();
        worker
            .db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO import_batches (id,request_id,target_kind,target_id,target_name_snapshot,origin,analysis_profile,state,created_at,updated_at) VALUES (?,?,?,?,?,?,?,'queued',?,?)",
                    params_from_iter([Db::Text(batch_id.clone()), Db::Text(request_id.clone()), Db::Text(kind.clone()), target_id.map_or(Db::Null, Db::BigInt), Db::Text(target_name.clone()), Db::Text(origin.clone()), Db::Text(profile.clone()), ts(now), ts(now)]),
                )?;
                for (index, path) in expanded.iter().enumerate() {
                    let item_id = format!("item-{}", uuid::Uuid::new_v4());
                    let metadata = std::fs::metadata(path).map_err(plumdeck_db::DbError::from)?;
                    let mtime = metadata.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
                    let text = path.to_string_lossy().into_owned();
                    conn.execute(
                        "INSERT INTO import_items (id,batch_id,input_order,source_path,canonical_path,size_bytes,mtime_ns,state) VALUES (?,?,?,?,?,?,?,'queued')",
                        params_from_iter([Db::Text(item_id.clone()), Db::Text(batch_id.clone()), Db::BigInt(index as i64), Db::Text(text.clone()), Db::Text(text), Db::BigInt(metadata.len() as i64), Db::BigInt(mtime)]),
                    )?;
                    conn.execute(
                        "INSERT INTO import_target_intents (item_id,batch_id,target_kind,target_id,reserved_position,state) VALUES (?,?,?,?,?,'pending')",
                        params_from_iter([Db::Text(item_id), Db::Text(batch_id.clone()), Db::Text(kind.clone()), target_id.map_or(Db::Null, Db::BigInt), Db::BigInt(index as i64)]),
                    )?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        let conn = worker.db.lease().map_err(|e| e.to_string())?;
        batch(&worker, &conn, &batch_id).map(|b| b.unwrap_or(Value::Null))
    })
    .await;
    match result {
        Ok(Ok(value)) => {
            if ["queued", "paused"].contains(&value["state"].as_str().unwrap_or("")) {
                spawn_batch(state.clone(), value["id"].as_str().unwrap_or("").to_owned());
            }
            Ok(value)
        }
        Ok(Err(message)) => Err(Ok(message)),
        Err(error) => Err(Err(server_error(&error))),
    }
}

/// `GET /api/workflows/play/imports?active=`.
pub async fn list_play_imports(State(state): State<AppState>, uri: Uri) -> Response {
    let active = match crate::query::Params::from_uri(&uri).opt_str("active") {
        None => false,
        Some(text) => match lax_bool(&Value::String(text)) {
            Some(active) => active,
            None => {
                return crate::body::validation_response(&[crate::body::ValidationError::new(
                    "bool_parsing",
                    vec![crate::body::Loc::from("query"), crate::body::Loc::from("active")],
                    "Input should be a valid boolean, unable to interpret input",
                    PyValue::None,
                )])
            }
        },
    };
    let worker = state.clone();
    match tokio::task::spawn_blocking(move || list_batches(&worker, active)).await {
        Ok(Ok(rows)) => answer(&Value::Array(rows)),
        Ok(Err(message)) => server_error(&message),
        Err(error) => server_error(&error),
    }
}

/// `GET /api/workflows/play/imports/{batch_id}`.
pub async fn get_play_import(State(state): State<AppState>, Path(batch_id): Path<String>) -> Response {
    let worker = state.clone();
    let result = tokio::task::spawn_blocking(move || worker.db.lease().map_err(|e| e.to_string()).and_then(|conn| batch(&worker, &conn, &batch_id))).await;
    match result {
        Ok(Ok(Some(value))) => answer(&value),
        Ok(Ok(None)) => http(404, "取込ジョブが見つかりません"),
        Ok(Err(message)) => server_error(&message),
        Err(error) => server_error(&error),
    }
}

/// `POST /api/workflows/play/imports/{batch_id}/{action}`.
pub async fn control_play_import(State(state): State<AppState>, Path((batch_id, action)): Path<(String, String)>) -> Response {
    if !["pause", "resume", "cancel", "retry"].contains(&action.as_str()) {
        return invalid("action must be pause, resume, cancel or retry");
    }
    let worker = state.clone();
    let id = batch_id.clone();
    let act = action.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Option<Value>, String> {
        let now = worker.clock.now_naive();
        worker
            .db
            .write(|conn| {
                let p = |values: Vec<Db>| params_from_iter(values);
                let key = || Db::Text(id.clone());
                match act.as_str() {
                    "pause" => conn.execute("UPDATE import_batches SET paused=true,state='paused',updated_at=? WHERE id=?", p(vec![ts(now), key()]))?,
                    "resume" => conn.execute("UPDATE import_batches SET paused=false,state='queued',updated_at=? WHERE id=? AND NOT cancel_requested", p(vec![ts(now), key()]))?,
                    "cancel" => {
                        conn.execute("UPDATE import_batches SET cancel_requested=true,state='canceled',updated_at=? WHERE id=?", p(vec![ts(now), key()]))?;
                        conn.execute("UPDATE import_target_intents SET state='cancelled' WHERE batch_id=? AND state='pending'", p(vec![key()]))?
                    }
                    _ => {
                        conn.execute("UPDATE import_items SET state='queued',analysis_level=NULL,error_code=NULL,error_message=NULL WHERE batch_id=? AND state IN ('failed','probing','analyzing')", p(vec![key()]))?;
                        conn.execute("UPDATE import_target_intents SET state='pending' WHERE batch_id=? AND state='cancelled'", p(vec![key()]))?;
                        conn.execute("UPDATE import_batches SET paused=false,cancel_requested=false,state='queued',updated_at=? WHERE id=?", p(vec![ts(now), key()]))?
                    }
                };
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        let conn = worker.db.lease().map_err(|e| e.to_string())?;
        batch(&worker, &conn, &id)
    })
    .await;
    match result {
        Ok(Ok(Some(value))) => {
            if action == "resume" || action == "retry" {
                spawn_batch(state.clone(), batch_id);
            }
            answer(&value)
        }
        Ok(Ok(None)) => http(409, "取込ジョブが見つかりません"),
        Ok(Err(message)) => http(409, message),
        Err(error) => server_error(&error),
    }
}

fn spawn_batch(state: AppState, batch_id: String) {
    std::thread::spawn(move || {
        let lock = state.analysis.imports.clone();
        let _serial = lock.lock().unwrap_or_else(|e| e.into_inner());
        let token = state.analysis.jobs.coordinator.acquire_wait("Play取り込み");
        if let Err(error) = process_batch(&state, &batch_id) {
            let now = state.clock.now_naive();
            let message: String = error.chars().take(1000).collect();
            let _ = state.db.write(|conn| {
                conn.execute(
                    "UPDATE import_items SET state='failed',analysis_level='failed',error_code='batch_failed',error_message=? WHERE batch_id=? AND state IN ('queued','probing','analyzing')",
                    params_from_iter([Db::Text(message.clone()), Db::Text(batch_id.clone())]),
                )?;
                conn.execute("UPDATE import_batches SET state='completed_with_errors',updated_at=? WHERE id=? AND state NOT IN ('paused','canceled')", params_from_iter([ts(now), Db::Text(batch_id.clone())]))?;
                Ok(())
            });
        }
        state.analysis.progress.lock().unwrap_or_else(|e| e.into_inner()).remove(&batch_id);
        // Playable tracks get their optional embedding from the background job.
        let ready: Result<Vec<i64>, String> = state.db.lease().map_err(|e| e.to_string()).and_then(|conn| {
            conn.prepare("SELECT DISTINCT track_id FROM import_items WHERE batch_id=? AND load_ready AND track_id IS NOT NULL")
                .and_then(|mut s| s.query_map([batch_id.as_str()], |r| r.get(0))?.collect())
                .map_err(|e| e.to_string())
        });
        if let Err(error) = ready.and_then(|ids| state.analysis.jobs.enqueue_embedding(Some(&ids), None)) {
            tracing::warn!("WARNING: could not queue optional embeddings: {error}");
        }
        state.analysis.jobs.coordinator.release(&token);
        state.analysis.jobs.launch_next();
    });
}

fn process_batch(state: &AppState, batch_id: &str) -> Result<(), String> {
    let db = |e: plumdeck_db::DbError| e.to_string();
    let conn = state.db.lease().map_err(db)?;
    let Some(current) = batch(state, &conn, batch_id)? else { return Ok(()) };
    drop(conn);
    if current["cancel_requested"].as_bool().unwrap_or(false) || current["paused"].as_bool().unwrap_or(false) {
        return Ok(());
    }
    let clock_time = || state.clock.now_naive();
    let from_rekordbox = current["origin"].as_str() == Some(REKORDBOX_ORIGIN);
    state.db.write(|c| c.execute("UPDATE import_batches SET state='processing',updated_at=? WHERE id=?", params_from_iter([ts(clock_time()), Db::Text(batch_id.into())])).map(|_| ()).map_err(Into::into)).map_err(db)?;
    for item in current["items"].as_array().cloned().unwrap_or_default() {
        let flags: Option<(bool, bool)> = state.db.lease().map_err(db)?.query_row("SELECT paused,cancel_requested FROM import_batches WHERE id=?", [batch_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|e| e.to_string())?;
        if flags.is_none_or(|(paused, cancel)| paused || cancel) {
            return Ok(());
        }
        if !["queued", "failed", "probing", "analyzing"].contains(&item["state"].as_str().unwrap_or("")) {
            continue;
        }
        let item_id = item["id"].as_str().unwrap_or("").to_owned();
        let path = item["canonical_path"].as_str().unwrap_or("").to_owned();
        let progress = state.analysis.progress.clone();
        let mut report = |event: Value| {
            progress.lock().unwrap_or_else(|e| e.into_inner()).insert(batch_id.to_owned(), json!({"file": path, "stage": event["stage"], "label": event["label"]}));
        };
        let outcome = (|| -> Result<(), String> {
            report(json!({"stage": "checking", "label": "音源の内容・登録済み情報を確認しています"}));
            let metadata = std::fs::metadata(&path).map_err(|_| "音源を読み取れません".to_owned())?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err("音源を読み取れません".into());
            }
            let identity = analysis::sha256_file(FsPath::new(&path)).ok_or("音源を読み取れません")?;
            state.db.write(|c| c.execute("UPDATE import_items SET state='probing',sha256=?,attempts=attempts+1 WHERE id=?", params_from_iter([Db::Text(identity.clone()), Db::Text(item_id.clone())])).map(|_| ()).map_err(Into::into)).map_err(db)?;
            let conn = state.db.lease().map_err(db)?;
            let mut track: Option<i64> = conn.query_row("SELECT id FROM tracks WHERE filepath=? LIMIT 1", [&path], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
            if let Some(id) = track {
                let known: Option<Option<String>> = conn.query_row("SELECT sha256 FROM track_media WHERE track_id=?", [id], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
                if known.flatten().is_some_and(|k| !k.is_empty() && k != identity) {
                    return Err("登録済み音源の内容が変化しています。参照修復または再解析を行ってください".into());
                }
            }
            let completed = |id: i64| scalar_int(&conn, &format!("SELECT 1 FROM tracks WHERE tracks.id=? AND {}", analysis::COMPLETED_SQL), vec![Db::BigInt(id)]).ok().flatten().is_some();
            let mut reused = track.is_some_and(completed);
            if track.is_none() {
                let matches: Vec<(i64, String)> = conn
                    .prepare("SELECT t.id, t.filepath FROM track_media m JOIN tracks t ON t.id=m.track_id WHERE m.sha256=?")
                    .and_then(|mut s| s.query_map([&identity], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
                    .map_err(|e| e.to_string())?;
                if matches.len() == 1 && analysis::sha256_file(FsPath::new(&matches[0].1)).as_deref() == Some(identity.as_str()) {
                    track = Some(matches[0].0);
                    reused = completed(matches[0].0);
                } else if matches.len() > 1 {
                    return Err("同じ内容の既存曲が複数あり、選択が必要です".into());
                }
            }
            drop(conn);
            if !reused {
                state.db.write(|c| c.execute("UPDATE import_items SET state='analyzing' WHERE id=?", params_from_iter([Db::Text(item_id.clone())])).map(|_| ()).map_err(Into::into)).map_err(db)?;
                let result = ingest_track_with(state, &path, track.is_some(), false, from_rekordbox, &mut report)?.ok_or("解析に必要な情報（BPM・長さ・特徴量・ビートなど）が不足しています")?;
                if !completed_result(&result) {
                    return Err("解析に必要な情報（BPM・長さ・特徴量・ビートなど）が不足しています".into());
                }
                report(json!({"stage": "saving", "label": "音源の整合性を確認し、解析結果を保存しています"}));
                let after = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                if after.len() != metadata.len() || after.modified().ok() != metadata.modified().ok() || analysis::sha256_file(FsPath::new(&path)).as_deref() != Some(identity.as_str()) {
                    return Err("解析中に音源が変更されました。再試行してください".into());
                }
                let at = clock_time();
                track = Some(state.db.write(|c| save_track(c, &result, at)).map_err(db)?.0);
            }
            let track_id = track.ok_or("曲を保存できませんでした")?;
            let at = clock_time();
            state
                .db
                .write(|c| {
                    let filepath: String = c.query_row("SELECT filepath FROM tracks WHERE id=?", [track_id], |r| r.get(0))?;
                    let stat = std::fs::metadata(&filepath)?;
                    let mtime = stat.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
                    c.execute(
                        "INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at) VALUES (?,?,?,?,'available',?) ON CONFLICT(track_id) DO UPDATE SET \
                         sha256=excluded.sha256,size_bytes=excluded.size_bytes, mtime_ns=excluded.mtime_ns,status='available',last_verified_at=excluded.last_verified_at",
                        params_from_iter([Db::BigInt(track_id), Db::Text(identity.clone()), Db::BigInt(stat.len() as i64), Db::BigInt(mtime), ts(at)]),
                    )?;
                    let intent: Option<(String, String, Option<i64>)> = c.query_row("SELECT state, target_kind, target_id FROM import_target_intents WHERE item_id=?", [&item_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
                    let (mut membership, mut entry) = ("not_required".to_owned(), None::<i64>);
                    if let Some((intent_state, kind, Some(target))) = intent.filter(|(s, k, _)| s != "cancelled" && k == "local_playlist") {
                        let _ = (intent_state, kind);
                        if c.query_row("SELECT id FROM setlists WHERE id=?", [target], |r| r.get::<_, i64>(0)).optional()?.is_none() {
                            membership = "target_missing".into();
                        } else {
                            let count: i64 = c.query_row("SELECT count(*) FROM setlist_tracks WHERE setlist_id=?", [target], |r| r.get(0))?;
                            if count > 0 {
                                c.execute("UPDATE setlist_tracks SET overlap_next_ms=0,revision=revision+1 WHERE setlist_id=? AND position=?", params_from_iter([Db::BigInt(target), Db::BigInt(count - 1)]))?;
                            }
                            entry = Some(c.query_row("INSERT INTO setlist_tracks (setlist_id,track_id,position) VALUES (?,?,?) RETURNING id", params_from_iter([Db::BigInt(target), Db::BigInt(track_id), Db::BigInt(count)]), |r| r.get(0))?);
                            c.execute("UPDATE setlists SET updated_at=? WHERE id=?", params_from_iter([ts(at), Db::BigInt(target)]))?;
                            membership = "applied".into();
                        }
                    }
                    let level: Option<String> = c.query_row("SELECT analysis_level FROM tracks WHERE id=?", [track_id], |r| r.get(0))?;
                    c.execute(
                        "UPDATE import_items SET track_id=?,state=?,load_ready=true, analysis_level=?,error_code=NULL,error_message=NULL WHERE id=?",
                        params_from_iter([Db::BigInt(track_id), Db::Text(if reused { "existing" } else { "completed" }.into()), Db::Text(level.unwrap_or_else(|| "full".into())), Db::Text(item_id.clone())]),
                    )?;
                    c.execute("UPDATE import_target_intents SET state=?,setlist_track_id=? WHERE item_id=?", params_from_iter([Db::Text(membership), entry.map_or(Db::Null, Db::BigInt), Db::Text(item_id.clone())]))?;
                    Ok(())
                })
                .map_err(db)
        })();
        if let Err(error) = outcome {
            let message: String = error.chars().take(1000).collect();
            state
                .db
                .write(|c| c.execute("UPDATE import_items SET state='failed',analysis_level='failed',error_code='processing_failed',error_message=? WHERE id=?", params_from_iter([Db::Text(message.clone()), Db::Text(item_id.clone())])).map(|_| ()).map_err(Into::into))
                .map_err(db)?;
        }
    }
    let conn = state.db.lease().map_err(db)?;
    let failed = scalar_int(&conn, "SELECT count(*) FROM import_items WHERE batch_id=? AND state='failed'", vec![Db::Text(batch_id.into())]).map_err(db)?.unwrap_or(0);
    let unfinished = scalar_int(&conn, "SELECT count(*) FROM import_items WHERE batch_id=? AND state NOT IN ('completed','existing','failed','skipped')", vec![Db::Text(batch_id.into())]).map_err(db)?.unwrap_or(0);
    drop(conn);
    let status = if unfinished > 0 { "paused" } else if failed > 0 { "completed_with_errors" } else { "completed" };
    state
        .db
        .write(|c| c.execute("UPDATE import_batches SET state=?,updated_at=? WHERE id=? AND NOT cancel_requested AND NOT paused", params_from_iter([Db::Text(status.into()), ts(clock_time()), Db::Text(batch_id.into())])).map(|_| ()).map_err(Into::into))
        .map_err(db)?;
    Ok(())
}

/// `/ws/play-imports`: a snapshot now and every 2 seconds.
pub async fn ws_play_imports(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| serve_imports(socket, state))
}

async fn serve_imports(mut socket: WebSocket, state: AppState) {
    loop {
        let worker = state.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            let active = list_batches(&worker, true).unwrap_or_default();
            let ids: Vec<Value> = active.iter().map(|b| b["id"].clone()).collect();
            let mut batches = active;
            batches.extend(list_batches(&worker, false).unwrap_or_default().into_iter().filter(|b| !ids.contains(&b["id"])).take(20));
            json!({"type": "import_queue", "batches": batches})
        })
        .await
        .unwrap_or(json!({"type": "import_queue", "batches": []}));
        if socket.send(Message::Text(snapshot.to_string().into())).await.is_err() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ------------------------------------------------------------------ MCP analysis tools

/// The MCP analysis tools (`plan/start/get/pause/resume_track_analysis`).
pub async fn mcp_tool(state: &AppState, name: &str, a: &Value) -> Result<Value, String> {
    let jobs = state.analysis.jobs.clone();
    let name = name.to_owned();
    let a = a.clone();
    tokio::task::spawn_blocking(move || {
        let limit = a["limit"].as_i64();
        let only_outdated = a["only_outdated"].as_bool().unwrap_or(true);
        match name.as_str() {
            "plan_track_analysis" => jobs.plan(ints(&a["track_ids"]), strings(&a["genres"]), strings(&a["features"]), only_outdated, limit),
            "start_track_analysis" => jobs.start(ints(&a["track_ids"]), strings(&a["genres"]), strings(&a["features"]), only_outdated, limit, a["workers"].as_i64().unwrap_or(2)),
            "get_track_analysis_status" => jobs.status(a["job_id"].as_str()),
            "pause_track_analysis" => jobs.pause(a["job_id"].as_str().unwrap_or("")),
            "resume_track_analysis" => jobs.resume(a["job_id"].as_str().unwrap_or(""), a["workers"].as_i64(), a["retry_failed"].as_bool().unwrap_or(false)),
            _ => Err(format!("Unknown tool: {name}")),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
