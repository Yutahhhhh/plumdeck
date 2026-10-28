//! `GET /api/dashboard` (backend/api/routers/system.py
//! `get_dashboard_stats`), and `POST /api/system/save-file` /
//! `/api/system/reveal-file`, which the desktop uses to save exports and show
//! them in the file manager (the backend runs inside the app since S13).
//! `GET /api/` is dropped (§2.2 item 1).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_db::repos::dashboard::{dashboard_stats, DashboardStats, SetlistRow};

use crate::response::{datetime, dict, int, opt, text, PyJson};
use crate::body::{parse_body_for, BodyModel, Fields, ValidationError};
use crate::AppState;
use crate::HttpError;
use plumdeck_core::error::ApiError;

/// `jsonable_encoder(Setlist)`: every model field, datetimes in ISO form.
/// A NaN duration makes the whole answer a 500, as in Python.
fn setlist_json(row: &SetlistRow) -> PyValue {
    dict([
        ("id", int(row.id)),
        ("name", opt(row.name.clone(), text)),
        ("description", opt(row.description.clone(), text)),
        ("display_order", opt(row.display_order, int)),
        ("genre", opt(row.genre.clone(), text)),
        ("target_duration", opt(row.target_duration, PyValue::Float)),
        ("rating", opt(row.rating, int)),
        ("created_at", opt(row.created_at.as_ref(), datetime)),
        ("updated_at", opt(row.updated_at.as_ref(), datetime)),
    ])
}

fn stats_json(stats: &DashboardStats) -> PyValue {
    let genres = stats
        .genre_distribution
        .iter()
        .map(|genre| dict([("name", text(genre.name.clone())), ("count", int(genre.count))]))
        .collect();
    dict([
        ("total_tracks", int(stats.total_tracks)),
        ("analyzed_tracks", int(stats.analyzed_tracks)),
        ("unanalyzed_tracks", int(stats.unanalyzed_tracks())),
        ("embedding_pending_tracks", int(stats.embedding_pending_tracks)),
        ("genre_distribution", PyValue::List(genres)),
        ("unverified_genres_count", int(stats.unverified_genres_count)),
        ("lyrics_tracks_count", int(stats.lyrics_tracks_count)),
        ("recent_setlists", PyValue::List(stats.recent_setlists.iter().map(setlist_json).collect())),
        ("config", dict([("has_root_path", PyValue::Bool(stats.has_root_path))])),
    ])
}

pub async fn get_dashboard(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| dashboard_stats(&conn)))
        .await
        .map(|stats| PyJson(stats_json(&stats)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn empty_and_populated_libraries() {
        let library = TestLibrary::new("dashboard");
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        assert_eq!(
            answer.json(),
            json!({
                "total_tracks": 0, "analyzed_tracks": 0, "unanalyzed_tracks": 0, "embedding_pending_tracks": 0,
                "genre_distribution": [],
                "unverified_genres_count": 0, "lyrics_tracks_count": 0, "recent_setlists": [],
                "config": {"has_root_path": false}
            })
        );
        library.execute(
            "INSERT INTO tracks (filepath, genre, bpm) VALUES ('/a', 'House', 120), ('/b', 'House', 0);
             INSERT INTO setlists (name, target_duration, rating, created_at, updated_at)
                VALUES ('Set', 1234.56, 4, '2026-01-02 03:04:05', '2026-01-02 03:04:05.000120');
             INSERT INTO settings VALUES ('root_path', '/music');",
        );
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        let body = answer.json();
        // Analyzed needs measured timbre and a stored waveform, not only a BPM.
        assert_eq!((body["analyzed_tracks"].clone(), body["unanalyzed_tracks"].clone()), (json!(0), json!(2)));
        assert_eq!(body["genre_distribution"], json!([{"name": "House", "count": 2}]));
        assert_eq!(body["unverified_genres_count"], json!(2));
        assert_eq!(
            body["recent_setlists"],
            json!([{
                "id": 1, "name": "Set", "description": null, "display_order": 0, "genre": null,
                "target_duration": f64::from(1234.56_f32), "rating": 4,
                "created_at": "2026-01-02T03:04:05", "updated_at": "2026-01-02T03:04:05.000120"
            }])
        );
        assert_eq!(body["config"], json!({"has_root_path": true}));
        // Python's json.dumps(allow_nan=False) fails on a NaN duration.
        library.execute("UPDATE setlists SET target_duration = 'nan'");
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        assert_eq!((answer.status.as_u16(), answer.body.as_str()), (500, "Internal Server Error"));
    }
}

/// `SaveFileRequest` / `RevealFileRequest`.
struct PathContent {
    path: String,
    content: Option<String>,
}

struct SaveFile(PathContent);
struct RevealFile(PathContent);

impl BodyModel for SaveFile {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let path = fields.required_str("path");
        let content = fields.required_str("content");
        fields.finish(|| Some(Self(PathContent { path: path?, content: Some(content?) })))
    }
}

impl BodyModel for RevealFile {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let path = fields.required_str("path");
        fields.finish(|| Some(Self(PathContent { path: path?, content: None })))
    }
}

/// `str(OSError)` as Python prints it.
fn os_error(error: &std::io::Error, path: &str) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let reason = text.split(" (os error").next().unwrap_or(&text).to_string();
            format!("[Errno {code}] {reason}: '{path}'")
        }
        None => error.to_string(),
    }
}

/// `POST /api/system/save-file`: write `content` (UTF-8) to `path`.
pub async fn save_file(headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_body_for::<SaveFile>(&headers, &body) {
        Ok(Ok(SaveFile(request))) => request,
        Ok(Err(errors)) => return crate::body::validation_response(&errors),
        Err(response) => return response,
    };
    let content = request.content.unwrap_or_default();
    match tokio::task::spawn_blocking(move || std::fs::write(&request.path, content.as_bytes()).map_err(|e| os_error(&e, &request.path))).await {
        Ok(Ok(())) => PyJson(dict([("status", text("success"))])).into_response(),
        Ok(Err(reason)) => HttpError(ApiError::new(500, format!("Failed to save file: {reason}"))).into_response(),
        Err(error) => HttpError(ApiError::new(500, format!("Failed to save file: {error}"))).into_response(),
    }
}

fn reveal(path: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg("-R").arg(path).status();
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::CommandExt;
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.into());
        // explorer parses its own command line; the quoted path keeps %, & and spaces.
        return std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", absolute.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = {
        let parent = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        std::process::Command::new("xdg-open").arg(parent).status()
    };
    #[cfg(not(windows))]
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("Command returned non-zero exit status {}.", status.code().unwrap_or(-1))),
        Err(error) => Err(error.to_string()),
    }
}

/// `POST /api/system/reveal-file`: show the file in the OS file manager.
pub async fn reveal_file(headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_body_for::<RevealFile>(&headers, &body) {
        Ok(Ok(RevealFile(request))) => request,
        Ok(Err(errors)) => return crate::body::validation_response(&errors),
        Err(response) => return response,
    };
    if request.path.contains('\0') || !std::path::Path::new(&request.path).exists() {
        tracing::info!("File not found for reveal: {}", request.path);
        return PyJson(dict([("status", text("error")), ("detail", text("File not found"))])).into_response();
    }
    match tokio::task::spawn_blocking(move || reveal(&request.path)).await {
        Ok(Ok(())) => PyJson(dict([("status", text("success"))])).into_response(),
        Ok(Err(reason)) => {
            tracing::info!("Failed to reveal file: {reason}");
            PyJson(dict([("status", text("error")), ("detail", text(reason))])).into_response()
        }
        Err(error) => PyJson(dict([("status", text("error")), ("detail", text(error.to_string()))])).into_response(),
    }
}
