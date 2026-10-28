//! `GET /api/tracks/{track_id}/lyrics` and `GET /api/lyrics/search`
//! (backend/api/routers/lyrics.py), and `PUT /api/tracks/{track_id}/lyrics`,
//! which the frontend's lyrics editor calls and Python no longer had
//! (405; restored by deviation D-02).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::py_lower;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::lyrics::{get_lyrics, put_lyrics, search_lyrics};
use plumdeck_db::repos::tracks::BigInt;
use plumdeck_domain::lrc::{clean_line, parse_timestamp};

use crate::body::{parse_body_for, BodyModel, Fields, ValidationError};
use crate::models::{lyrics_read, track_dict};
use crate::query::Params;
use crate::response::{boolean, dict, float, opt, pydantic, text, PyJson};
use crate::{server_error, AppState, HttpError};

/// `GET /api/tracks/{track_id}/lyrics`: `LyricsRead`, or 404.
pub async fn get_track_lyrics(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let track_id = params.path_int("track_id", &track_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let track_id = BigInt(track_id.expect("validated"));
    let db = state.db.clone();
    match state.blocking(move || db.lease().and_then(|conn| get_lyrics(&conn, track_id))).await {
        Ok(Some(row)) => match lyrics_read(&row) {
            Some(value) => pydantic(value).into_response(),
            None => server_error(&"response validation failed (LyricsRead)"),
        },
        Ok(None) => HttpError(ApiError::new(404, "Lyrics not found")).into_response(),
        Err(response) => response,
    }
}

/// The body of `PUT /api/tracks/{track_id}/lyrics`: `content` and an
/// optional `language`. The frontend also sends `track_id`; the path wins.
struct LyricsPut {
    content: String,
    language: Option<String>,
}

impl BodyModel for LyricsPut {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let content = fields.required_str("content");
        let language = fields.optional_str("language");
        fields.finish(|| Some(Self { content: content?, language }))
    }
}

/// `PUT /api/tracks/{track_id}/lyrics` (D-02): store the user's lyrics and
/// answer the stored row (`LyricsRead`); 404 for a missing track.
pub async fn put_track_lyrics(
    State(state): State<AppState>,
    Path(track_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed = match parse_body_for::<LyricsPut>(&headers, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let track_id = params.path_int("track_id", &track_id);
    let update = match parsed {
        Ok(update) => Some(update),
        Err(errors) => {
            params.extend(errors);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (track_id, update) = (BigInt(track_id.expect("validated")), update.expect("validated"));
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let stored = state
        .blocking(move || db.write(|conn| put_lyrics(conn, track_id, &update.content, update.language.as_deref(), now)))
        .await;
    match stored {
        Ok(Some(row)) => match lyrics_read(&row) {
            Some(value) => pydantic(value).into_response(),
            None => server_error(&"response validation failed (LyricsRead)"),
        },
        Ok(None) => HttpError(ApiError::new(404, "Track not found")).into_response(),
        Err(response) => response,
    }
}

/// One search hit: the first line containing `q` (case-insensitive), with
/// one line of context on each side, LRC time tags removed.
fn hit(q: &str, content: &str) -> Result<Option<(Vec<String>, Option<f64>, String)>, ()> {
    let needle = py_lower(q);
    let lines: Vec<&str> = content.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if !py_lower(line).contains(&needle) {
            continue;
        }
        let timestamp = parse_timestamp(line).map_err(|_| ())?;
        let start = index.saturating_sub(1);
        let end = (index + 2).min(lines.len());
        let snippet = lines[start..end].iter().map(|line| clean_line(line)).collect();
        return Ok(Some((snippet, timestamp, clean_line(line))));
    }
    Ok(None)
}

/// `GET /api/lyrics/search?q=...&exclude_track_id=...`.
pub async fn search(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let q = params.required_str("q");
    let exclude = params.opt_int("exclude_track_id");
    if let Err(response) = params.finish() {
        return response;
    }
    let q = q.expect("validated");
    if q.chars().count() < 3 {
        return PyJson(PyValue::List(Vec::new())).into_response();
    }
    let db = state.db.clone();
    let pattern = q.clone();
    let rows = match state.blocking(move || db.lease().and_then(|conn| search_lyrics(&conn, &pattern, exclude.map(BigInt)))).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut answer = Vec::new();
    for (content, track) in &rows {
        match hit(&q, content) {
            Ok(Some((snippet, timestamp, matched))) => answer.push(dict([
                ("track", track_dict(track, vec![("has_lyrics", boolean(true))])),
                ("snippet", PyValue::List(snippet.into_iter().map(text).collect())),
                ("timestamp", opt(timestamp, float)),
                ("matched_text", text(matched)),
            ])),
            Ok(None) => {}
            Err(()) => return server_error(&"lyrics timestamp out of range"),
        }
    }
    PyJson(PyValue::List(answer)).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn reads_and_searches_lyrics() {
        let library = TestLibrary::new("lyrics");
        library.execute(
            "INSERT INTO tracks (filepath, title, created_at) VALUES ('/a', 'A', '2026-01-01 00:00:00'), ('/b', 'B', '2026-01-01 00:00:00');
             INSERT INTO lyrics (track_id, content, created_at, updated_at) VALUES
               (1, '[00:01.00]intro\n[00:12.50]Hello World [x]\noutro', '2026-01-01 00:00:00', '2026-01-02 00:00:00'),
               (2, 'hello', NULL, NULL);",
        );
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/api/tracks/1/lyrics", &[], "").await;
        assert_eq!(answer.json()["source"], "user");
        assert_eq!(send(app(), Method::GET, "/api/tracks/2/lyrics", &[], "").await.status.as_u16(), 500);
        assert_eq!(send(app(), Method::GET, "/api/tracks/3/lyrics", &[], "").await.status.as_u16(), 404);
        let found = send(app(), Method::GET, "/api/lyrics/search?q=WORLD&exclude_track_id=2", &[], "").await.json();
        assert_eq!(found[0]["snippet"], json!(["intro", "Hello World [x]", "outro"]));
        assert_eq!(found[0]["timestamp"], json!(12.5));
        assert_eq!(found[0]["matched_text"], "Hello World [x]");
        assert_eq!(found[0]["track"]["has_lyrics"], true);
        assert_eq!(send(app(), Method::GET, "/api/lyrics/search?q=he", &[], "").await.json(), json!([]));
        assert_eq!(send(app(), Method::GET, "/api/lyrics/search", &[], "").await.status.as_u16(), 422);
    }

    #[tokio::test]
    async fn put_restores_the_lyrics_editor_route() {
        let library = TestLibrary::new("lyrics-put");
        library.execute("INSERT INTO tracks (filepath, title) VALUES ('/a', 'A');");
        let app = || router(library.state());
        let json_type = [("content-type", "application/json")];
        let answer = send(app(), Method::PUT, "/api/tracks/1/lyrics", &json_type, r#"{"track_id": 1, "content": "la\nla"}"#).await;
        assert_eq!(answer.status.as_u16(), 200);
        let body = answer.json();
        assert_eq!((body["content"].clone(), body["source"].clone(), body["language"].clone()), (json!("la\nla"), json!("user"), json!(null)));
        assert_eq!(body["created_at"], body["updated_at"]);
        assert_eq!(send(app(), Method::GET, "/api/tracks/1/lyrics", &[], "").await.json()["content"], "la\nla");
        assert_eq!(send(app(), Method::PUT, "/api/tracks/2/lyrics", &json_type, r#"{"content": "x"}"#).await.status.as_u16(), 404);
        assert_eq!(send(app(), Method::PUT, "/api/tracks/1/lyrics", &json_type, r#"{"track_id": 1}"#).await.status.as_u16(), 422);
    }
}
