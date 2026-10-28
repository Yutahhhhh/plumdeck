//! Genre lists and bulk edits (backend/api/routers/genres.py,
//! app/services/genre_app_service.py): `GET /api/genres/{list, subgenres,
//! unknown, unknown-ids, cleanup-suggestions}` and `POST /api/genres/
//! {batch-update, apply-analyses, cleanup-execute}`.
//!
//! The suggestion routes (`grouped-suggestions`) arrive with S4 and
//! `apply-to-files` (tag writing) with S8.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::{py_lower, py_strip};
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::genres::{
    all_genres, all_subgenres, batch_update_genres, execute_cleanup, tracks_with_genre, unknown_track_ids, unknown_tracks,
    write_genres, AnalysisMode, GenreWrite,
};
use plumdeck_db::repos::tracks::{get_track, BigInt, TrackRow};
use plumdeck_db::DbError;
use plumdeck_domain::genre_rules::{
    cleanup_key, confidence_is_not_low, label_map, normalize_genre_label, normalize_subgenre_label, primary_variant,
};

use crate::body::{parse_body, BodyModel, Fields, ValidationError};
use crate::models::{track_reads, TrackExtras};
use crate::query::Params;
use crate::response::{dict, float, int, opt, pydantic, text, PyJson};
use crate::{server_error, AppState, HttpError};

const MODES: [&str; 3] = ["genre", "subgenre", "both"];

fn mode(value: &str) -> AnalysisMode {
    AnalysisMode::parse(value).expect("validated mode")
}

fn strings(values: Vec<String>) -> PyValue {
    PyValue::List(values.into_iter().map(text).collect())
}

/// `GET /api/genres/list`.
pub async fn list_genres(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    state.blocking(move || db.lease().and_then(|conn| all_genres(&conn))).await.map(|v| pydantic(strings(v))).into_response()
}

/// `GET /api/genres/subgenres`.
pub async fn list_subgenres(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    state.blocking(move || db.lease().and_then(|conn| all_subgenres(&conn))).await.map(|v| pydantic(strings(v))).into_response()
}

/// `GET /api/genres/unknown`: `List[TrackRead]` of tracks needing a genre.
pub async fn unknown(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let offset = params.int_or("offset", 0);
    let limit = params.int_or("limit", 50);
    let mode_name = params.choice("mode", "genre", &MODES);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let rows = state
        .blocking(move || db.lease().and_then(|conn| unknown_tracks(&conn, BigInt(offset), BigInt(limit), mode(&mode_name))))
        .await;
    match rows {
        Ok(rows) => match track_reads(rows.iter().map(|row| (row, TrackExtras::default()))) {
            Some(value) => pydantic(value).into_response(),
            None => server_error(&"response validation failed (TrackRead)"),
        },
        Err(response) => response,
    }
}

/// `GET /api/genres/unknown-ids`.
pub async fn unknown_ids(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let mode_name = params.choice("mode", "genre", &MODES);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| unknown_track_ids(&conn, mode(&mode_name))))
        .await
        .map(|ids| pydantic(PyValue::List(ids.into_iter().map(int).collect())))
        .into_response()
}

/// A `TrackSuggestion`, or `None` when pydantic rejects it (a NULL title,
/// artist or bpm: the service raised, so the route answered 500).
fn suggestion(track: &TrackRow) -> Option<PyValue> {
    Some(dict([
        ("id", int(track.id)),
        ("title", text(track.title.clone()?)),
        ("artist", text(track.artist.clone()?)),
        ("bpm", float(track.bpm?)),
        ("filepath", text(track.filepath.clone())),
        ("current_genre", opt(track.genre.clone(), text)),
        ("similarity", PyValue::None),
    ]))
}

/// `get_cleanup_suggestions`: spelling variants grouped by [`cleanup_key`];
/// every track outside the primary spelling is a suggestion.
fn cleanup_groups(tracks: &[TrackRow], mode: AnalysisMode) -> Option<PyValue> {
    // key -> [(label, [tracks])], both in first-seen order.
    let mut groups: Vec<(String, Vec<(String, Vec<&TrackRow>)>)> = Vec::new();
    for track in tracks {
        let raw = if mode == AnalysisMode::Subgenre { &track.subgenre } else { &track.genre };
        let Some(raw) = raw.as_deref().filter(|raw| !raw.is_empty()) else { continue };
        let key = cleanup_key(raw);
        if key.is_empty() {
            continue;
        }
        let at = match groups.iter().position(|(existing, _)| *existing == key) {
            Some(at) => at,
            None => {
                groups.push((key, Vec::new()));
                groups.len() - 1
            }
        };
        let variants = &mut groups[at].1;
        match variants.iter_mut().find(|(label, _)| label == raw) {
            Some((_, members)) => members.push(track),
            None => variants.push((raw.to_owned(), vec![track])),
        }
    }
    let mut out: Vec<(usize, PyValue)> = Vec::new();
    for (_, variants) in &groups {
        if variants.len() < 2 {
            continue;
        }
        let counts: Vec<(&str, usize)> = variants.iter().map(|(label, members)| (label.as_str(), members.len())).collect();
        let primary = primary_variant(&counts);
        let mut suggestions = Vec::new();
        for (label, members) in variants {
            if label != primary {
                for track in members {
                    suggestions.push(suggestion(track)?);
                }
            }
        }
        if !suggestions.is_empty() {
            let count = suggestions.len();
            out.push((
                count,
                dict([
                    ("primary_genre", text(primary)),
                    ("variant_genres", PyValue::List(variants.iter().map(|(label, _)| text(label.clone())).collect())),
                    ("track_count", int(count as i64)),
                    ("suggestions", PyValue::List(suggestions)),
                ]),
            ));
        }
    }
    // list.sort(key=track_count, reverse=True) is stable.
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Some(PyValue::List(out.into_iter().map(|(_, group)| group).collect()))
}

/// `GET /api/genres/cleanup-suggestions`.
pub async fn cleanup_suggestions(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let mode_name = params.choice("mode", "genre", &MODES);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    match state.blocking(move || db.lease().and_then(|conn| tracks_with_genre(&conn))).await {
        Ok(tracks) => match cleanup_groups(&tracks, mode(&mode_name)) {
            Some(value) => pydantic(value).into_response(),
            None => server_error(&"TrackSuggestion validation failed"),
        },
        Err(response) => response,
    }
}

/// Ids beyond BIGINT match no track.
fn ids(values: Vec<i128>) -> Vec<i64> {
    values.into_iter().filter_map(|id| i64::try_from(id).ok()).collect()
}

/// `GenreBatchUpdateRequest`.
struct BatchUpdate {
    parent_track_id: i128,
    target_track_ids: Vec<i128>,
}

impl BodyModel for BatchUpdate {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let parent = fields.required_int("parent_track_id");
        let targets = fields.int_list("target_track_ids");
        fields.finish(|| Some(Self { parent_track_id: parent?, target_track_ids: targets? }))
    }
}

/// `POST /api/genres/batch-update`: copy the parent's genre to the targets.
pub async fn batch_update(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request: BatchUpdate = match parse_body(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Ok(parent) = i64::try_from(request.parent_track_id) else {
        return HttpError(ApiError::bad_request(plumdeck_db::repos::genres::PARENT_NOT_FOUND)).into_response();
    };
    let targets = ids(request.target_track_ids);
    let db = state.db.clone();
    match state.blocking_result(move || db.write(|conn| batch_update_genres(conn, parent, &targets))).await {
        Ok(Ok((count, genre))) => pydantic(dict([("updated_count", int(count)), ("genre", text(genre))])).into_response(),
        Ok(Err(DbError::Invalid(message))) => HttpError(ApiError::bad_request(message)).into_response(),
        Ok(Err(error)) => server_error(&error),
        Err(response) => response,
    }
}

/// `GenreCleanupRequest`.
struct CleanupRequest {
    target_genre: String,
    track_ids: Vec<i128>,
    mode: String,
}

impl BodyModel for CleanupRequest {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let target = fields.required_str("target_genre");
        let track_ids = fields.int_list("track_ids");
        let mode = fields.choice_or("mode", "genre", &MODES);
        fields.finish(|| Some(Self { target_genre: target?, track_ids: track_ids?, mode }))
    }
}

/// `POST /api/genres/cleanup-execute`: set one spelling on the listed tracks.
pub async fn cleanup_execute(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request: CleanupRequest = match parse_body(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let track_ids = ids(request.track_ids);
    let target = request.target_genre.clone();
    let mode = mode(&request.mode);
    let db = state.db.clone();
    state
        .blocking(move || db.write(|conn| execute_cleanup(conn, &target, &track_ids, mode)))
        .await
        .map(|count| PyJson(dict([("updated_count", int(count)), ("genre", text(request.target_genre))])))
        .into_response()
}

/// `GenreAnalysisApplyItem`.
struct AnalysisItem {
    track_id: i128,
    genre: Option<String>,
    subgenre: Option<String>,
    confidence: String,
}

/// `GenreAnalysisApplyRequest`.
struct ApplyRequest {
    analyses: Vec<AnalysisItem>,
    mode: String,
    overwrite: bool,
}

impl BodyModel for ApplyRequest {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let analyses = fields.model_list("analyses", |item| {
            let track_id = item.required_int("track_id");
            let genre = item.optional_str("genre");
            let subgenre = item.optional_str("subgenre");
            let confidence = item.str_or("confidence", "Medium");
            let _reason = item.str_or("reason", "");
            Some(AnalysisItem { track_id: track_id?, genre, subgenre, confidence })
        });
        let mode = fields.choice_or("mode", "both", &MODES);
        let overwrite = fields.bool_or("overwrite", false);
        fields.finish(|| Some(Self { analyses: analyses?, mode, overwrite }))
    }
}

/// Why `apply_genre_analyses` answered 500: a `GenreUpdateResult` with a
/// NULL title or artist (nothing was committed).
struct ResultInvalid;

/// `apply_genre_analyses`: the session's view of each track, every change
/// committed once at the end. Returns the `results` entries.
fn apply_analyses(
    conn: &plumdeck_db::duckdb::Connection,
    request: &ApplyRequest,
) -> Result<Result<Vec<PyValue>, ResultInvalid>, DbError> {
    let mode = mode(&request.mode);
    let mut session: HashMap<i64, TrackRow> = HashMap::new();
    let mut touched: Vec<i64> = Vec::new();
    let mut genre_map = None;
    let mut subgenre_map = None;
    let mut results = Vec::new();
    for item in &request.analyses {
        let Ok(track_id) = i64::try_from(item.track_id) else { continue };
        if !session.contains_key(&track_id) {
            match get_track(conn, BigInt(item.track_id))? {
                Some(row) => {
                    session.insert(track_id, row);
                }
                None => continue,
            }
        }
        let track = session.get_mut(&track_id).expect("loaded");
        let old_genre = track.genre.clone().filter(|genre| !genre.is_empty()).unwrap_or_else(|| "Unknown".into());
        // _normalize_analysis_data
        let genre = match &item.genre {
            Some(value) => {
                let map = match &genre_map {
                    Some(map) => map,
                    None => genre_map.insert(label_map(&all_genres(conn)?)),
                };
                normalize_genre_label(value, map)
            }
            None => track.genre.clone().filter(|genre| !genre.is_empty()).unwrap_or_else(|| "Unknown".into()),
        };
        let subgenre = match &item.subgenre {
            Some(value) => {
                let map = match &subgenre_map {
                    Some(map) => map,
                    None => subgenre_map.insert(label_map(&all_subgenres(conn)?)),
                };
                normalize_subgenre_label(value, map)
            }
            None => track.subgenre.clone().unwrap_or_default(),
        };
        // apply_genre_analysis
        let can_update_genre = request.overwrite
            || track.is_genre_verified != Some(true)
            || track.genre.as_deref().is_none_or(str::is_empty)
            || track.genre.as_deref().is_some_and(|genre| py_lower(genre) == "unknown");
        let can_update_subgenre = request.overwrite || track.subgenre.as_deref().is_none_or(str::is_empty);
        if matches!(mode, AnalysisMode::Genre | AnalysisMode::Both) && can_update_genre {
            track.genre = Some(genre);
        }
        if matches!(mode, AnalysisMode::Subgenre | AnalysisMode::Both) && can_update_subgenre {
            track.subgenre = Some(subgenre);
        }
        let applied = py_lower(py_strip(track.genre.as_deref().unwrap_or("")));
        track.is_genre_verified = Some(!applied.is_empty() && applied != "unknown" && confidence_is_not_low(&item.confidence));
        if !touched.contains(&track_id) {
            touched.push(track_id);
        }
        // GenreUpdateResult(title=..., artist=...) validates str fields.
        let (Some(title), Some(artist)) = (track.title.clone(), track.artist.clone()) else {
            return Ok(Err(ResultInvalid));
        };
        results.push(dict([
            ("track_id", int(track.id)),
            ("title", text(title)),
            ("artist", text(artist)),
            ("old_genre", text(old_genre)),
            ("new_genre", text(track.genre.clone().unwrap_or_default())),
        ]));
    }
    let writes: Vec<GenreWrite> = touched
        .iter()
        .map(|id| {
            let track = &session[id];
            GenreWrite {
                track_id: *id,
                genre: track.genre.clone(),
                subgenre: track.subgenre.clone(),
                is_genre_verified: track.is_genre_verified == Some(true),
            }
        })
        .collect();
    write_genres(conn, &writes)?;
    Ok(Ok(results))
}

/// `POST /api/genres/apply-analyses`: apply classifications from an MCP
/// client's model.
pub async fn apply_genre_analyses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request: ApplyRequest = match parse_body(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| apply_analyses(conn, &request))).await {
        Ok(Ok(results)) => PyJson(dict([("results", PyValue::List(results))])).into_response(),
        Ok(Err(ResultInvalid)) => server_error(&"GenreUpdateResult validation failed"),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    const ROWS: &str = "INSERT INTO tracks (filepath, title, artist, genre, subgenre, bpm, is_genre_verified, created_at) VALUES
        ('/1', 'A', 'X', 'Drum & Bass', 'Liquid', 174, true, '2026-01-01 00:00:00'),
        ('/2', 'B', 'X', 'drum and bass', 'liquid', 172, true, '2026-01-01 00:00:00'),
        ('/3', 'C', 'X', 'DnB', '', 170, false, '2026-01-01 00:00:00'),
        ('/4', 'D', 'X', 'Unknown', NULL, 120, true, '2026-01-01 00:00:00'),
        ('/5', 'E', 'X', 'House', 'Deep', 124, true, '2026-01-01 00:00:00');";

    #[tokio::test]
    async fn lists_and_cleanup_suggestions() {
        let library = TestLibrary::new("genres");
        library.execute(ROWS);
        let app = || router(library.state());
        let list = send(app(), Method::GET, "/api/genres/list", &[], "").await.json();
        assert_eq!(list, json!(["DnB", "Drum & Bass", "House", "drum and bass"]));
        assert_eq!(send(app(), Method::GET, "/api/genres/unknown-ids?mode=both", &[], "").await.json(), json!([3, 4]));
        assert_eq!(send(app(), Method::GET, "/api/genres/unknown-ids?mode=x", &[], "").await.status.as_u16(), 422);
        let unknown = send(app(), Method::GET, "/api/genres/unknown?limit=1&offset=1", &[], "").await;
        assert_eq!(unknown.status.as_u16(), 500, "track 4 has no key/scale/duration: TrackRead fails");
        let groups = send(app(), Method::GET, "/api/genres/cleanup-suggestions", &[], "").await.json();
        assert_eq!(groups[0]["primary_genre"], "Drum & Bass");
        assert_eq!(groups[0]["variant_genres"], json!(["Drum & Bass", "drum and bass", "DnB"]));
        assert_eq!(groups[0]["track_count"], 2);
        let sub = send(app(), Method::GET, "/api/genres/cleanup-suggestions?mode=subgenre", &[], "").await.json();
        assert_eq!(sub[0]["suggestions"][0]["current_genre"], "drum and bass");
    }

    #[tokio::test]
    async fn bulk_edits_and_analyses() {
        let library = TestLibrary::new("genre-edits");
        library.execute(ROWS);
        let app = || router(library.state());
        let json_type = [("content-type", "application/json")];
        let answer = send(app(), Method::POST, "/api/genres/batch-update", &json_type, r#"{"parent_track_id":5,"target_track_ids":[3,4,99]}"#).await;
        assert_eq!(answer.json(), json!({"updated_count": 2, "genre": "House"}));
        let answer = send(app(), Method::POST, "/api/genres/batch-update", &json_type, r#"{"parent_track_id":99,"target_track_ids":[]}"#).await;
        assert_eq!((answer.status.as_u16(), answer.body.as_str()), (400, r#"{"detail":"Parent track not found"}"#));
        let answer = send(app(), Method::POST, "/api/genres/cleanup-execute", &json_type, r#"{"target_genre":"DnB","track_ids":[1,2],"mode":"genre"}"#).await;
        assert_eq!(answer.json(), json!({"updated_count": 2, "genre": "DnB"}));
        let body = r#"{"analyses":[{"track_id":3,"genre":"\"dnb\"","confidence":"high"},{"track_id":3,"subgenre":"trap soul"},{"track_id":42}],"overwrite":true}"#;
        let answer = send(app(), Method::POST, "/api/genres/apply-analyses", &json_type, body).await;
        assert_eq!(
            answer.json(),
            json!({"results": [
                {"track_id": 3, "title": "C", "artist": "X", "old_genre": "House", "new_genre": "DnB"},
                {"track_id": 3, "title": "C", "artist": "X", "old_genre": "DnB", "new_genre": "DnB"}
            ]})
        );
        let bad = send(app(), Method::POST, "/api/genres/apply-analyses", &json_type, r#"{"analyses":[{"genre":"x"}]}"#).await;
        assert_eq!(bad.status.as_u16(), 422);
    }
}
