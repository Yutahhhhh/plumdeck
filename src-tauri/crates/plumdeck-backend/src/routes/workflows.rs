//! Library workflows (backend/api/routers/workflows.py over
//! app/services/workflow_service.py; S10): media repair plans, version
//! groups, audio presets and controller profiles, USB handoffs to rekordbox,
//! recording timelines, external recordings, and backup / restore.
//!
//! Audio files are read for identity only; nothing here renames, tags or
//! deletes them. The Play import routes come with S11.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::types::{TimeUnit, Value as Db};
use plumdeck_db::duckdb::{params_from_iter, Connection};
use plumdeck_db::repos::rows::{json_rows, py_rows, scalar_int};
use plumdeck_db::DbError;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::response::PyJson;
use crate::{server_error, AppState, HttpError};

const BACKUP_FORMAT_VERSION: i64 = 1;
const MAX_BACKUP_ENTRIES: usize = 100_000;
const MAX_BACKUP_UNCOMPRESSED: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const SENSITIVE_SETTING_WORDS: [&str; 8] = ["secret", "token", "credential", "invite", "answer", "recovery", "private", "password"];
const EXTERNAL_RECORDING_EXTENSIONS: [&str; 8] = [".wav", ".wave", ".aif", ".aiff", ".flac", ".mp3", ".ogg", ".oga"];
const ALL_TABLES: [&str; 31] = [
    "tracks", "lyrics", "setlists", "setlist_tracks", "wordplay_pairs", "track_performance_metadata", "track_grid_candidates",
    "rekordbox_sources", "rekordbox_playlists", "rekordbox_playlist_tracks", "play_sessions", "play_history", "recordings", "settings",
    "schema_info", "track_media", "media_repair_operations", "operation_journal", "track_version_groups", "track_version_members",
    "audio_presets", "controller_profiles", "controller_device_bindings", "usb_devices", "usb_exports", "recording_segments",
    "import_batches", "import_items", "import_target_intents", "track_analyses", "track_embeddings",
];
const CONTROLLER_ACTIONS: [&str; 26] = [
    "library.browse", "library.load", "deck.play", "deck.cue", "deck.sync", "deck.tempo", "deck.jog", "deck.jog_touch", "deck.nudge",
    "deck.search", "mixer.trim", "mixer.eq_low", "mixer.eq_mid", "mixer.eq_high", "mixer.channel_fader", "mixer.crossfader",
    "mixer.filter", "mixer.cue", "pad.hotcue", "pad.loop", "pad.beatjump", "pad.sampler", "loop.in", "loop.out", "loop.exit", "loop.size",
];

fn workflow_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A `ValueError` the route turns into `status` with its text.
struct Fail(String);

impl From<DbError> for Fail {
    fn from(error: DbError) -> Self {
        Fail(format!("\u{0}{error}"))
    }
}

impl From<std::io::Error> for Fail {
    fn from(error: std::io::Error) -> Self {
        Fail(error.to_string())
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T, Fail> {
    Err(Fail(message.into()))
}

type Out = Result<Value, Fail>;

fn http(status: u16, detail: impl Into<String>) -> Response {
    HttpError(ApiError::new(status, detail.into())).into_response()
}

fn answer(result: Result<Out, Response>, status: u16) -> Response {
    match result {
        Ok(Ok(value)) => json_response(&value),
        // A DbError (marked with NUL) was an unhandled exception: a 500.
        Ok(Err(Fail(message))) if message.starts_with('\u{0}') => server_error(&message.trim_start_matches('\u{0}')),
        Ok(Err(Fail(message))) => http(status, message),
        Err(response) => response,
    }
}

fn to_py(value: &Value) -> PyValue {
    plumdeck_core::compat::pyjson_loads::loads(&value.to_string()).unwrap_or(PyValue::None)
}

fn to_value(value: &PyValue) -> Value {
    serde_json::from_str(&dumps(value, &DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(Value::Null)
}

fn json_response(value: &Value) -> Response {
    PyJson(to_py(value)).into_response()
}

/// `_json`: compact, unicode, `default=str`.
fn py_json(value: &PyValue) -> String {
    dumps(value, &DumpOptions::compact_unicode_default_str()).unwrap_or_default()
}

fn ts(at: NaiveDateTime) -> Db {
    Db::Timestamp(TimeUnit::Microsecond, at.and_utc().timestamp_micros())
}

fn text_or_null(value: Option<&str>) -> Db {
    value.map_or(Db::Null, |v| Db::Text(v.to_owned()))
}

fn rows(conn: &Connection, sql: &str, params: Vec<Db>) -> Result<Vec<Value>, Fail> {
    Ok(json_rows(conn, sql, params)?.iter().map(to_value).collect())
}

fn row(conn: &Connection, sql: &str, params: Vec<Db>) -> Result<Option<Value>, Fail> {
    Ok(rows(conn, sql, params)?.into_iter().next())
}

fn exec(conn: &Connection, sql: &str, params: Vec<Db>) -> Result<usize, Fail> {
    Ok(conn.execute(sql, params_from_iter(params)).map_err(DbError::from)?)
}

fn sha256_file(path: &Path, limit: Option<u64>) -> Result<String, Fail> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if limit.is_some_and(|limit| read > limit) {
            return fail("ファイルが許可されたサイズを超えています");
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn expand_user(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => PathBuf::from(format!("{home}{rest}")),
        _ => PathBuf::from(path),
    }
}

fn mtime_ns(metadata: &std::fs::Metadata) -> i128 {
    metadata.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i128)
}

/// `_file_identity`.
fn file_identity(path: &Path, with_hash: bool) -> Result<Value, Fail> {
    let metadata = std::fs::metadata(path)?;
    Ok(json!({
        "path": path.to_string_lossy(), "size_bytes": metadata.len(), "mtime_ns": mtime_ns(&metadata) as i64,
        "sha256": if with_hash { json!(sha256_file(path, None)?) } else { Value::Null },
    }))
}

/// `_safe_real_file`.
fn safe_real_file(value: &str) -> Result<PathBuf, Fail> {
    let path = expand_user(value);
    if !path.is_absolute() {
        return fail("絶対パスを指定してください");
    }
    // Path.resolve(strict=True) raises FileNotFoundError: str() is
    // "[Errno 2] No such file or directory: '<path>'".
    let resolved = std::fs::canonicalize(&path).map_err(|error| os_error(&error, &path))?;
    if !resolved.is_file() {
        return fail("ファイルが見つかりません");
    }
    Ok(resolved)
}

/// `str(OSError)` with a filename, as Python prints it.
fn os_error(error: &std::io::Error, path: &Path) -> Fail {
    let text = error.to_string();
    match error.raw_os_error() {
        Some(code) => {
            let strerror = text.split(" (os error").next().unwrap_or(&text);
            let name = path.to_string_lossy().replace('\\', "\\\\").replace('\'', "\\'");
            Fail(format!("[Errno {code}] {strerror}: '{name}'"))
        }
        None => Fail(text),
    }
}

fn operation(conn: &Connection, id: &str, kind: &str, target: &str, state: &str, detail: &Value, progress: f64, now: NaiveDateTime) -> Result<(), Fail> {
    exec(
        conn,
        "INSERT INTO operation_journal (id,kind,target,state,progress,detail_json) VALUES (?,?,?,?,?,?) \
         ON CONFLICT (id) DO UPDATE SET state=excluded.state,progress=excluded.progress, detail_json=excluded.detail_json,updated_at=?",
        vec![Db::Text(id.into()), Db::Text(kind.into()), Db::Text(target.into()), Db::Text(state.into()), Db::Double(progress), Db::Text(py_json(&to_py(detail))), ts(now)],
    )?;
    Ok(())
}

fn uuid4() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn body(headers: &HeaderMap, bytes: &[u8]) -> Result<Value, Response> {
    match crate::body::read_body(headers, bytes)? {
        Some(value @ PyValue::Dict(_)) => Ok(to_value(&value)),
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

fn int_of(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_f64().filter(|x| x.fract() == 0.0).map(|x| x as i64)).or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

fn path_int(name: &str, value: &str) -> Result<i64, Response> {
    let mut params = crate::query::Params::from_uri(&axum::http::Uri::from_static("/"));
    let parsed = params.path_int(name, value);
    params.finish()?;
    Ok(parsed.unwrap_or_default() as i64)
}

// ------------------------------------------------------------------ summary

/// `GET /api/workflows/summary`.
pub async fn summary(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let mut out = Map::new();
            for name in ["track_media", "track_version_groups", "audio_presets", "controller_profiles", "usb_exports", "recording_segments", "import_batches"] {
                out.insert(name.into(), json!(scalar_int(&conn, &format!("SELECT count(*) FROM \"{name}\""), vec![])?.unwrap_or(0)));
            }
            Ok(Ok(Value::Object(out)))
        })
        .await;
    answer(result, 422)
}

// ------------------------------------------------------------------ media repair

/// `POST /api/workflows/media/diagnose`.
pub async fn diagnose_media(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    if let Err(response) = repair_request(&payload) {
        return response;
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    answer(state.blocking_result(move || db.write(|conn| Ok(diagnose(conn, &payload, now)))).await.and_then(|r| r.map_err(|e| server_error(&e))), 422)
}

/// `RepairPlanRequest`: list[int] | None and two str | None.
fn repair_request(payload: &Value) -> Result<(), Response> {
    match &payload["track_ids"] {
        Value::Null => {}
        Value::Array(items) => {
            if items.iter().any(|item| matches!(item, Value::Bool(_)) || int_of(item).is_none()) {
                return Err(invalid("track_ids: Input should be a valid integer"));
            }
        }
        _ => return Err(invalid("track_ids: Input should be a valid list")),
    }
    for key in ["old_root", "new_root"] {
        if !matches!(payload[key], Value::Null | Value::String(_)) {
            return Err(invalid(&format!("{key}: Input should be a valid string")));
        }
    }
    Ok(())
}

fn diagnose(conn: &Connection, payload: &Value, now: NaiveDateTime) -> Out {
    let ids: Vec<i64> = payload["track_ids"].as_array().map(|a| a.iter().filter_map(int_of).collect()).unwrap_or_default();
    let mut sql = "SELECT tracks.id, tracks.filepath, tracks.title, tracks.artist FROM tracks".to_owned();
    if !ids.is_empty() {
        sql.push_str(&format!(" WHERE tracks.id IN ({})", ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")));
    }
    sql.push_str(" ORDER BY tracks.id");
    let tracks = rows(conn, &sql, vec![])?;
    let plan_id = format!("repair-{}", uuid4());
    let old_root = payload["old_root"].as_str().filter(|s| !s.is_empty()).map(expand_user);
    let new_root = payload["new_root"].as_str().filter(|s| !s.is_empty()).map(expand_user);
    let mut by_name: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    if let Some(root) = new_root.as_ref().filter(|r| r.is_dir()) {
        let mut stack = vec![root.clone()];
        let mut seen = 0;
        'walk: while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                seen += 1;
                if seen >= 20_000 {
                    break 'walk;
                }
                let path = entry.path();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() {
                    by_name.entry(entry.file_name().to_string_lossy().into_owned()).or_default().push(path);
                }
            }
        }
    }
    let mut items = Vec::new();
    for track in &tracks {
        let id = track["id"].as_i64().unwrap_or_default();
        let filepath = track["filepath"].as_str().unwrap_or("");
        let source = expand_user(filepath);
        let available = source.is_file() && std::fs::File::open(&source).is_ok();
        let state = if available { "available" } else { "missing" };
        let mut candidate: Option<PathBuf> = match (&old_root, &new_root) {
            (Some(old), Some(new)) => source.strip_prefix(old).ok().map(|rest| new.join(rest)),
            _ => None,
        };
        let mut expected = row(conn, "SELECT * FROM track_media WHERE track_id=?", vec![Db::BigInt(id)])?.unwrap_or(json!({}));
        if available {
            let identity = file_identity(&source, true)?;
            exec(
                conn,
                "INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at) VALUES (?,?,?,?,'available',?) \
                 ON CONFLICT (track_id) DO UPDATE SET sha256=excluded.sha256,size_bytes=excluded.size_bytes, mtime_ns=excluded.mtime_ns,\
                 status='available',last_verified_at=excluded.last_verified_at, revision=track_media.revision+1",
                vec![Db::BigInt(id), Db::Text(identity["sha256"].as_str().unwrap_or("").into()), Db::BigInt(identity["size_bytes"].as_i64().unwrap_or(0)), Db::BigInt(identity["mtime_ns"].as_i64().unwrap_or(0)), ts(now)],
            )?;
            expected = identity;
        }
        if !available && !candidate.as_ref().is_some_and(|c| c.is_file()) {
            let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let size = expected["size_bytes"].as_i64().filter(|s| *s != 0);
            let sized: Vec<&PathBuf> = by_name.get(&name).map(|v| v.iter().filter(|p| size.is_none_or(|s| std::fs::metadata(p).map(|m| m.len() as i64 == s).unwrap_or(false))).collect()).unwrap_or_default();
            candidate = if let Some(hash) = expected["sha256"].as_str().filter(|h| !h.is_empty()) {
                sized.into_iter().find(|p| sha256_file(p, None).ok().as_deref() == Some(hash)).cloned()
            } else if sized.len() == 1 {
                Some(sized[0].clone())
            } else {
                // Python leaves the root-mapped candidate in place.
                candidate
            };
        }
        let (mut candidate_state, mut reason) = ("none", if available { "現在の音源を利用できます" } else { "候補を指定してください" });
        if let Some(path) = candidate.as_ref().filter(|c| c.is_file()) {
            let identity = file_identity(path, true)?;
            if expected["sha256"].as_str().is_some_and(|h| !h.is_empty() && Some(h) == identity["sha256"].as_str()) {
                (candidate_state, reason) = ("confirmed", "保存済みSHA-256と一致");
            } else {
                (candidate_state, reason) = ("needs_confirmation", "ファイル名・サイズ候補（内容の確認が必要）");
            }
        }
        items.push(json!({
            "track_id": id, "title": track["title"], "artist": track["artist"], "old_path": filepath,
            "old_revision": expected.get("revision").cloned().unwrap_or(json!(1)), "access_state": state,
            "candidate_path": candidate.map(|c| c.to_string_lossy().into_owned()), "candidate_state": candidate_state, "reason": reason,
        }));
    }
    let count = |key: &str| items.iter().filter(|i| i["access_state"] == key).count();
    let plan = json!({"id": plan_id, "state": "planned", "items": items, "summary": {"available": count("available"), "missing": count("missing")}});
    exec(conn, "INSERT INTO media_repair_operations (id,state,plan_json) VALUES (?,'planned',?)", vec![Db::Text(plan_id.clone()), Db::Text(py_json(&to_py(&plan)))])?;
    operation(conn, &plan_id, "media_repair", "tracks", "planned", &plan, 0.0, now)?;
    Ok(plan)
}

/// `POST /api/workflows/media/repairs/{plan_id}/apply`.
pub async fn apply_media_repair(State(state): State<AppState>, UrlPath(plan_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(selections) = payload["selections"].as_object().cloned() else { return invalid("selections is required") };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                conn.execute_batch("BEGIN TRANSACTION")?;
                let out = apply_repair(conn, &plan_id, &selections, now);
                conn.execute_batch(if out.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
                Ok(out)
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

fn apply_repair(conn: &Connection, plan_id: &str, selections: &Map<String, Value>, now: NaiveDateTime) -> Out {
    let Some(record) = row(conn, "SELECT * FROM media_repair_operations WHERE id=?", vec![Db::Text(plan_id.into())])? else { return fail("修復計画が見つかりません") };
    let plan: Value = serde_json::from_str(record["plan_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    let (mut changed, mut conflicts) = (Vec::new(), Vec::new());
    for item in plan["items"].as_array().cloned().unwrap_or_default() {
        let id = item["track_id"].as_i64().unwrap_or_default();
        let Some(chosen) = selections.get(&id.to_string()).and_then(Value::as_str).filter(|s| !s.is_empty()) else { continue };
        let current: Option<String> = conn.query_row("SELECT filepath FROM tracks WHERE id=?", [id], |r| r.get(0)).ok();
        if current.as_deref() != item["old_path"].as_str() {
            conflicts.push(json!({"track_id": id, "reason": "プレビュー後に参照が変更されました"}));
            continue;
        }
        let candidate = safe_real_file(chosen)?;
        let candidate_text = candidate.to_string_lossy().into_owned();
        let owner: Option<i64> = conn.query_row("SELECT id FROM tracks WHERE filepath=? LIMIT 1", [&candidate_text], |r| r.get(0)).ok();
        if owner.is_some_and(|owner| owner != id) {
            conflicts.push(json!({"track_id": id, "reason": "候補は別の曲が使用しています"}));
            continue;
        }
        let identity = file_identity(&candidate, true)?;
        let saved: Option<Option<String>> = conn.query_row("SELECT sha256 FROM track_media WHERE track_id=?", [id], |r| r.get(0)).ok();
        if saved.flatten().is_some_and(|s| !s.is_empty() && Some(s.as_str()) != identity["sha256"].as_str()) {
            conflicts.push(json!({"track_id": id, "reason": "候補の内容が保存済み音源と一致しません"}));
            continue;
        }
        exec(conn, "UPDATE tracks SET filepath=? WHERE id=?", vec![Db::Text(candidate_text.clone()), Db::BigInt(id)])?;
        exec(
            conn,
            "INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at) VALUES (?,?,?,?,'available',?) \
             ON CONFLICT(track_id) DO UPDATE SET sha256=excluded.sha256,size_bytes=excluded.size_bytes, mtime_ns=excluded.mtime_ns,\
             status='available',last_verified_at=excluded.last_verified_at, revision=track_media.revision+1",
            vec![Db::BigInt(id), Db::Text(identity["sha256"].as_str().unwrap_or("").into()), Db::BigInt(identity["size_bytes"].as_i64().unwrap_or(0)), Db::BigInt(identity["mtime_ns"].as_i64().unwrap_or(0)), ts(now)],
        )?;
        changed.push(json!({"track_id": id, "old_path": item["old_path"], "new_path": candidate_text}));
    }
    let state = if conflicts.is_empty() { "completed" } else { "completed_with_conflicts" };
    let result = json!({"changed": changed, "conflicts": conflicts, "state": state});
    exec(conn, "UPDATE media_repair_operations SET state=?,result_json=?,updated_at=? WHERE id=?", vec![Db::Text(state.into()), Db::Text(py_json(&to_py(&result))), ts(now), Db::Text(plan_id.into())])?;
    operation(conn, plan_id, "media_repair", "tracks", state, &result, 1.0, now)?;
    Ok(result)
}

/// `POST /api/workflows/media/repairs/{plan_id}/undo`.
pub async fn undo_media_repair(State(state): State<AppState>, UrlPath(plan_id): UrlPath<String>) -> Response {
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let Some(record) = row(conn, "SELECT result_json FROM media_repair_operations WHERE id=?", vec![Db::Text(plan_id.clone())])? else {
                        return fail("修復履歴が見つかりません");
                    };
                    let result: Value = serde_json::from_str(record["result_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    let (mut reverted, mut conflicts) = (Vec::new(), Vec::new());
                    for change in result["changed"].as_array().cloned().unwrap_or_default() {
                        let id = change["track_id"].as_i64().unwrap_or_default();
                        let current: Option<String> = conn.query_row("SELECT filepath FROM tracks WHERE id=?", [id], |r| r.get(0)).ok();
                        let old = change["old_path"].as_str().unwrap_or("");
                        if current.as_deref() != change["new_path"].as_str() || !Path::new(old).is_file() {
                            conflicts.push(json!({"track_id": id, "reason": "現在値または元ファイルが変化しています"}));
                            continue;
                        }
                        exec(conn, "UPDATE tracks SET filepath=? WHERE id=?", vec![Db::Text(old.into()), Db::BigInt(id)])?;
                        reverted.push(json!(id));
                    }
                    Ok(json!({"reverted": reverted, "conflicts": conflicts}))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

// ------------------------------------------------------------------ versions

fn version_group(conn: &Connection, id: i64) -> Out {
    let Some(mut group) = row(conn, "SELECT * FROM track_version_groups WHERE id=?", vec![Db::BigInt(id)])? else { return fail("バージョングループが見つかりません") };
    let preferred = group["preferred_track_id"].as_i64().map_or(Db::Null, Db::BigInt);
    group["members"] = Value::Array(rows(
        conn,
        "SELECT m.*,t.title,t.artist,t.filepath,t.duration,t.bpm,t.key FROM track_version_members m JOIN tracks t ON t.id=m.track_id \
         WHERE m.group_id=? ORDER BY (m.track_id=?) DESC,m.track_id",
        vec![Db::BigInt(id), preferred],
    )?);
    Ok(group)
}

fn version_for_track(conn: &Connection, track: i64) -> Result<Option<Value>, Fail> {
    match scalar_int(conn, "SELECT group_id FROM track_version_members WHERE track_id=?", vec![Db::BigInt(track)])? {
        Some(group) => Ok(Some(version_group(conn, group)?)),
        None => Ok(None),
    }
}

/// `POST /api/workflows/version-groups`.
pub async fn create_version_group(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(ids) = payload["track_ids"].as_array().map(|a| a.iter().filter_map(int_of).collect::<Vec<_>>()) else { return invalid("track_ids is required") };
    if payload["name"].as_str().is_some_and(|n| n.chars().count() > 200) {
        return invalid("name is too long");
    }
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let mut unique: Vec<i64> = Vec::new();
                    for id in ids {
                        if !unique.contains(&id) {
                            unique.push(id);
                        }
                    }
                    if unique.len() < 2 {
                        return fail("2曲以上を選択してください");
                    }
                    let found = scalar_int(conn, &format!("SELECT count(*) FROM tracks WHERE id IN ({})", unique.iter().map(i64::to_string).collect::<Vec<_>>().join(",")), vec![])?.unwrap_or(0);
                    if found as usize != unique.len() {
                        return fail("曲が見つかりません");
                    }
                    for id in &unique {
                        if scalar_int(conn, "SELECT track_id FROM track_version_members WHERE track_id=?", vec![Db::BigInt(*id)])?.is_some() {
                            return fail("選択曲の一部は既に別バージョングループに属しています");
                        }
                    }
                    let group = scalar_int(conn, "INSERT INTO track_version_groups (name,preferred_track_id) VALUES (?,?) RETURNING id", vec![text_or_null(payload["name"].as_str()), Db::BigInt(unique[0])])?.unwrap_or(0);
                    for id in &unique {
                        let label = &payload["labels"][id.to_string()];
                        exec(
                            conn,
                            "INSERT INTO track_version_members (group_id,track_id,version_label,content_label,note) VALUES (?,?,?,?,?)",
                            vec![
                                Db::BigInt(group),
                                Db::BigInt(*id),
                                Db::Text(label["version_label"].as_str().unwrap_or("Original").into()),
                                Db::Text(label["content_label"].as_str().unwrap_or("Unknown").into()),
                                text_or_null(label["note"].as_str()),
                            ],
                        )?;
                    }
                    version_group(conn, group)
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `GET /api/workflows/tracks/{track_id}/versions`.
pub async fn versions_for_track(State(state): State<AppState>, UrlPath(track_id): UrlPath<String>) -> Response {
    let id = match path_int("track_id", &track_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state.blocking_result(move || db.lease().map_err(Fail::from).and_then(|conn| version_for_track(&conn, id)).map(|g| g.unwrap_or(Value::Null))).await;
    answer(result, 422)
}

/// `PATCH /api/workflows/version-groups/{group_id}`.
pub async fn update_version_group(State(state): State<AppState>, UrlPath(group_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let id = match path_int("group_id", &group_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(revision) = int_of(&payload["revision"]) else { return invalid("revision is required") };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let current = version_group(conn, id)?;
                    if current["revision"].as_i64() != Some(revision) {
                        return fail("別の画面でグループが更新されました");
                    }
                    let members: BTreeSet<i64> = current["members"].as_array().map(|m| m.iter().filter_map(|x| x["track_id"].as_i64()).collect()).unwrap_or_default();
                    let preferred = int_of(&payload["preferred_track_id"]);
                    if preferred.is_some_and(|p| !members.contains(&p)) {
                        return fail("優先版は現在のメンバーから選択してください");
                    }
                    exec(
                        conn,
                        "UPDATE track_version_groups SET name=coalesce(?,name), preferred_track_id=coalesce(?,preferred_track_id),revision=revision+1, updated_at=? WHERE id=?",
                        vec![text_or_null(payload["name"].as_str()), preferred.map_or(Db::Null, Db::BigInt), ts(now), Db::BigInt(id)],
                    )?;
                    for member in payload["members"].as_array().cloned().unwrap_or_default() {
                        let track = int_of(&member["track_id"]).unwrap_or(-1);
                        if !members.contains(&track) {
                            return fail("グループ外の曲は更新できません");
                        }
                        exec(
                            conn,
                            "UPDATE track_version_members SET version_label=?,content_label=?,note=? WHERE group_id=? AND track_id=?",
                            vec![
                                Db::Text(member["version_label"].as_str().unwrap_or("Original").into()),
                                Db::Text(member["content_label"].as_str().unwrap_or("Unknown").into()),
                                text_or_null(member["note"].as_str()),
                                Db::BigInt(id),
                                Db::BigInt(track),
                            ],
                        )?;
                    }
                    version_group(conn, id)
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `PATCH /api/workflows/setlist-entries/{entry_id}/version`.
pub async fn swap_setlist_version(State(state): State<AppState>, UrlPath(entry_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let id = match path_int("entry_id", &entry_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (Some(new_track), Some(revision)) = (int_of(&payload["new_track_id"]), int_of(&payload["revision"])) else { return invalid("new_track_id and revision are required") };
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let Some(entry) = row(conn, "SELECT * FROM setlist_tracks WHERE id=?", vec![Db::BigInt(id)])? else { return fail("セットの行が見つかりません") };
                    if entry["revision"].as_i64() != Some(revision) {
                        return fail("セットの行が別の画面で更新されました");
                    }
                    let group = version_for_track(conn, entry["track_id"].as_i64().unwrap_or_default())?;
                    let allowed = group.as_ref().is_some_and(|g| g["members"].as_array().is_some_and(|m| m.iter().any(|x| x["track_id"].as_i64() == Some(new_track))));
                    if !allowed {
                        return fail("同じバージョングループの曲だけに差し替えられます");
                    }
                    if scalar_int(conn, "SELECT id FROM tracks WHERE id=?", vec![Db::BigInt(new_track)])?.is_none() {
                        return fail("差し替え先の曲が見つかりません");
                    }
                    exec(
                        conn,
                        "UPDATE setlist_tracks SET track_id=?,in_ms=0,out_ms=NULL,playback_rate=1,extra_duration_ms=0,overlap_next_ms=0,wordplay_json=NULL,revision=revision+1 WHERE id=?",
                        vec![Db::BigInt(new_track), Db::BigInt(id)],
                    )?;
                    let position = entry["position"].as_i64().unwrap_or(0);
                    exec(
                        conn,
                        "UPDATE setlist_tracks SET overlap_next_ms=0,revision=revision+1 WHERE id=(SELECT id FROM setlist_tracks WHERE setlist_id=? AND position=? LIMIT 1)",
                        vec![Db::BigInt(entry["setlist_id"].as_i64().unwrap_or(0)), Db::BigInt(position - 1)],
                    )?;
                    Ok(json!({"entry_id": id, "track_id": new_track, "revision": revision + 1}))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

// ------------------------------------------------------------------ presets

const DDJ400_PROFILE: &str = include_str!("ddj400_profile.json");

fn validate_audio(config: &Value) -> Result<(), Fail> {
    let rate = &config["sample_rate"];
    let buffer = &config["buffer_size"];
    if !(rate.is_null() || rate.as_i64() == Some(44100)) || !(buffer.is_null() || buffer.as_i64() == Some(256)) {
        return fail("現在の音声ホストで適用できるのは44,100 Hz / 256 framesのみです");
    }
    let channels = config["master"]["channels"].as_array().cloned().unwrap_or_default();
    let cue = config["cue"]["channels"].as_array().cloned();
    for (route, name) in [(Some(channels.clone()), "MASTER"), (cue.clone(), "CUE")] {
        let Some(route) = route else { continue };
        let valid = route.len() == 2 && route.iter().all(|v| v.as_i64().is_some_and(|n| n >= 0) && v.is_i64()) && route[0] != route[1];
        if !valid {
            return fail(format!("{name}は異なる2つの有効なチャンネルで指定してください"));
        }
    }
    if let Some(cue) = cue.filter(|c| !c.is_empty()) {
        if cue.iter().any(|c| channels.contains(c)) {
            return fail("MASTERとCUEのチャンネルは重複できません");
        }
    }
    Ok(())
}

fn audio_list(conn: &Connection) -> Result<Vec<Value>, Fail> {
    let mut list = rows(conn, "SELECT * FROM audio_presets ORDER BY updated_at DESC", vec![])?;
    for item in &mut list {
        if let Some(map) = item.as_object_mut() {
            let config = map.remove("config_json").and_then(|c| c.as_str().and_then(|s| serde_json::from_str(s).ok())).unwrap_or(Value::Null);
            map.insert("config".into(), config);
        }
    }
    Ok(list)
}

/// `GET /api/workflows/audio-presets`.
pub async fn audio_presets(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    answer(state.blocking_result(move || db.lease().map_err(Fail::from).and_then(|conn| audio_list(&conn)).map(Value::Array)).await, 422)
}

/// `PUT /api/workflows/audio-presets`.
pub async fn save_audio_preset(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    if !payload["name"].is_string() || !payload["config"].is_object() {
        return invalid("name and config are required");
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    validate_audio(&payload["config"])?;
                    let id = payload["id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("audio-{}", uuid4()));
                    let current = scalar_int(conn, "SELECT revision FROM audio_presets WHERE id=?", vec![Db::Text(id.clone())])?;
                    if let Some(current) = current {
                        if int_of(&payload["revision"]) != Some(current) {
                            return fail("プリセットが別の画面で更新されました");
                        }
                    }
                    let name: String = payload["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("名称未設定").chars().take(200).collect();
                    exec(
                        conn,
                        "INSERT INTO audio_presets (id,name,config_json) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,\
                         config_json=excluded.config_json, revision=audio_presets.revision+1,updated_at=?",
                        vec![Db::Text(id.clone()), Db::Text(name), Db::Text(py_json(&to_py(&payload["config"]))), ts(now)],
                    )?;
                    Ok(audio_list(conn)?.into_iter().find(|p| p["id"] == id.as_str()).unwrap_or(Value::Null))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `DELETE /api/workflows/audio-presets/{preset_id}`.
pub async fn delete_audio_preset(State(state): State<AppState>, UrlPath(preset_id): UrlPath<String>) -> Response {
    let db = state.db.clone();
    let id = preset_id.clone();
    let result = state.blocking(move || db.write(|conn| Ok(conn.query_row("DELETE FROM audio_presets WHERE id=? RETURNING id", [&id], |r| r.get::<_, String>(0)).is_ok()))).await;
    match result {
        Ok(true) => json_response(&json!({"deleted": preset_id})),
        Ok(false) => http(404, "プリセットが見つかりません"),
        Err(response) => response,
    }
}

fn controllers(conn: &Connection) -> Result<Vec<Value>, Fail> {
    let mut definitions: Vec<Value> = rows(conn, "SELECT * FROM controller_profiles ORDER BY built_in DESC,name", vec![])?
        .into_iter()
        .map(|row| {
            let mut definition: Value = serde_json::from_str(row["definition_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
            definition["revision"] = row["revision"].clone();
            definition
        })
        .collect();
    let builtin: Value = serde_json::from_str(DDJ400_PROFILE).unwrap_or(json!({}));
    if !definitions.iter().any(|d| d["id"] == builtin["id"]) {
        definitions.insert(0, builtin);
    }
    Ok(definitions)
}

fn validate_profile(definition: &Value) -> Result<(), Fail> {
    if definition["schemaVersion"].as_i64() != Some(1) || !["generic-midi", "ddj1000", "ddj400"].contains(&definition["adapterId"].as_str().unwrap_or("")) {
        return fail("未対応のコントローラープロファイルです");
    }
    let Some(bindings) = definition["bindings"].as_array().filter(|b| b.len() <= 512) else { return fail("MIDI割り当て数が不正です") };
    let mut ids = BTreeSet::new();
    let mut inputs = BTreeSet::new();
    for binding in bindings {
        let id = binding["id"].to_string();
        if ids.contains(&id) || !CONTROLLER_ACTIONS.contains(&binding["actionId"].as_str().unwrap_or("")) {
            return fail("重複IDまたは未許可の操作があります");
        }
        ids.insert(id);
        let source = &binding["input"];
        let kind = source["kind"].as_str().unwrap_or("");
        let channel = int_of(&source["channel"]).unwrap_or(-1);
        if !["note", "cc", "cc14", "pitchbend"].contains(&kind) || !(0..=15).contains(&channel) {
            return fail("MIDI入力の形式が不正です");
        }
        let number = int_of(&source["number"]).unwrap_or(-1);
        if !(0..=if kind == "cc14" { 31 } else { 127 }).contains(&number) {
            return fail("MIDI番号が範囲外です");
        }
        if !inputs.insert((kind.to_owned(), channel, number)) {
            return fail("同じMIDI入力が複数の操作へ割り当てられています");
        }
        if !(binding["deck"].is_null() || ["A", "B", "C", "D"].contains(&binding["deck"].as_str().unwrap_or("?"))) {
            return fail("デッキ指定が不正です");
        }
    }
    Ok(())
}

/// `GET /api/workflows/controller-profiles`.
pub async fn controller_profiles(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    answer(state.blocking_result(move || db.lease().map_err(Fail::from).and_then(|conn| controllers(&conn)).map(Value::Array)).await, 422)
}

/// `PUT /api/workflows/controller-profiles`.
pub async fn save_controller_profile(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    if py_json(&to_py(&payload)).len() > 512 * 1024 {
                        return fail("プロファイルが大きすぎます");
                    }
                    validate_profile(&payload)?;
                    let mut id = payload["id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("controller-{}", uuid4()));
                    if id.starts_with("builtin-") {
                        id = format!("controller-{}", uuid4());
                    }
                    let mut definition = payload.clone();
                    definition["id"] = json!(id);
                    let current = row(conn, "SELECT built_in,revision FROM controller_profiles WHERE id=?", vec![Db::Text(id.clone())])?;
                    if let Some(current) = &current {
                        if current["built_in"].as_bool().unwrap_or(false) {
                            return fail("組込プリセットは複製して編集してください");
                        }
                        if definition["revision"] != current["revision"] {
                            return fail("プロファイルが別の画面で更新されました");
                        }
                    }
                    definition.as_object_mut().map(|m| m.remove("revision"));
                    let name: String = definition["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("MIDI mapping").chars().take(200).collect();
                    exec(
                        conn,
                        "INSERT INTO controller_profiles (id,name,adapter_id,definition_json) VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET \
                         name=excluded.name,adapter_id=excluded.adapter_id, definition_json=excluded.definition_json,revision=controller_profiles.revision+1, updated_at=?",
                        vec![Db::Text(id.clone()), Db::Text(name), Db::Text(definition["adapterId"].as_str().unwrap_or("").into()), Db::Text(py_json(&to_py(&definition))), ts(now)],
                    )?;
                    definition["revision"] = json!(scalar_int(conn, "SELECT revision FROM controller_profiles WHERE id=?", vec![Db::Text(id)])?.unwrap_or(1));
                    Ok(definition)
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 422)
}

// ------------------------------------------------------------------ USB handoff

/// `_tempo_markers`: (start ms, BPM, beat in bar, beats per bar).
fn tempo_markers(grid: &Value) -> Vec<(f64, f64, i64, i64)> {
    let (Some(bpm), Some(first)) = (grid["bpm"].as_f64(), grid["first_beat_ms"].as_f64()) else { return Vec::new() };
    let bar = int_of(&grid["beats_per_bar"]).filter(|b| *b != 0).unwrap_or(4);
    if !(bpm.is_finite() && bpm > 0.0 && first.is_finite() && (1..=16).contains(&bar)) {
        return Vec::new();
    }
    let numbers: Vec<i64> = grid["beat_numbers"].as_array().map(|a| a.iter().filter_map(int_of).collect()).unwrap_or_default();
    let number_at = |index: usize| numbers.get(index).copied().unwrap_or(index as i64 % bar + 1);
    let constant = || {
        let period = 60_000.0 / bpm;
        let skipped = (-first / period).ceil().max(0.0);
        vec![(first + skipped * period, bpm, (number_at(0) - 1 + skipped as i64).rem_euclid(bar) + 1, bar)]
    };
    let times: Vec<f64> = grid["beat_times_ms"].as_array().map(|a| a.iter().take(100_000).filter_map(Value::as_f64).collect()).unwrap_or_default();
    let Some(mut start) = times.iter().position(|v| *v >= 0.0) else { return constant() };
    if times.len() < 2 || start == times.len() - 1 {
        return constant();
    }
    if times.windows(2).any(|w| !(w[1] - w[0]).is_finite() || w[1] <= w[0]) {
        return constant();
    }
    let mut markers = Vec::new();
    let mut end = start + 1;
    while end < times.len() {
        while end + 1 < times.len() {
            let period = (times[end] - times[start]) / (end - start) as f64;
            let predicted = times[start] + (end + 1 - start) as f64 * period;
            if (times[end + 1] - predicted).abs() > 10.0 {
                break;
            }
            end += 1;
        }
        let period = (times[end] - times[start]) / (end - start) as f64;
        markers.push((times[start], 60_000.0 / period, number_at(start), bar));
        start = end;
        end += 1;
    }
    let tempos: Vec<f64> = markers.iter().map(|m| m.1).collect();
    let (max, min) = (tempos.iter().cloned().fold(f64::MIN, f64::max), tempos.iter().cloned().fold(f64::MAX, f64::min));
    if max / min > 1.5 {
        return constant();
    }
    markers
}

fn get(pairs: &PyValue, key: &str) -> PyValue {
    match pairs {
        PyValue::Dict(items) => items.iter().find(|(k, _)| matches!(k, PyValue::Str(s) if s == key)).map(|(_, v)| v.clone()).unwrap_or(PyValue::None),
        _ => PyValue::None,
    }
}

fn set(pairs: &mut PyValue, key: &str, value: PyValue) {
    if let PyValue::Dict(items) = pairs {
        match items.iter_mut().find(|(k, _)| matches!(k, PyValue::Str(s) if s == key)) {
            Some(slot) => slot.1 = value,
            None => items.push((PyValue::Str(key.into()), value)),
        }
    }
}

fn py_str(value: &PyValue) -> String {
    match value {
        PyValue::Str(s) => s.clone(),
        PyValue::None => String::new(),
        other => to_value(other).to_string(),
    }
}

fn py_f64(value: &PyValue) -> Option<f64> {
    match value {
        PyValue::Float(x) => Some(*x),
        PyValue::Int(n) => Some(*n as f64),
        _ => None,
    }
}

/// `_snapshot`: the setlist, its entries with file identities, the timing and a hash.
fn snapshot(conn: &Connection, setlist_id: i64) -> Result<PyValue, Fail> {
    let Some(setlist) = py_rows(conn, "SELECT * FROM setlists WHERE id=?", vec![Db::BigInt(setlist_id)])?.into_iter().next() else { return fail("セットリストが見つかりません") };
    let mut entries = py_rows(
        conn,
        "SELECT st.id AS entry_id,st.position,st.track_id,st.in_ms,st.out_ms,st.playback_rate, st.extra_duration_ms,st.overlap_next_ms,st.revision,\
         t.filepath,t.title,t.artist,t.album, t.genre,t.bpm,t.duration,t.key,m.sha256,m.size_bytes,pm.cue_points_json, pm.loops_json,pm.beat_grid_json,\
         pm.revision AS metadata_revision FROM setlist_tracks st JOIN tracks t ON t.id=st.track_id LEFT JOIN track_media m ON m.track_id=t.id \
         LEFT JOIN track_performance_metadata pm ON pm.track_id=t.id WHERE st.setlist_id=? ORDER BY st.position,st.id",
        vec![Db::BigInt(setlist_id)],
    )?;
    let missing: Vec<String> = entries.iter().filter(|e| !Path::new(&py_str(&get(e, "filepath"))).is_file()).map(|e| py_str(&get(e, "track_id"))).collect();
    if !missing.is_empty() {
        return fail(format!("音源が見つからない曲があります: [{}]", missing.join(", ")));
    }
    let mut identities: BTreeMap<String, Value> = BTreeMap::new();
    for entry in &mut entries {
        let path = py_str(&get(entry, "filepath"));
        if !identities.contains_key(&path) {
            identities.insert(path.clone(), file_identity(Path::new(&path), true)?);
        }
        let identity = &identities[&path];
        set(entry, "snapshot_sha256", PyValue::Str(identity["sha256"].as_str().unwrap_or("").into()));
        set(entry, "snapshot_size_bytes", PyValue::Int(identity["size_bytes"].as_i64().unwrap_or(0).into()));
    }
    let timed: Vec<plumdeck_domain::set_duration::Entry> = entries
        .iter()
        .map(|e| plumdeck_domain::set_duration::Entry {
            duration: py_f64(&get(e, "duration")),
            in_ms: get(e, "in_ms"),
            out_ms: get(e, "out_ms"),
            playback_rate: get(e, "playback_rate"),
            extra_duration_ms: get(e, "extra_duration_ms"),
            overlap_next_ms: get(e, "overlap_next_ms"),
        })
        .collect();
    let durations: Vec<PyValue> = timed.iter().map(|e| plumdeck_domain::set_duration::entry_duration_ms(e).map_or(PyValue::None, PyValue::Float)).collect();
    let total = plumdeck_domain::set_duration::set_duration_ms(&timed).map_err(|e| Fail(format!("{e:?}")))?;
    let timing = PyValue::Dict(vec![
        (PyValue::Str("planned_duration_ms".into()), PyValue::Float(total)),
        (PyValue::Str("unknown_entries".into()), PyValue::Int(0)),
        (PyValue::Str("entry_duration_ms".into()), PyValue::List(durations)),
    ]);
    let mut snapshot = PyValue::Dict(vec![
        (PyValue::Str("schema_version".into()), PyValue::Int(1)),
        (PyValue::Str("serializer_version".into()), PyValue::Int(1)),
        (PyValue::Str("setlist".into()), setlist),
        (PyValue::Str("entries".into()), PyValue::List(entries)),
        (PyValue::Str("timing".into()), timing),
    ]);
    let hash = hex(&Sha256::digest(py_json(&snapshot).as_bytes()));
    set(&mut snapshot, "hash", PyValue::Str(hash));
    Ok(snapshot)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\n', "&#10;").replace('\r', "&#13;").replace('\t', "&#09;")
}

fn location(path: &str) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let text = resolved.to_string_lossy().replace('\\', "/");
    let encoded: String = text
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn element(name: &str, attrs: &[(&str, String)], indent: usize, close: bool) -> String {
    let mut out = format!("{}<{name}", "  ".repeat(indent));
    for (key, value) in attrs {
        out.push_str(&format!(" {key}=\"{}\"", xml_escape(value)));
    }
    out.push_str(if close { " />\n" } else { ">\n" });
    out
}

/// `_xml`: a rekordbox XML with the collection and one playlist.
fn rekordbox_xml(snapshot: &PyValue) -> (Vec<u8>, Vec<String>) {
    let PyValue::List(entries) = get(snapshot, "entries") else { return (Vec::new(), Vec::new()) };
    let unique: BTreeSet<String> = entries.iter().map(|e| py_str(&get(e, "track_id"))).collect();
    let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n<DJ_PLAYLISTS Version=\"1.0.0\">\n");
    out.push_str(&element("PRODUCT", &[("Name", "plumdeck".into()), ("Version", "1".into()), ("Company", "plumdeck".into())], 1, true));
    out.push_str(&element("COLLECTION", &[("Entries", unique.len().to_string())], 1, false));
    let mut limitations = Vec::new();
    let mut by_track: BTreeMap<String, usize> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let index = index + 1;
        let track = py_str(&get(entry, "track_id"));
        if by_track.contains_key(&track) {
            continue;
        }
        by_track.insert(track, index);
        let title = py_str(&get(entry, "title"));
        let mut attrs = vec![("TrackID", index.to_string()), ("Name", title.clone()), ("Artist", py_str(&get(entry, "artist"))), ("Location", location(&py_str(&get(entry, "filepath"))))];
        for (key, attr) in [("album", "Album"), ("genre", "Genre"), ("key", "Tonality")] {
            let value = py_str(&get(entry, key));
            if !value.is_empty() {
                attrs.push((attr, value));
            }
        }
        if let Some(bpm) = py_f64(&get(entry, "bpm")).filter(|b| b.is_finite() && *b > 0.0) {
            attrs.push(("AverageBpm", format!("{bpm:.2}")));
        }
        if let Some(duration) = py_f64(&get(entry, "duration")) {
            attrs.push(("TotalTime", plumdeck_core::compat::pyfmt::round_to_int(duration).unwrap_or(0).max(0).to_string()));
        }
        let mut children = String::new();
        let cues: Value = serde_json::from_str(&py_str(&get(entry, "cue_points_json"))).unwrap_or(json!([]));
        for cue in cues.as_array().cloned().unwrap_or_default() {
            let slot = int_of(&cue["slot"]).unwrap_or(-1);
            if slot > 7 {
                limitations.push(format!("{title}: Hot Cue {} はXML転送対象外", slot + 1));
                continue;
            }
            let position = cue["position_ms"].as_f64();
            if slot < 0 || !position.is_some_and(|p| p.is_finite() && p >= 0.0) {
                limitations.push(format!("{title}: 不正なCueを除外"));
                continue;
            }
            let name = cue["label"].as_str().filter(|l| !l.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("Hot Cue {}", (b'A' + slot as u8) as char));
            children.push_str(&element("POSITION_MARK", &[("Name", name), ("Type", "0".into()), ("Start", format!("{:.6}", position.unwrap_or(0.0) / 1000.0)), ("Num", slot.to_string())], 3, true));
        }
        let loops: Value = serde_json::from_str(&py_str(&get(entry, "loops_json"))).unwrap_or(json!([]));
        for item in loops.as_array().cloned().unwrap_or_default() {
            let (Some(start), Some(end)) = (item["start_ms"].as_f64(), item["end_ms"].as_f64()) else {
                limitations.push(format!("{title}: 不正なLoopを除外"));
                continue;
            };
            if end <= start {
                limitations.push(format!("{title}: 不正なLoopを除外"));
                continue;
            }
            let name = item["label"].as_str().filter(|l| !l.is_empty()).unwrap_or("Memory Loop").to_owned();
            children.push_str(&element("POSITION_MARK", &[("Name", name), ("Type", "4".into()), ("Start", format!("{:.6}", start / 1000.0)), ("End", format!("{:.6}", end / 1000.0)), ("Num", "-1".into())], 3, true));
        }
        let grid: Value = serde_json::from_str(&py_str(&get(entry, "beat_grid_json"))).unwrap_or(Value::Null);
        let markers = tempo_markers(&grid);
        if !markers.is_empty() {
            for (time, bpm, number, bar) in markers {
                children.push_str(&element("TEMPO", &[("Inizio", format!("{:.6}", time / 1000.0)), ("Bpm", format!("{bpm:.2}")), ("Metro", format!("{bar}/4")), ("Battito", number.to_string())], 3, true));
            }
        } else if let Some(bpm) = py_f64(&get(entry, "bpm")).filter(|b| *b != 0.0) {
            children.push_str(&element("TEMPO", &[("Inizio", "0.000000".into()), ("Bpm", format!("{bpm:.2}")), ("Metro", "4/4".into()), ("Battito", "1".into())], 3, true));
        }
        if children.is_empty() {
            out.push_str(&element("TRACK", &attrs, 2, true));
        } else {
            out.push_str(&element("TRACK", &attrs, 2, false));
            out.push_str(&children);
            out.push_str("    </TRACK>\n");
        }
    }
    out.push_str("  </COLLECTION>\n  <PLAYLISTS>\n");
    out.push_str(&element("NODE", &[("Type", "0".into()), ("Name", "ROOT".into()), ("Count", "1".into())], 2, false));
    let name = py_str(&get(&get(snapshot, "setlist"), "name"));
    out.push_str(&element("NODE", &[("Type", "1".into()), ("Name", name), ("KeyType", "0".into()), ("Entries", entries.len().to_string())], 3, entries.is_empty()));
    for entry in &entries {
        out.push_str(&element("TRACK", &[("Key", by_track[&py_str(&get(entry, "track_id"))].to_string())], 4, true));
    }
    if !entries.is_empty() {
        out.push_str("      </NODE>\n");
    }
    out.push_str("    </NODE>\n  </PLAYLISTS>\n</DJ_PLAYLISTS>");
    (out.into_bytes(), limitations)
}

fn user_data_dir(state: &AppState) -> PathBuf {
    state.db.path().parent().map(Path::to_path_buf).unwrap_or_default()
}

fn persist_handoff(conn: &Connection, mut snapshot: PyValue, device: Option<&str>, root_dir: &Path, now: NaiveDateTime) -> Out {
    if let Some(device) = device.filter(|d| !d.is_empty()) {
        let connected: Option<Option<bool>> = conn.query_row("SELECT connected FROM usb_devices WHERE id=?", [device], |r| r.get(0)).ok();
        if connected.flatten() != Some(true) {
            return fail("選択したUSB媒体が接続されていません");
        }
    }
    let (xml, limitations) = rekordbox_xml(&snapshot);
    let export_id = format!("usb-{}", uuid4());
    let root = root_dir.join("usb-handoffs").join(&export_id);
    std::fs::create_dir_all(root.parent().unwrap_or(Path::new(".")))?;
    std::fs::create_dir(&root)?;
    let xml_path = root.join("plumdeck-rekordbox.xml");
    std::fs::write(&xml_path, xml)?;
    set(&mut snapshot, "limitations", PyValue::List(limitations.iter().cloned().map(PyValue::Str).collect()));
    let pretty = DumpOptions { ensure_ascii: false, indent: Some(2), default_str: true, ..DumpOptions::default() };
    std::fs::write(root.join("manifest.json"), dumps(&snapshot, &pretty).unwrap_or_default())?;
    let hash = py_str(&get(&snapshot, "hash"));
    let setlist_id = py_str(&get(&get(&snapshot, "setlist"), "id"));
    exec(
        conn,
        "INSERT INTO usb_exports (id,usb_device_id,setlist_id,state,snapshot_hash,snapshot_json,handoff_path) VALUES (?,?,?,'awaiting_rekordbox',?,?,?)",
        vec![Db::Text(export_id.clone()), text_or_null(device), Db::BigInt(setlist_id.parse().unwrap_or(0)), Db::Text(hash.clone()), Db::Text(py_json(&snapshot)), Db::Text(root.to_string_lossy().into_owned())],
    )?;
    operation(conn, &export_id, "usb_handoff", &setlist_id, "awaiting_rekordbox", &json!({"path": root.to_string_lossy()}), 1.0, now)?;
    Ok(json!({
        "id": export_id, "state": "awaiting_rekordbox", "handoff_path": root.to_string_lossy(), "xml_path": xml_path.to_string_lossy(),
        "snapshot_hash": hash, "limitations": limitations,
        "instructions": ["rekordboxでXMLの参照先を選択", "プレイリストをコレクションへ取り込み", "rekordboxから対象USBへExport", "書き出し完了後にplumdeckでUSBを確認"],
    }))
}

/// `POST /api/workflows/usb/handoffs`.
pub async fn create_usb_handoff(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(setlist) = int_of(&payload["setlist_id"]) else { return invalid("setlist_id is required") };
    let root = user_data_dir(&state);
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| Ok(snapshot(conn, setlist).and_then(|s| persist_handoff(conn, s, payload["usb_device_id"].as_str(), &root, now))))
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `POST /api/workflows/usb/handoffs/{export_id}/duplicate`.
pub async fn duplicate_usb_handoff(State(state): State<AppState>, UrlPath(export_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let root = user_data_dir(&state);
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let stored: Option<String> = conn.query_row("SELECT snapshot_json FROM usb_exports WHERE id=?", [&export_id], |r| r.get(0)).ok();
                    let Some(stored) = stored else { return fail("元のUSB受け渡しが見つかりません") };
                    let snapshot = plumdeck_core::compat::pyjson_loads::loads(&stored).map_err(|e| Fail(e.to_string()))?;
                    persist_handoff(conn, snapshot, payload["usb_device_id"].as_str(), &root, now)
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `GET /api/workflows/usb/handoffs`.
pub async fn list_usb_handoffs(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            let conn = db.lease().map_err(Fail::from)?;
            let mut list = rows(&conn, "SELECT id,usb_device_id,setlist_id,state,snapshot_hash,handoff_path,verification_json,created_at,updated_at FROM usb_exports ORDER BY created_at DESC", vec![])?;
            for item in &mut list {
                let current = snapshot(&conn, item["setlist_id"].as_i64().unwrap_or(0)).ok().map(|s| py_str(&get(&s, "hash")));
                let stale = current.as_deref() != item["snapshot_hash"].as_str();
                item["current_snapshot_hash"] = json!(current);
                item["stale"] = json!(stale);
                if stale {
                    item["state"] = json!("stale");
                }
            }
            Ok(Value::Array(list))
        })
        .await;
    answer(result, 422)
}

/// `removable_devices.devices()`: mounted external volumes (macOS via diskutil).
fn removable_devices() -> Vec<Value> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/Volumes") else { return out };
    let mut mounts: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    mounts.sort();
    for mount in mounts {
        let Ok(output) = std::process::Command::new("/usr/sbin/diskutil").args(["info"]).arg(&mount).output() else { continue };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let field = |name: &str| text.lines().find_map(|l| l.trim().strip_prefix(&format!("{name}:")).map(|v| v.trim().to_owned()));
        if field("Device Location").as_deref() == Some("Internal") || field("Mount Point").is_none() {
            continue;
        }
        let bytes = |name: &str| field(name).and_then(|v| v.split('(').nth(1).and_then(|s| s.split_whitespace().next()).and_then(|n| n.parse::<i64>().ok()));
        let id = field("Volume UUID").or_else(|| field("Disk / Partition UUID")).or_else(|| field("Device Identifier")).unwrap_or_default();
        out.push(json!({
            "id": id, "device_identifier": field("Device Identifier"), "volume_uuid": field("Volume UUID"),
            "label": field("Volume Name").unwrap_or_else(|| mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            "mount_path": field("Mount Point"), "filesystem": field("Type (Bundle)"),
            "capacity_bytes": bytes("Disk Size").or_else(|| bytes("Volume Total Space")), "free_bytes": bytes("Volume Free Space").or_else(|| bytes("Container Free Space")),
            "read_only": field("Volume Read-Only").is_some_and(|v| v.starts_with("Yes")), "connected": true,
        }));
    }
    out
}

fn sync_devices(conn: &Connection, now: NaiveDateTime) -> Result<Vec<Value>, Fail> {
    let devices = removable_devices();
    for device in &devices {
        let s = |k: &str| text_or_null(device[k].as_str());
        let n = |k: &str| device[k].as_i64().map_or(Db::Null, Db::BigInt);
        exec(
            conn,
            "INSERT INTO usb_devices (id,device_identifier,volume_uuid,label,mount_path,filesystem, capacity_bytes,free_bytes,read_only,connected,last_seen_at) \
             VALUES (?,?,?,?,?,?,?,?,?,true,?) ON CONFLICT(id) DO UPDATE SET label=excluded.label,mount_path=excluded.mount_path, filesystem=excluded.filesystem,\
             capacity_bytes=excluded.capacity_bytes, free_bytes=excluded.free_bytes,read_only=excluded.read_only,connected=true,last_seen_at=excluded.last_seen_at",
            vec![s("id"), s("device_identifier"), s("volume_uuid"), s("label"), s("mount_path"), s("filesystem"), n("capacity_bytes"), n("free_bytes"), Db::Boolean(device["read_only"].as_bool().unwrap_or(false)), ts(now)],
        )?;
    }
    if devices.is_empty() {
        exec(conn, "UPDATE usb_devices SET connected=false", vec![])?;
    } else {
        let marks = vec!["?"; devices.len()].join(",");
        exec(conn, &format!("UPDATE usb_devices SET connected=false WHERE id NOT IN ({marks})"), devices.iter().map(|d| text_or_null(d["id"].as_str())).collect())?;
    }
    Ok(devices)
}

/// `GET /api/workflows/usb/devices`.
pub async fn list_usb_devices(State(state): State<AppState>) -> Response {
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state.blocking_result(move || db.write(|conn| Ok(sync_devices(conn, now).map(Value::Array)))).await.and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 422)
}

/// `POST /api/workflows/usb/devices/{device_id}/eject`.
pub async fn eject_usb_device(State(state): State<AppState>, UrlPath(device_id): UrlPath<String>) -> Response {
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let devices = sync_devices(conn, now)?;
                    let Some(current) = devices.into_iter().find(|d| d["id"] == device_id.as_str()) else { return fail("接続中のUSBが見つかりません") };
                    let mount = current["mount_path"].as_str().unwrap_or("").to_owned();
                    let output = std::process::Command::new("/usr/sbin/diskutil").args(["eject", &mount]).output().map_err(|e| Fail(format!("USBを取り外せませんでした: {e}")))?;
                    if !output.status.success() {
                        return fail(format!("USBを取り外せませんでした: {}", String::from_utf8_lossy(&output.stderr).trim()));
                    }
                    exec(conn, "UPDATE usb_devices SET connected=false,mount_path=NULL WHERE id=?", vec![Db::Text(device_id.clone())])?;
                    Ok(json!({"id": device_id, "ejected": true, "detail": String::from_utf8_lossy(&output.stdout).trim()}))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `POST /api/workflows/usb/handoffs/{export_id}/verify`.
pub async fn verify_usb_handoff(State(state): State<AppState>, UrlPath(export_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    if !["user_rekordbox_check", "user_hardware_check"].contains(&payload["level"].as_str().unwrap_or("")) {
        return invalid("level must be user_rekordbox_check or user_hardware_check");
    }
    let mut evidence = Map::new();
    for key in ["level", "model", "firmware", "checked_at", "checks", "notes"] {
        if let Some(value) = payload.get(key).filter(|v| !v.is_null()) {
            evidence.insert(key.into(), value.clone());
        }
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let Some(record) = row(conn, "SELECT * FROM usb_exports WHERE id=?", vec![Db::Text(export_id.clone())])? else { return fail("USB受け渡しが見つかりません") };
                    let current = py_str(&get(&snapshot(conn, record["setlist_id"].as_i64().unwrap_or(0))?, "hash"));
                    if Some(current.as_str()) != record["snapshot_hash"].as_str() {
                        return fail("セットリストまたは音源が変更されています。新しいUSB受け渡しを作成してください");
                    }
                    let level = evidence["level"].as_str().unwrap_or("").to_owned();
                    if level == "user_hardware_check" && !["model", "firmware", "checked_at", "checks"].iter().all(|k| evidence.get(*k).is_some_and(|v| v.as_str().is_none_or(|s| !s.is_empty()) && v.as_array().is_none_or(|a| !a.is_empty()))) {
                        return fail("実機確認には機種・firmware・日時・確認項目が必要です");
                    }
                    let mut verification = evidence.clone();
                    verification.insert("snapshot_hash".into(), record["snapshot_hash"].clone());
                    let verification = Value::Object(verification);
                    let state = if level == "user_hardware_check" { "verified" } else { "needs_user_check" };
                    exec(conn, "UPDATE usb_exports SET state=?,verification_json=?,updated_at=? WHERE id=?", vec![Db::Text(state.into()), Db::Text(py_json(&to_py(&verification))), ts(now), Db::Text(export_id.clone())])?;
                    Ok(json!({"id": export_id, "state": state, "verification": verification}))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 422)
}

// ------------------------------------------------------------------ recording timeline

fn timeline(conn: &Connection, id: i64) -> Result<Vec<Value>, Fail> {
    rows(conn, "SELECT * FROM recording_segments WHERE recording_id=? ORDER BY start_ms,position,id", vec![Db::BigInt(id)])
}

/// `GET /api/workflows/recordings/{recording_id}/timeline`.
pub async fn recording_timeline(State(state): State<AppState>, UrlPath(recording_id): UrlPath<String>) -> Response {
    let id = match path_int("recording_id", &recording_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    answer(state.blocking_result(move || db.lease().map_err(Fail::from).and_then(|conn| timeline(&conn, id)).map(Value::Array)).await, 422)
}

/// `PUT /api/workflows/recordings/{recording_id}/timeline`.
pub async fn replace_recording_timeline(State(state): State<AppState>, UrlPath(recording_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let id = match path_int("recording_id", &recording_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (Some(revision), Some(segments)) = (int_of(&payload["revision"]), payload["segments"].as_array().cloned()) else { return invalid("revision and segments are required") };
    if segments.len() > 5000 {
        return invalid("too many segments");
    }
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    let Some(recording) = row(conn, "SELECT * FROM recordings WHERE id=?", vec![Db::BigInt(id)])? else { return fail("録音が見つかりません") };
                    if recording["revision"].as_i64().unwrap_or(1) != revision {
                        return fail("録音が別の画面で更新されました");
                    }
                    let duration = recording["duration_ms"].as_i64().unwrap_or(0);
                    let mut checked = Vec::new();
                    for (index, segment) in segments.iter().enumerate() {
                        let start = int_of(&segment["start_ms"]).unwrap_or(-1);
                        let end = int_of(&segment["end_ms"]);
                        if start < 0 || start > duration || end.is_some_and(|e| e <= start || e > duration) {
                            return fail("曲目時刻が録音範囲外です");
                        }
                        let track = match int_of(&segment["track_id"]).filter(|t| *t != 0) {
                            Some(t) => row(conn, "SELECT title, artist FROM tracks WHERE id=?", vec![Db::BigInt(t)])?,
                            None => None,
                        };
                        let pick = |key: &str, fallback: &str| {
                            segment[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| track.as_ref().and_then(|t| t[fallback].as_str().map(str::to_owned)).unwrap_or_default())
                        };
                        let mut title = pick("title_snapshot", "title");
                        if title.is_empty() && track.is_none() {
                            title = "曲名未設定".into();
                        }
                        checked.push((index, start, end, segment.clone(), title, pick("artist_snapshot", "artist"), segment["event_key"].as_str().filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("manual:{id}:{}", uuid4()))));
                    }
                    exec(conn, "DELETE FROM recording_segments WHERE recording_id=? AND source='manual'", vec![Db::BigInt(id)])?;
                    for (index, start, end, segment, title, artist, key) in checked {
                        exec(
                            conn,
                            "INSERT INTO recording_segments (recording_id,event_key,track_id,deck,start_ms,end_ms,title_snapshot, artist_snapshot,version_snapshot,source,confidence,position) \
                             VALUES (?,?,?,?,?,?,?,?,?,'manual','confirmed',?)",
                            vec![
                                Db::BigInt(id),
                                Db::Text(key),
                                int_of(&segment["track_id"]).map_or(Db::Null, Db::BigInt),
                                text_or_null(segment["deck"].as_str()),
                                Db::BigInt(start),
                                end.map_or(Db::Null, Db::BigInt),
                                Db::Text(title),
                                Db::Text(artist),
                                text_or_null(segment["version_snapshot"].as_str()),
                                Db::BigInt(index as i64),
                            ],
                        )?;
                    }
                    exec(conn, "UPDATE recordings SET revision=revision+1 WHERE id=?", vec![Db::BigInt(id)])?;
                    Ok(Value::Array(timeline(conn, id)?))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `POST /api/workflows/recordings/{recording_id}/timeline/engine`.
pub async fn upsert_recording_engine_timeline(State(state): State<AppState>, UrlPath(recording_id): UrlPath<String>, headers: HeaderMap, bytes: Bytes) -> Response {
    let id = match path_int("recording_id", &recording_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let rate = int_of(&payload["sample_rate_hz"]).unwrap_or(0);
    let frames = int_of(&payload["frame_count"]).unwrap_or(-1);
    let dropped = int_of(&payload["dropped_events"]).unwrap_or(0);
    let segments = payload["segments"].as_array().cloned().unwrap_or_default();
    if !(8000..=384_000).contains(&rate) || frames < 0 || dropped < 0 || segments.len() > 10_000 {
        return invalid("invalid engine timeline");
    }
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                Ok((|| -> Out {
                    if scalar_int(conn, "SELECT id FROM recordings WHERE id=?", vec![Db::BigInt(id)])?.is_none() {
                        return fail("録音が見つかりません");
                    }
                    let confidence = if dropped != 0 { "incomplete" } else { "estimated" };
                    for (position, segment) in segments.iter().enumerate() {
                        let start = int_of(&segment["startFrame"]).unwrap_or(-1);
                        let end = int_of(&segment["endFrame"]).map_or(frames, |e| e.min(frames));
                        if start < 0 || end <= start || start > frames {
                            continue;
                        }
                        let track = segment["trackId"].as_str().map(str::to_owned).or_else(|| segment["trackId"].as_i64().map(|n| n.to_string())).filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).and_then(|s| s.parse::<i64>().ok());
                        let event = segment["eventKey"].as_str().map(str::to_owned).or_else(|| segment["eventKey"].as_i64().map(|n| n.to_string())).filter(|s| !s.is_empty()).unwrap_or_else(|| position.to_string());
                        exec(
                            conn,
                            "INSERT INTO recording_segments (recording_id,event_key,track_id,deck,load_generation, start_frame,end_frame,start_ms,end_ms,title_snapshot,artist_snapshot,source,confidence,position) \
                             VALUES (?,?,?,?,?,?,?,?,?,?,?,'engine_observed',?,?) ON CONFLICT(event_key) DO UPDATE SET end_frame=excluded.end_frame,end_ms=excluded.end_ms, \
                             confidence=excluded.confidence,revision=recording_segments.revision+1",
                            vec![
                                Db::BigInt(id),
                                Db::Text(format!("engine:{id}:{event}")),
                                track.map_or(Db::Null, Db::BigInt),
                                text_or_null(segment["deck"].as_str()),
                                int_of(&segment["loadGeneration"]).map_or(Db::Null, Db::BigInt),
                                Db::BigInt(start),
                                Db::BigInt(end),
                                Db::BigInt(start * 1000 / rate),
                                Db::BigInt(end * 1000 / rate),
                                Db::Text(segment["title"].as_str().filter(|s| !s.is_empty()).unwrap_or("曲名未設定").into()),
                                Db::Text(segment["artist"].as_str().unwrap_or("").into()),
                                Db::Text(confidence.into()),
                                Db::BigInt(position as i64),
                            ],
                        )?;
                    }
                    exec(
                        conn,
                        "UPDATE recordings SET sample_rate_hz=?,frame_count=?, timeline_quality=?,timeline_dropped_events=?, duration_ms=?,revision=revision+1 WHERE id=?",
                        vec![Db::BigInt(rate), Db::BigInt(frames), Db::Text(if dropped != 0 { "incomplete" } else { "engine_sampled" }.into()), Db::BigInt(dropped), Db::BigInt(frames * 1000 / rate), Db::BigInt(id)],
                    )?;
                    Ok(Value::Array(timeline(conn, id)?))
                })())
            })
        })
        .await
        .and_then(|r| r.map_err(|e| server_error(&e)));
    answer(result, 409)
}

/// `GET /api/workflows/recordings/{recording_id}/tracklist.txt`.
pub async fn recording_tracklist(State(state): State<AppState>, UrlPath(recording_id): UrlPath<String>) -> Response {
    let id = match path_int("recording_id", &recording_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state.blocking_result(move || db.lease().map_err(Fail::from).and_then(|conn| timeline(&conn, id))).await;
    match result {
        Ok(Ok(list)) => {
            let lines: Vec<String> = list
                .iter()
                .map(|r| {
                    let seconds = r["start_ms"].as_i64().unwrap_or(0).div_euclid(1000);
                    let stamp = format!("{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60);
                    let version = r["version_snapshot"].as_str().filter(|v| !v.is_empty()).map(|v| format!(" [{v}]")).unwrap_or_default();
                    format!("{stamp} {} - {}{version}", r["artist_snapshot"].as_str().unwrap_or(""), r["title_snapshot"].as_str().unwrap_or("")).trim().to_owned()
                })
                .collect();
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], lines.join("\n")).into_response()
        }
        Ok(Err(Fail(message))) => server_error(&message),
        Err(response) => response,
    }
}

/// `POST /api/workflows/recordings/external`.
pub async fn register_external_recording(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let (Some(filepath), Some(title)) = (payload["filepath"].as_str().map(str::to_owned), payload["title"].as_str().map(str::to_owned)) else { return invalid("filepath and title are required") };
    let artist = payload["artist"].as_str().unwrap_or("").to_owned();
    if title.is_empty() || title.chars().count() > 200 || artist.chars().count() > 200 {
        return invalid("invalid title or artist");
    }
    let path = expand_user(&filepath);
    if !path.is_absolute() || !path.is_file() {
        return http(422, "録音ファイルが見つかりません");
    }
    let suffix = path.extension().map(|e| format!(".{}", plumdeck_core::compat::text::casefold(&e.to_string_lossy()))).unwrap_or_default();
    if !EXTERNAL_RECORDING_EXTENSIONS.contains(&suffix.as_str()) {
        return http(422, "録音ライブラリで再生できない音声形式です");
    }
    let duration = plumdeck_media::tags::duration_seconds(&path);
    let Some(duration) = duration else { return http(422, "録音ファイルを読み取れません") };
    let duration_ms = plumdeck_core::compat::pyfmt::round_to_int(duration * 1000.0).unwrap_or(0);
    if duration_ms <= 0 {
        return http(422, "録音ファイルの長さを確認できません");
    }
    let key = format!("external:{}", uuid4());
    let resolved = std::fs::canonicalize(&path).unwrap_or(path).to_string_lossy().into_owned();
    let now = chrono::Utc::now().naive_utc();
    let db = state.db.clone();
    let key_value = key.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                Ok(conn.query_row(
                    "INSERT INTO recordings (recording_key,filepath,started_at,ended_at,duration_ms,status,artist,title,source) VALUES (?,?,?,?,?,'completed',?,?,'external') RETURNING id",
                    params_from_iter([Db::Text(key_value), Db::Text(resolved), ts(now), ts(now), Db::BigInt(duration_ms), Db::Text(artist), Db::Text(title)]),
                    |r| r.get::<_, i64>(0),
                )?)
            })
        })
        .await;
    match result {
        Ok(id) => json_response(&json!({"id": id, "recording_key": key, "duration_ms": duration_ms})),
        Err(response) => response,
    }
}

// ------------------------------------------------------------------ backup and restore

fn allowed_ui_setting(key: &str, value: &Value) -> bool {
    let lowered = plumdeck_core::compat::text::casefold(key);
    value.is_string() && (key == "vite-ui-theme" || key.starts_with("plumdeck.")) && !SENSITIVE_SETTING_WORDS.iter().any(|w| lowered.contains(w))
}

fn app_version() -> String {
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../package.json");
    std::fs::read_to_string(package).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["version"].as_str().map(str::to_owned)).unwrap_or_else(|| "unknown".into())
}

/// `POST /api/workflows/backups`.
pub async fn backup(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(destination) = payload["destination"].as_str().map(str::to_owned) else { return invalid("destination is required") };
    let db = state.db.clone();
    let result = state.blocking_result(move || create_backup(&db, &destination, &payload)).await;
    answer(result, 422)
}

fn create_backup(db: &plumdeck_db::handle::DbHandle, destination: &str, payload: &Value) -> Out {
    let mut target = expand_user(destination);
    if !target.is_absolute() {
        return fail("バックアップ先は絶対パスで指定してください");
    }
    if target.extension().map(|e| e != "plumdeck-backup").unwrap_or(true) || !target.to_string_lossy().ends_with(".plumdeck-backup") {
        target = target.with_extension("plumdeck-backup");
    }
    let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&parent)?;
    let operation_id = format!("backup-{}", uuid4());
    let settings: Map<String, Value> = payload["ui_settings"].as_object().map(|m| m.iter().filter(|(k, v)| allowed_ui_setting(k, v)).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    let stage = parent.join(format!("plumdeck-backup-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&stage)?;
    let archive_tmp = target.with_file_name(format!(".{}.{}.partial", target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), uuid::Uuid::new_v4().simple()));
    let result = (|| -> Out {
        let _workflow = workflow_lock().lock().unwrap_or_else(|e| e.into_inner());
        let snapshot_path = stage.join("plumdeck.duckdb");
        let mut table_counts = Map::new();
        {
            let guard = db.maintenance()?;
            guard.checkpoint()?;
            let conn = guard.lease()?;
            let database: String = conn.query_row("SELECT current_database()", [], |r| r.get(0)).map_err(DbError::from)?;
            let quoted = snapshot_path.to_string_lossy().replace('\'', "''");
            conn.execute_batch(&format!("ATTACH '{quoted}' AS plumdeck_backup_snapshot")).map_err(DbError::from)?;
            let copied = conn
                .execute_batch(&format!("COPY FROM DATABASE \"{}\" TO plumdeck_backup_snapshot", database.replace('"', "\"\"")))
                .and_then(|_| conn.execute_batch("CHECKPOINT plumdeck_backup_snapshot"));
            let _ = conn.execute_batch("DETACH plumdeck_backup_snapshot");
            copied.map_err(DbError::from)?;
            let queue = plumdeck_db::restore_recovery::queue_path(db.path());
            if queue.is_file() {
                snapshot_sqlite(&queue, &stage.join("analysis-jobs.sqlite3"))?;
            }
            for table in ALL_TABLES {
                table_counts.insert(table.into(), json!(scalar_int(&conn, &format!("SELECT count(*) FROM \"{table}\""), vec![])?.unwrap_or(0)));
            }
        }
        let snapshot = plumdeck_db::duckdb::Connection::open_with_flags(&snapshot_path, plumdeck_db::duckdb::Config::default().access_mode(plumdeck_db::duckdb::AccessMode::ReadOnly).map_err(DbError::from)?).map_err(DbError::from)?;
        let expected: BTreeMap<String, String> = rows(&snapshot, "SELECT t.filepath,m.sha256 FROM tracks t JOIN track_media m ON m.track_id=t.id WHERE m.sha256 IS NOT NULL", vec![])?
            .into_iter()
            .filter_map(|r| Some((r["filepath"].as_str()?.to_owned(), r["sha256"].as_str()?.to_owned())))
            .collect();
        let mut assets: Vec<(String, &str)> = Vec::new();
        if payload["include_media"].as_bool().unwrap_or(false) {
            assets.extend(rows(&snapshot, "SELECT filepath FROM tracks ORDER BY id", vec![])?.into_iter().filter_map(|r| r["filepath"].as_str().map(|p| (p.to_owned(), "media"))));
            let sampler: Value = settings.get("plumdeck.sampler.paths").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!([]));
            assets.extend(sampler.as_array().cloned().unwrap_or_default().into_iter().filter_map(|p| p.as_str().filter(|s| !s.is_empty()).map(|s| (s.to_owned(), "sampler"))));
        }
        if payload["include_recordings"].as_bool().unwrap_or(false) {
            assets.extend(rows(&snapshot, "SELECT filepath FROM recordings WHERE status='completed' ORDER BY id", vec![])?.into_iter().filter_map(|r| r["filepath"].as_str().map(|p| (p.to_owned(), "recording"))));
        }
        drop(snapshot);
        let (mut files, mut missing) = (Vec::new(), Vec::new());
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for (original, category) in assets {
            let source = PathBuf::from(&original);
            if !source.is_file() {
                missing.push(json!(original));
                continue;
            }
            let identity = file_identity(&source, true)?;
            let hash = identity["sha256"].as_str().unwrap_or("").to_owned();
            if category == "media" && expected.get(&original).is_some_and(|e| *e != hash) {
                return fail("保存済みの解析対象と音源内容が異なります。再解析してからバックアップしてください");
            }
            let relative = match seen.get(&hash) {
                Some(relative) => relative.clone(),
                None => {
                    let suffix = source.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
                    let relative = format!("assets/content/{hash}{suffix}");
                    let destination = stage.join(&relative);
                    std::fs::create_dir_all(destination.parent().unwrap_or(&stage))?;
                    std::fs::copy(&source, &destination)?;
                    if sha256_file(&destination, None)? != hash {
                        return fail("収集中に音源内容が変化しました");
                    }
                    seen.insert(hash.clone(), relative.clone());
                    relative
                }
            };
            let mut item = identity.clone();
            item["path"] = json!(relative);
            item["category"] = json!(category);
            item["source_path"] = json!(original);
            files.push(item);
        }
        std::fs::write(stage.join("ui-settings.json"), serde_json::to_string(&Value::Object(settings.clone())).unwrap_or_default())?;
        let mut base = vec![("plumdeck.duckdb", "database"), ("ui-settings.json", "settings")];
        if stage.join("analysis-jobs.sqlite3").exists() {
            base.push(("analysis-jobs.sqlite3", "jobs"));
        }
        for (name, category) in base {
            let path = stage.join(name);
            files.push(json!({"path": name, "category": category, "size_bytes": std::fs::metadata(&path)?.len(), "sha256": sha256_file(&path, None)?}));
        }
        let library_identity = hex(&Sha256::digest(std::fs::canonicalize(db.path()).unwrap_or_else(|_| db.path().to_path_buf()).to_string_lossy().as_bytes()));
        let manifest = json!({
            "format": "plumdeck-backup", "format_version": BACKUP_FORMAT_VERSION, "app_version": app_version(),
            "created_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string(), "schema_version": plumdeck_db::SCHEMA_VERSION,
            "snapshot_id": operation_id, "library_identity": library_identity, "table_counts": table_counts, "files": files,
            "included": {"media": payload["include_media"].as_bool().unwrap_or(false), "recordings": payload["include_recordings"].as_bool().unwrap_or(false), "ui_settings": true},
            "excluded": ["credentials", "keychain", "os_permissions", "live_playback", "live_recording_handles", "rebuildable_waveform_cache"],
            "missing_assets": missing,
        });
        std::fs::write(stage.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap_or_default())?;
        write_archive(&stage, &archive_tmp)?;
        inspect_archive(&archive_tmp)?;
        std::fs::rename(&archive_tmp, &target)?;
        Ok(json!({"id": operation_id, "path": target.to_string_lossy(), "missing_assets": manifest["missing_assets"], "file_count": files.len(), "table_counts": table_counts}))
    })();
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_file(&archive_tmp);
    result
}

fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<(), Fail> {
    let live = plumdeck_db::rusqlite::Connection::open(source).map_err(|e| Fail(e.to_string()))?;
    let mut saved = plumdeck_db::rusqlite::Connection::open(destination).map_err(|e| Fail(e.to_string()))?;
    let backup = plumdeck_db::rusqlite::backup::Backup::new(&live, &mut saved).map_err(|e| Fail(e.to_string()))?;
    backup.run_to_completion(256, std::time::Duration::ZERO, None).map_err(|e| Fail(e.to_string()))?;
    drop(backup);
    let check: String = saved.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| Fail(e.to_string()))?;
    if check != "ok" {
        return fail("解析ジョブDBの検査に失敗しました");
    }
    Ok(())
}

fn write_archive(stage: &Path, target: &Path) -> Result<(), Fail> {
    let file = std::fs::File::create(target)?;
    let mut archive = zip::ZipWriter::new(file);
    let mut stack = vec![stage.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    for path in paths {
        let name = path.strip_prefix(stage).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let stored = ["mp3", "flac", "ogg", "zip"].contains(&path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default().as_str());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(if stored { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated })
            .large_file(std::fs::metadata(&path)?.len() >= 0xFFFF_FFFF);
        archive.start_file(name, options).map_err(|e| Fail(e.to_string()))?;
        let mut source = std::fs::File::open(&path)?;
        std::io::copy(&mut source, &mut archive)?;
    }
    archive.finish().map_err(|e| Fail(e.to_string()))?.flush()?;
    Ok(())
}

/// `_validated_archive`.
fn inspect_archive(path: &Path) -> Result<(zip::ZipArchive<std::fs::File>, Value), Fail> {
    let file = std::fs::File::open(safe_real_file(&path.to_string_lossy())?)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| Fail("バックアップを読み取れません".into()))?;
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    if archive.len() > MAX_BACKUP_ENTRIES {
        return fail("バックアップの展開サイズが上限を超えています");
    }
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|e| Fail(e.to_string()))?;
        total += entry.size();
        let name = entry.name().to_owned();
        let unsafe_path = name.starts_with('/') || name.split('/').any(|p| p == "..") || name.contains('\\') || entry.is_dir() || names.contains(&name);
        if unsafe_path {
            return fail("安全でないアーカイブパスです");
        }
        names.insert(name);
    }
    if total > MAX_BACKUP_UNCOMPRESSED {
        return fail("バックアップの展開サイズが上限を超えています");
    }
    if !["manifest.json", "plumdeck.duckdb", "ui-settings.json"].iter().all(|n| names.contains(*n)) {
        return fail("必須データがありません");
    }
    let manifest: Value = {
        let mut entry = archive.by_name("manifest.json").map_err(|e| Fail(e.to_string()))?;
        if entry.size() > 32 * 1024 * 1024 {
            return fail("manifestが大きすぎます");
        }
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        serde_json::from_str(&text).map_err(|_| Fail("manifestを読み取れません".into()))?
    };
    if manifest["format"] != "plumdeck-backup" || manifest["format_version"].as_i64() != Some(BACKUP_FORMAT_VERSION) {
        return fail("未対応のバックアップ形式です");
    }
    let id = manifest["snapshot_id"].as_str().unwrap_or("");
    if id.is_empty() || id.len() > 100 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return fail("snapshot IDが不正です");
    }
    if int_of(&manifest["schema_version"]).unwrap_or(0) > plumdeck_db::SCHEMA_VERSION {
        return fail("新しいバージョンのバックアップは復元できません");
    }
    let mut checked: BTreeMap<String, (Value, Value)> = BTreeMap::new();
    for item in manifest["files"].as_array().cloned().unwrap_or_default() {
        let name = item["path"].as_str().unwrap_or("").to_owned();
        if !names.contains(&name) || name == "manifest.json" {
            return fail("manifestに記載されたファイルがありません");
        }
        let identity = (item["sha256"].clone(), item["size_bytes"].clone());
        if let Some(previous) = checked.get(&name) {
            if *previous != identity {
                return fail("重複したファイル情報が一致しません");
            }
            continue;
        }
        let mut entry = archive.by_name(&name).map_err(|e| Fail(e.to_string()))?;
        let size = entry.size();
        let mut digest = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        if Some(hex(&digest.finalize()).as_str()) != identity.0.as_str() || Some(size) != identity.1.as_u64() {
            return fail("バックアップのハッシュまたはサイズが一致しません");
        }
        checked.insert(name, identity);
    }
    let mut expected = names.clone();
    expected.remove("manifest.json");
    if checked.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return fail("manifestに未検証のファイルがあります");
    }
    Ok((archive, manifest))
}

/// `POST /api/workflows/restore/inspect` (`{"path": ...}`).
pub async fn restore_inspect(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(path) = payload["path"].as_str().map(str::to_owned) else { return invalid("path is required") };
    let result = state.blocking_result(move || inspect_archive(Path::new(&path)).map(|(_, manifest)| manifest)).await;
    answer(result, 422)
}

/// `POST /api/workflows/restore/apply`.
pub async fn restore_apply(State(state): State<AppState>, headers: HeaderMap, bytes: Bytes) -> Response {
    let payload = match body(&headers, &bytes) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(path) = payload["path"].as_str().map(str::to_owned) else { return invalid("path is required") };
    let confirmed = payload["confirmed"].as_bool().unwrap_or(false);
    let db = state.db.clone();
    let now = state.clock.now_naive();
    let result = state.blocking_result(move || restore_backup(&db, &path, confirmed, now)).await;
    answer(result, 409)
}

fn restore_backup(db: &plumdeck_db::handle::DbHandle, path: &str, confirmed: bool, now: NaiveDateTime) -> Out {
    if !confirmed {
        return fail("現在のアプリデータを置き換える確認が必要です");
    }
    let (mut archive, manifest) = inspect_archive(Path::new(path))?;
    let db_path = db.path().to_path_buf();
    let snapshot_id = manifest["snapshot_id"].as_str().unwrap_or("").to_owned();
    let restore_root = db_path.parent().unwrap_or(Path::new(".")).join("restore-staging").join(&snapshot_id);
    let rollback = db_path.with_file_name(format!("{}.pre-restore-{}", db_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(restore_root.parent().unwrap_or(Path::new(".")))?;
    std::fs::create_dir(&restore_root)?;
    let result = (|| -> Out {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|e| Fail(e.to_string()))?;
            let destination = restore_root.join(entry.name());
            std::fs::create_dir_all(destination.parent().unwrap_or(&restore_root))?;
            let mut out = std::fs::File::create(&destination)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        let staged = restore_root.join("plumdeck.duckdb");
        plumdeck_db::compaction::ensure_healthy_db(&staged)?;
        let managed = db_path.parent().unwrap_or(Path::new(".")).join("restored-assets").join(&snapshot_id);
        let mut sampler: BTreeMap<String, String> = BTreeMap::new();
        {
            let conn = plumdeck_db::duckdb::Connection::open(&staged).map_err(DbError::from)?;
            for item in manifest["files"].as_array().cloned().unwrap_or_default() {
                let category = item["category"].as_str().unwrap_or("");
                if !["media", "recording", "sampler"].contains(&category) {
                    continue;
                }
                let relative = item["path"].as_str().unwrap_or("");
                let (source, target) = (restore_root.join(relative), managed.join(relative));
                std::fs::create_dir_all(target.parent().unwrap_or(&managed))?;
                if target.exists() && sha256_file(&target, None)? != item["sha256"].as_str().unwrap_or("") {
                    return fail("復元先に同名で内容の異なるファイルがあります");
                }
                if !target.exists() {
                    std::fs::copy(&source, &target)?;
                }
                let target_text = target.to_string_lossy().into_owned();
                if category == "sampler" {
                    sampler.insert(item["source_path"].as_str().unwrap_or("").to_owned(), target_text);
                } else {
                    let table = if category == "media" { "tracks" } else { "recordings" };
                    exec(&conn, &format!("UPDATE {table} SET filepath=? WHERE filepath=?"), vec![Db::Text(target_text), text_or_null(item["source_path"].as_str())])?;
                }
            }
            exec(&conn, "UPDATE import_items SET state='queued' WHERE state IN ('probing','analyzing')", vec![])?;
            exec(&conn, "UPDATE import_batches SET state='paused',paused=true WHERE state IN ('queued','processing','pausing')", vec![])?;
            exec(&conn, "UPDATE operation_journal SET state='paused',updated_at=? WHERE state IN ('queued','running','processing')", vec![ts(now)])?;
            conn.execute_batch("CHECKPOINT").map_err(DbError::from)?;
        }
        let jobs = restore_root.join("analysis-jobs.sqlite3");
        if jobs.exists() {
            let queue = plumdeck_db::rusqlite::Connection::open(&jobs).map_err(|e| Fail(e.to_string()))?;
            queue
                .execute_batch("UPDATE jobs SET status=CASE WHEN json_extract(config, '$.background') THEN 'queued' ELSE 'paused' END WHERE status IN ('running','pausing'); UPDATE items SET status='pending' WHERE status='running';")
                .map_err(|e| Fail(e.to_string()))?;
        }
        let queue_path = plumdeck_db::restore_recovery::queue_path(&db_path);
        let queue_rollback = PathBuf::from(format!("{}.analysis-jobs.sqlite3", rollback.to_string_lossy()));
        {
            let _workflow = workflow_lock().lock().unwrap_or_else(|e| e.into_inner());
            let mut guard = db.maintenance()?;
            guard.checkpoint()?;
            guard.close()?;
            let queue_existed = queue_path.exists();
            if db_path.exists() {
                std::fs::copy(&db_path, &rollback)?;
            }
            if queue_existed {
                snapshot_sqlite(&queue_path, &queue_rollback)?;
            }
            let replacement = db_path.with_file_name(format!(".{}.restore", db_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            std::fs::copy(&staged, &replacement)?;
            plumdeck_db::restore_recovery::begin_restore(&db_path, &rollback, queue_existed)?;
            let swapped = (|| -> Result<(), Fail> {
                std::fs::rename(&replacement, &db_path)?;
                for suffix in ["-wal", "-shm", ""] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", queue_path.to_string_lossy()));
                }
                if jobs.exists() {
                    snapshot_sqlite(&jobs, &queue_path)?;
                }
                plumdeck_db::restore_recovery::sync_restored_files(&db_path)?;
                guard.reopen()?;
                plumdeck_db::restore_recovery::finish_restore(&db_path)?;
                Ok(())
            })();
            if let Err(error) = swapped {
                let _ = guard.close();
                if rollback.exists() {
                    std::fs::copy(&rollback, &db_path)?;
                }
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", queue_path.to_string_lossy()));
                }
                if queue_existed {
                    snapshot_sqlite(&queue_rollback, &queue_path)?;
                }
                plumdeck_db::restore_recovery::sync_restored_files(&db_path)?;
                guard.reopen()?;
                plumdeck_db::restore_recovery::finish_restore(&db_path)?;
                return Err(error);
            }
        }
        let mut restored: Value = std::fs::read_to_string(restore_root.join("ui-settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        if !sampler.is_empty() {
            if let Some(paths) = restored["plumdeck.sampler.paths"].as_str().and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()) {
                let mapped: Vec<String> = paths.iter().map(|p| sampler.get(p).cloned().unwrap_or_else(|| p.clone())).collect();
                restored["plumdeck.sampler.paths"] = json!(py_json(&to_py(&json!(mapped))).replace(",", ", "));
            }
        }
        Ok(json!({"restored": true, "snapshot_id": snapshot_id, "rollback_path": rollback.to_string_lossy(), "ui_settings": restored}))
    })();
    drop(archive);
    let _ = std::fs::remove_dir_all(&restore_root);
    result
}
