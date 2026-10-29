//! The Dashboard's import helpers (owner decision README §2.5; additions
//! D-52, D-53):
//!
//! - `GET /api/library/attention`: tracks that need work (unanalyzed, failed
//!   imports, default timbre, pending embeddings, outdated analysis) and the
//!   analysis job that is fixing them.
//! - `POST /api/library/attention/{kind}`: one-click fix. Unanalyzed and
//!   failed tracks go through the shared Play import queue; timbre, outdated
//!   components and embeddings through the analysis job queue.
//! - `GET /api/rekordbox/unregistered`: rekordbox collection tracks whose
//!   files exist but are not in plumdeck (`available: false` without
//!   rekordbox).
//! - `POST /api/rekordbox/unregistered/import`: imports all or the selected
//!   ones through the import queue; they take BPM, grid and key from
//!   rekordbox (read-only access to master.db).

use std::collections::HashSet;
use std::path::Path as FsPath;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::analyzed::{pending_embedding_ids_sql, COMPLETED_ANALYSIS_SQL};
use serde_json::{json, Value};

use crate::analysis::{current_versions, SUPPORTED_EXTENSIONS};
use crate::routes::ingest::{start_import, ImportRequest, REKORDBOX_ORIGIN};
use crate::{AppState, HttpError};

fn http(status: u16, detail: impl Into<String>) -> Response {
    HttpError(ApiError::new(status, detail.into())).into_response()
}

const TIMBRE_DEFAULT_SQL: &str = "(coalesce(tracks.energy, 0) = 0 AND coalesce(tracks.danceability, 0) = 0 \
    AND coalesce(tracks.brightness, 0) = 0 AND coalesce(tracks.noisiness, 0) = 0 AND coalesce(tracks.bpm, 0) > 0)";

fn component_sql(feature: &str) -> String {
    let versions = current_versions(feature).iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(",");
    format!("coalesce(json_extract_string(CASE WHEN json_valid(a.features_extra_json) THEN a.features_extra_json END, '$.analysis_components.{feature}') IN ({versions}), false)")
}

fn source_sql(field: &str) -> String {
    format!("coalesce(json_extract_string(CASE WHEN json_valid(a.features_extra_json) THEN a.features_extra_json END, '$.analysis_components.sources.{field}') IN ('rekordbox','manual'), false)")
}

/// Analyzed tracks with a component from an older analyzer. BPM/key that
/// come from rekordbox or a manual edit do not need their own component.
fn outdated_sql() -> String {
    format!(
        "SELECT tracks.id FROM tracks JOIN track_analyses a ON a.track_id = tracks.id WHERE {COMPLETED_ANALYSIS_SQL} AND NOT {TIMBRE_DEFAULT_SQL} AND NOT ( \
         ({} OR {}) AND ({} OR {}) AND {} AND {})",
        component_sql("rhythm"),
        source_sql("bpm"),
        component_sql("key"),
        source_sql("key"),
        component_sql("timbre"),
        component_sql("waveform"),
    )
}

fn ids(conn: &plumdeck_db::duckdb::Connection, sql: &str) -> Result<Vec<i64>, String> {
    let mut statement = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = statement.query_map([], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn strings(conn: &plumdeck_db::duckdb::Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut statement = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = statement.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn unanalyzed_paths(conn: &plumdeck_db::duckdb::Connection) -> Result<Vec<String>, String> {
    let paths = strings(conn, &format!("SELECT tracks.filepath FROM tracks WHERE NOT {COMPLETED_ANALYSIS_SQL} AND NOT {TIMBRE_DEFAULT_SQL} ORDER BY tracks.id"))?;
    Ok(paths.into_iter().filter(|p| FsPath::new(p).is_file()).collect())
}

/// Files whose latest import attempt failed and that are not analyzed now.
fn failed_paths(conn: &plumdeck_db::duckdb::Connection) -> Result<Vec<String>, String> {
    let paths = strings(
        conn,
        &format!(
            "SELECT p FROM (SELECT i.canonical_path AS p, arg_max(i.state, b.created_at) AS last FROM import_items i JOIN import_batches b ON b.id = i.batch_id \
             WHERE i.canonical_path IS NOT NULL GROUP BY i.canonical_path) WHERE last = 'failed' \
             AND p NOT IN (SELECT tracks.filepath FROM tracks WHERE {COMPLETED_ANALYSIS_SQL}) ORDER BY p"
        ),
    )?;
    Ok(paths.into_iter().filter(|p| FsPath::new(p).is_file()).collect())
}

fn running_job(state: &AppState) -> Value {
    let job = state.analysis.jobs.status(None).unwrap_or(Value::Null);
    let active = ["running", "queued", "pausing", "paused"].contains(&job["status"].as_str().unwrap_or(""));
    if !active {
        return Value::Null;
    }
    let counts = &job["counts"];
    let done = ["completed", "skipped", "failed"].iter().map(|k| counts[*k].as_i64().unwrap_or(0)).sum::<i64>();
    json!({
        "id": job["id"], "status": job["status"], "features": job["config"]["features"],
        "total": job["total"], "done": done, "failed": counts["failed"],
    })
}

/// `GET /api/library/attention`.
pub async fn attention(State(state): State<AppState>) -> Response {
    let worker = state.clone();
    let result = state
        .blocking_result(move || -> Result<Value, String> {
            let conn = worker.db.lease().map_err(|e| e.to_string())?;
            let count = |sql: String| -> Result<i64, String> { conn.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |r| r.get(0)).map_err(|e| e.to_string()) };
            let unanalyzed = unanalyzed_paths(&conn)?.len();
            let failed = failed_paths(&conn)?.len();
            let timbre_default = count(format!("SELECT id FROM tracks WHERE {TIMBRE_DEFAULT_SQL}"))?;
            let embedding_pending = count(pending_embedding_ids_sql())?;
            let outdated = count(outdated_sql())?;
            drop(conn);
            Ok(json!({
                "unanalyzed": unanalyzed, "failed": failed, "timbre_default": timbre_default,
                "embedding_pending": embedding_pending, "outdated": outdated, "job": running_job(&worker),
            }))
        })
        .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(message)) => http(500, message),
        Err(response) => response,
    }
}

fn friendly(message: String) -> String {
    if message.contains("already running") {
        "ほかの解析が進行中です。終わってからもう一度お試しください。".into()
    } else if message.contains("Library import is running") {
        "取り込みが進行中です。終わってからもう一度お試しください。".into()
    } else {
        message
    }
}

async fn import_paths(state: &AppState, paths: Vec<String>, origin: &str) -> Response {
    if paths.is_empty() {
        return Json(json!({"started": 0})).into_response();
    }
    let count = paths.len();
    let request = ImportRequest {
        request_id: format!("{origin}-{}", uuid::Uuid::new_v4()),
        paths,
        kind: "collection".into(),
        target_id: None,
        origin: origin.into(),
        profile: "full".into(),
    };
    match start_import(state, request).await {
        Ok(batch) => Json(json!({"started": count, "batch_id": batch["id"]})).into_response(),
        Err(Ok(message)) => http(409, message),
        Err(Err(response)) => response,
    }
}

/// `POST /api/library/attention/{kind}`.
pub async fn fix_attention(State(state): State<AppState>, Path(kind): Path<String>) -> Response {
    let worker = state.clone();
    let kind_for_work = kind.clone();
    let planned = state
        .blocking_result(move || -> Result<Result<Vec<String>, Value>, String> {
            let jobs = worker.analysis.jobs.clone();
            let conn = worker.db.lease().map_err(|e| e.to_string())?;
            match kind_for_work.as_str() {
                "unanalyzed" => Ok(Ok(unanalyzed_paths(&conn)?)),
                "failed" => Ok(Ok(failed_paths(&conn)?)),
                "timbre_default" => {
                    let ids = ids(&conn, &format!("SELECT id FROM tracks WHERE {TIMBRE_DEFAULT_SQL} ORDER BY id"))?;
                    drop(conn);
                    if ids.is_empty() {
                        return Ok(Err(json!({"started": 0})));
                    }
                    let count = ids.len();
                    jobs.start(Some(ids), None, Some(vec!["timbre".into()]), false, None, 1).map_err(friendly)?;
                    Ok(Err(json!({"started": count})))
                }
                "outdated" => {
                    let ids = ids(&conn, &format!("{} ORDER BY tracks.id", outdated_sql()))?;
                    drop(conn);
                    if ids.is_empty() {
                        return Ok(Err(json!({"started": 0})));
                    }
                    let count = ids.len();
                    let features = ["rhythm", "key", "timbre", "waveform"].map(String::from).to_vec();
                    jobs.start(Some(ids), None, Some(features), true, None, 1).map_err(friendly)?;
                    Ok(Err(json!({"started": count})))
                }
                "embedding_pending" => {
                    drop(conn);
                    let job = jobs.enqueue_embedding(None, None).map_err(friendly)?;
                    Ok(Err(json!({"started": job["total"].as_i64().unwrap_or(0)})))
                }
                _ => Err("unknown".into()),
            }
        })
        .await;
    match planned {
        Ok(Ok(Ok(paths))) => import_paths(&state, paths, if kind == "failed" { "attention_retry" } else { "attention" }).await,
        Ok(Ok(Err(value))) => Json(value).into_response(),
        Ok(Err(message)) if message == "unknown" => http(404, "Unknown attention kind"),
        Ok(Err(message)) => http(409, message),
        Err(response) => response,
    }
}

// ------------------------------------------------------------------ rekordbox

const STREAMING_FILE_TYPES: [i64; 3] = [19, 20, 26];

/// rekordbox tracks with an existing, supported file that plumdeck does not
/// have. `None` when rekordbox is not installed or its library is unreadable.
fn unregistered(state: &AppState) -> Result<Option<Vec<plumdeck_rekordbox::masterdb::Content>>, String> {
    let Some(path) = plumdeck_rekordbox::locate::default_master_db_path().filter(|p| p.is_file()) else { return Ok(None) };
    let Ok(db) = plumdeck_rekordbox::MasterDb::open(&path) else { return Ok(None) };
    let Ok(contents) = db.contents() else { return Ok(None) };
    let conn = state.db.lease().map_err(|e| e.to_string())?;
    let known: HashSet<String> = strings(&conn, "SELECT filepath FROM tracks")?.iter().map(|p| plumdeck_rekordbox::paths::normalize_path(p)).collect();
    drop(conn);
    let mut seen = HashSet::new();
    Ok(Some(
        contents
            .into_iter()
            .filter(|c| !c.file_type.is_some_and(|t| STREAMING_FILE_TYPES.contains(&t)))
            .filter(|c| {
                let Some(file) = c.folder_path.as_deref().filter(|p| !p.is_empty()) else { return false };
                let normalized = plumdeck_rekordbox::paths::normalize_path(file);
                let lower = file.to_lowercase();
                !known.contains(&normalized)
                    && SUPPORTED_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
                    && FsPath::new(file).is_file()
                    && seen.insert(normalized)
            })
            .collect(),
    ))
}

/// `GET /api/rekordbox/unregistered`.
pub async fn rekordbox_unregistered(State(state): State<AppState>) -> Response {
    let worker = state.clone();
    match state.blocking_result(move || unregistered(&worker)).await {
        Ok(Ok(None)) => Json(json!({"available": false, "total": 0, "tracks": []})).into_response(),
        Ok(Ok(Some(rows))) => {
            let tracks: Vec<Value> = rows
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id, "title": c.title, "artist": c.artist, "filepath": c.folder_path,
                        "bpm": c.bpm(), "key": c.key_name, "duration": c.length_s,
                    })
                })
                .collect();
            Json(json!({"available": true, "total": tracks.len(), "tracks": tracks})).into_response()
        }
        Ok(Err(message)) => http(500, message),
        Err(response) => response,
    }
}

/// `POST /api/rekordbox/unregistered/import` with `{"ids": [...]}` (selected
/// rekordbox content ids) or `{}` (all).
pub async fn import_rekordbox_unregistered(State(state): State<AppState>, body: Bytes) -> Response {
    let payload: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body).unwrap_or(Value::Null) };
    if !payload.is_object() {
        return http(422, "Invalid request body");
    }
    let selected: Option<HashSet<String>> = match &payload["ids"] {
        Value::Null => None,
        Value::Array(items) => Some(items.iter().filter_map(|v| v.as_str().map(str::to_owned).or_else(|| v.as_i64().map(|n| n.to_string()))).collect()),
        _ => return http(422, "ids: Input should be a valid list"),
    };
    let worker = state.clone();
    let found = state.blocking_result(move || unregistered(&worker)).await;
    let rows = match found {
        Ok(Ok(Some(rows))) => rows,
        Ok(Ok(None)) => return http(409, "rekordbox のライブラリを読み取れませんでした"),
        Ok(Err(message)) => return http(500, message),
        Err(response) => return response,
    };
    let paths: Vec<String> = rows
        .into_iter()
        .filter(|c| selected.as_ref().is_none_or(|ids| ids.contains(&c.id)))
        .filter_map(|c| c.folder_path)
        .collect();
    import_paths(&state, paths, REKORDBOX_ORIGIN).await
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn attention_counts_and_unknown_kind() {
        let library = TestLibrary::new("attention");
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/api/library/attention", &[], "").await;
        let body = answer.json();
        assert_eq!(
            (body["unanalyzed"].clone(), body["failed"].clone(), body["timbre_default"].clone(), body["embedding_pending"].clone(), body["outdated"].clone()),
            (json!(0), json!(0), json!(0), json!(0), json!(0))
        );
        // Missing files are not offered for import; zero timbre is its own group.
        library.execute(
            "INSERT INTO tracks (filepath, bpm, duration, energy, danceability, brightness, noisiness) VALUES \
             ('/nonexistent/a.mp3', 0, 0, NULL, NULL, NULL, NULL), ('/nonexistent/b.mp3', 120, 200, 0, 0, 0, 0);",
        );
        let body = send(app(), Method::GET, "/api/library/attention", &[], "").await.json();
        assert_eq!((body["unanalyzed"].clone(), body["timbre_default"].clone()), (json!(0), json!(1)));
        let answer = send(app(), Method::POST, "/api/library/attention/nope", &[], "").await;
        assert_eq!(answer.status.as_u16(), 404);
        let answer = send(app(), Method::POST, "/api/library/attention/unanalyzed", &[], "").await;
        assert_eq!(answer.json(), json!({"started": 0}));
    }
}
