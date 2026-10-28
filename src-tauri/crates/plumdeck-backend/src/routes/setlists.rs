//! The setlist and recommendation routes (backend/api/routers/setlists.py
//! over backend/app/services/setlist_app_service.py, S4).
//!
//! `GET /api/setlists/{id}/duration` is not ported (no caller; deviation
//! D-17). Pool tracks are dumped the way Python dumps the `Track` objects it
//! builds from the pool query: the selected columns, the model defaults for
//! the others (`scale`, `analysis_level`, the advanced features,
//! `is_genre_verified`) and `created_at` = now.

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::compat::pyobj::int_from_str;
use plumdeck_core::compat::text::py_strip;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::recommendations::{candidate_pool, embedding, ranked_page, PoolQuery, PoolRow, RankTarget};
use plumdeck_db::repos::setlists::{
    create_setlist, delete_setlist, get_setlist, list_setlists, replace_setlist_tracks, set_entry_wordplay,
    setlist_tracks, update_setlist, EntryRow, ReplaceError, SetlistRow, SETLIST_FIELDS,
};
use plumdeck_db::repos::settings::setting_value;
use plumdeck_db::repos::tracks::{get_track, tracks_by_ids, BigInt, TrackRow};
use plumdeck_db::repos::wordplay::{approved_edges, PairRow};
use plumdeck_db::DbError;
use plumdeck_domain::mixability::{cosine_similarity, mixability_score, parse_embedding, DEFAULT_WEIGHTS};
use plumdeck_domain::setlist_builder::{build_chain, build_path, Edges, Node, Pick, APPROVED_WORDPLAY_BONUS};
use plumdeck_domain::targets::{sanitize, RawTargets, Targets};

use crate::body::{parse_embedded, read_body, BodyModel, Fields, Loc, ValidationError};
use crate::models::{track_dict, track_dump};
use crate::query::Params;
use crate::response::{boolean, datetime, dict, float, int, opt, text, PyJson};
use crate::{server_error, AppState, HttpError};

fn not_found(detail: &str) -> Response {
    HttpError(ApiError::new(404, detail)).into_response()
}

/// A setlist as `jsonable_encoder` writes the model.
fn setlist_json(row: &SetlistRow) -> PyValue {
    dict([
        ("id", int(row.id)),
        ("name", opt(row.name.clone(), text)),
        ("description", opt(row.description.clone(), text)),
        ("display_order", opt(row.display_order, int)),
        ("genre", opt(row.genre.clone(), text)),
        ("target_duration", opt(row.target_duration, float)),
        ("rating", opt(row.rating, int)),
        ("created_at", opt(row.created_at.as_ref(), datetime)),
        ("updated_at", opt(row.updated_at.as_ref(), datetime)),
    ])
}

/// A `SetlistTrack` as `jsonable_encoder` writes the model.
fn entry_json(row: &EntryRow) -> PyValue {
    dict([
        ("id", int(row.id)),
        ("setlist_id", int(row.setlist_id)),
        ("track_id", int(row.track_id)),
        ("position", int(row.position)),
        ("transition_note", opt(row.transition_note.clone(), text)),
        ("wordplay_json", opt(row.wordplay_json.clone(), text)),
        ("in_ms", float(row.in_ms)),
        ("out_ms", opt(row.out_ms, float)),
        ("playback_rate", float(row.playback_rate)),
        ("extra_duration_ms", float(row.extra_duration_ms)),
        ("overlap_next_ms", float(row.overlap_next_ms)),
        ("revision", int(row.revision)),
        ("created_at", opt(row.created_at.as_ref(), datetime)),
    ])
}

fn path_id(params: &mut Params, name: &str, value: &str) -> Option<BigInt> {
    params.path_int(name, value).map(BigInt)
}

/// `GET /api/setlists`.
pub async fn get_setlists(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    match state.blocking(move || db.lease().and_then(|conn| list_setlists(&conn))).await {
        Ok(rows) => PyJson(PyValue::List(rows.iter().map(setlist_json).collect())).into_response(),
        Err(response) => response,
    }
}

struct CreateBody {
    name: String,
}

impl BodyModel for CreateBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let name = fields.required_str("name");
        fields.finish(|| Some(Self { name: name? }))
    }
}

/// `POST /api/setlists` (`{"name": ...}`).
pub async fn post_setlist(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let created = match parse_embedded::<CreateBody>(&headers, &body) {
        Ok(created) => created,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| create_setlist(conn, &created.name, now))).await {
        Ok(row) => PyJson(setlist_json(&row)).into_response(),
        Err(response) => response,
    }
}

/// Instance attributes of a `Setlist` other than its fields (`dir()`):
/// `setattr` on one raises, a 500. Names starting with `_` are left out:
/// Python sets most of them on the instance with unpredictable effects
/// (deviation D-18), the port ignores them like unknown keys.
const MODEL_ATTRIBUTES: [&str; 30] = [
    "construct", "copy", "dict", "from_orm", "json", "metadata", "model_computed_fields", "model_config",
    "model_construct", "model_copy", "model_dump", "model_dump_json", "model_extra", "model_fields",
    "model_fields_set", "model_json_schema", "model_parametrized_name", "model_post_init", "model_rebuild",
    "model_validate", "model_validate_json", "model_validate_strings", "parse_file", "parse_obj", "parse_raw",
    "schema", "schema_json", "sqlmodel_update", "update_forward_refs", "validate",
];

/// `PUT /api/setlists/{id}`: any JSON object; model fields are set as sent.
pub async fn put_setlist(State(state): State<AppState>, Path(setlist_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    let changes: Option<Vec<(String, PyValue)>> = match &body {
        Some(PyValue::Dict(pairs)) => {
            Some(pairs.iter().filter_map(|(key, value)| match key {
                PyValue::Str(key) => Some((key.clone(), value.clone())),
                _ => None,
            }).collect())
        }
        other => {
            let (kind, msg) = match other {
                None => ("missing", "Field required"),
                Some(_) => ("dict_type", "Input should be a valid dictionary"),
            };
            params.extend(vec![ValidationError::new(kind, vec![Loc::from("body")], msg, other.clone().unwrap_or(PyValue::None))]);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, changes) = (id.expect("validated"), changes.expect("validated"));
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                let Some(_) = get_setlist(conn, id)? else { return Ok(Err(not_found("Setlist not found"))) };
                // setattr runs key by key: a model attribute fails the request.
                if changes.iter().any(|(key, _)| MODEL_ATTRIBUTES.contains(&key.as_str())) {
                    return Ok(Err(server_error(&"setattr on a model attribute")));
                }
                let fields: Vec<(String, PyValue)> =
                    changes.into_iter().filter(|(key, _)| SETLIST_FIELDS.contains(&key.as_str())).collect();
                Ok(match update_setlist(conn, id, &fields, now)? {
                    Some(row) => Ok(row),
                    None => Err(server_error(&"setlist vanished after update")),
                })
            })
        })
        .await;
    match result {
        Ok(Ok(row)) => PyJson(setlist_json(&row)).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

/// `DELETE /api/setlists/{id}`.
pub async fn remove_setlist(State(state): State<AppState>, Path(setlist_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| delete_setlist(conn, id))).await {
        Ok(true) => PyJson(dict([("ok", boolean(true))])).into_response(),
        Ok(false) => not_found("Setlist not found"),
        Err(response) => response,
    }
}

/// `GET /api/setlists/{id}/tracks`: the entries with their tracks (an
/// unknown setlist has none).
pub async fn get_setlist_tracks(State(state): State<AppState>, Path(setlist_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    match state.blocking(move || db.lease().and_then(|conn| setlist_tracks(&conn, id))).await {
        Ok(rows) => {
            let items = rows
                .iter()
                .map(|row| {
                    let entry = &row.entry;
                    track_dict(
                        &row.track,
                        vec![
                            ("setlist_track_id", int(entry.id)),
                            ("position", int(entry.position)),
                            ("wordplay_json", opt(entry.wordplay_json.clone(), text)),
                            ("in_ms", float(entry.in_ms)),
                            ("out_ms", opt(entry.out_ms, float)),
                            ("playback_rate", float(entry.playback_rate)),
                            ("extra_duration_ms", float(entry.extra_duration_ms)),
                            ("overlap_next_ms", float(entry.overlap_next_ms)),
                            ("revision", int(entry.revision)),
                            ("has_lyrics", boolean(row.has_lyrics())),
                        ],
                    )
                })
                .collect();
            PyJson(PyValue::List(items)).into_response()
        }
        Err(response) => response,
    }
}

/// `POST /api/setlists/{id}/tracks`: the whole ordered entry list.
pub async fn post_setlist_tracks(State(state): State<AppState>, Path(setlist_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    let items = match body {
        Some(PyValue::List(items)) => Some(items),
        other => {
            let (kind, msg) = if other.is_none() { ("missing", "Field required") } else { ("list_type", "Input should be a valid list") };
            params.extend(vec![ValidationError::new(kind, vec![Loc::from("body")], msg, other.unwrap_or(PyValue::None))]);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, items) = (id.expect("validated"), items.expect("validated"));
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || db.write(|conn| Ok(replace_setlist_tracks(conn, id, &items, now))))
        .await;
    match result {
        Ok(Ok(Ok(true))) => PyJson(dict([("status", text("success"))])).into_response(),
        Ok(Ok(Ok(false))) => not_found("Setlist not found"),
        Ok(Ok(Err(ReplaceError::Conflict(detail)))) => HttpError(ApiError::conflict(detail)).into_response(),
        Ok(Ok(Err(ReplaceError::Server(reason)))) => server_error(&reason),
        Ok(Ok(Err(ReplaceError::Db(error)))) | Ok(Err(error)) => server_error(&error),
        Err(response) => response,
    }
}

/// `filename*=UTF-8''...`: `urllib.parse.quote` (unreserved and `/` kept).
pub fn quote_path(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `int(track.duration) if track.duration else -1`; `Err` is the
/// ValueError of a NaN (answered 404 with its message, as in Python).
fn extinf_duration(duration: Option<f64>) -> Result<String, Response> {
    match duration {
        None => Ok("-1".into()),
        Some(seconds) if seconds == 0.0 => Ok("-1".into()),
        Some(seconds) if seconds.is_nan() => Err(not_found("cannot convert float NaN to integer")),
        Some(seconds) if seconds.is_infinite() => Err(server_error(&"cannot convert float infinity to integer")),
        Some(seconds) => Ok(format!("{}", seconds.trunc() as i128)),
    }
}

fn exists(path: &str) -> bool {
    !path.contains('\0') && std::path::Path::new(path).exists()
}

/// `GET /api/setlists/{id}/export/m3u8`.
pub async fn export_m3u8(State(state): State<AppState>, Path(setlist_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    let loaded = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(setlist) = get_setlist(&conn, id)? else { return Ok(None) };
            Ok(Some((setlist, setlist_tracks(&conn, id)?)))
        })
        .await;
    let (setlist, rows) = match loaded {
        Ok(Some(found)) => found,
        Ok(None) => return not_found("Setlist not found"),
        Err(response) => return response,
    };
    let mut lines = vec!["#EXTM3U".to_string()];
    for row in &rows {
        let track = &row.track;
        let duration = match extinf_duration(track.duration) {
            Ok(duration) => duration,
            Err(response) => return response,
        };
        let artist = track.artist.as_deref().filter(|value| !value.is_empty()).unwrap_or("Unknown Artist");
        let title = track.title.as_deref().filter(|value| !value.is_empty()).unwrap_or("Unknown Title");
        if !track.filepath.is_empty() && !exists(&track.filepath) {
            lines.push(format!("# MISSING: {artist} - {title} ({})", track.filepath));
            continue;
        }
        lines.push(format!("#EXTINF:{duration},{artist} - {title}"));
        if !track.filepath.is_empty() {
            lines.push(track.filepath.clone());
        }
    }
    let name = setlist.name.as_deref().unwrap_or("None");
    let filename: String = format!("{name}.m3u8").chars().filter(|c| !"\\/*?:\"<>|".contains(*c)).collect();
    let disposition = format!("attachment; filename*=UTF-8''{}", quote_path(&filename));
    let mut response = lines.join("\n").into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-mpegurl"));
    match HeaderValue::from_str(&disposition) {
        Ok(value) => {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
        Err(error) => return server_error(&error),
    }
    response
}

/// `GET /api/setlists/{id}/export/validate`.
pub async fn validate_export(State(state): State<AppState>, Path(setlist_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_id", &setlist_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    let loaded = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(_) = get_setlist(&conn, id)? else { return Ok(None) };
            Ok(Some(setlist_tracks(&conn, id)?))
        })
        .await;
    let rows = match loaded {
        Ok(Some(rows)) => rows,
        Ok(None) => return not_found("Setlist not found"),
        Err(response) => return response,
    };
    let missing: Vec<PyValue> = rows
        .iter()
        .filter(|row| row.track.filepath.is_empty() || !exists(&row.track.filepath))
        .map(|row| {
            let track = &row.track;
            dict([
                ("id", int(track.id)),
                ("title", opt(track.title.clone(), text)),
                ("artist", opt(track.artist.clone(), text)),
                ("filepath", text(track.filepath.clone())),
            ])
        })
        .collect();
    PyJson(dict([("total", int(rows.len() as i64)), ("missing", PyValue::List(missing))])).into_response()
}

// ------------------------------------------------------------------ recommendations

/// A pool `Track(...)` as `model_dump()` writes it.
pub(crate) fn pool_track(row: &PoolRow, now: NaiveDateTime) -> Vec<(&'static str, PyValue)> {
    let s = |value: &Option<String>| opt(value.clone(), text);
    let f = |value: Option<f64>| opt(value, float);
    vec![
        ("id", int(row.id)),
        ("filepath", text(row.filepath.clone())),
        ("title", s(&row.title)),
        ("artist", s(&row.artist)),
        ("album", text(row.album.clone().filter(|album| !album.is_empty()).unwrap_or_default())),
        ("genre", s(&row.genre)),
        ("subgenre", text(row.subgenre.clone().filter(|subgenre| !subgenre.is_empty()).unwrap_or_default())),
        ("year", opt(row.year, int)),
        ("bpm", f(row.bpm)),
        ("key", s(&row.key)),
        ("scale", text("")),
        ("duration", f(row.duration)),
        ("analysis_level", PyValue::None),
        ("energy", f(row.energy)),
        ("danceability", f(row.danceability)),
        ("loudness", f(row.loudness)),
        ("brightness", f(row.brightness)),
        ("noisiness", f(row.noisiness)),
        ("contrast", f(row.contrast)),
        ("loudness_range", float(0.0)),
        ("spectral_flux", float(0.0)),
        ("spectral_rolloff", float(0.0)),
        ("is_genre_verified", boolean(false)),
        ("created_at", datetime(&now)),
        ("has_lyrics", boolean(row.has_lyrics)),
    ]
}

fn set(pairs: &mut Vec<(&'static str, PyValue)>, key: &'static str, value: PyValue) {
    match pairs.iter_mut().find(|(name, _)| *name == key) {
        Some(slot) => slot.1 = value,
        None => pairs.push((key, value)),
    }
}

fn node(row: &PoolRow, only_from: Option<BTreeSet<i64>>) -> Node {
    Node {
        id: row.id,
        artist: row.artist.clone(),
        bpm: row.bpm,
        energy: row.energy,
        danceability: row.danceability,
        brightness: row.brightness,
        noisiness: row.noisiness,
        vector: row.vector.clone(),
        embedding_model: row.embedding_model.clone(),
        wordplay_only_from: only_from,
    }
}

/// `_wordplay_payload` as `json.dumps(..., ensure_ascii=False)`.
fn wordplay_payload(pair: &PairRow) -> Result<String, Response> {
    let f = |value: Option<f64>| opt(value, float);
    let payload = dict([
        ("pair_id", int(pair.id)),
        ("from_track_id", int(pair.from_track_id)),
        ("to_track_id", int(pair.to_track_id)),
        ("keyword", text(pair.keyword.clone())),
        ("source_phrase", text(pair.source_phrase.clone())),
        ("target_phrase", text(pair.target_phrase.clone())),
        ("source_cue_mode", text(pair.source_cue_mode.clone())),
        ("from_timestamp", f(pair.from_timestamp)),
        ("source_cue_end_timestamp", f(pair.source_cue_end_timestamp)),
        ("to_timestamp", f(pair.to_timestamp)),
        ("target_intro_timestamp", f(pair.target_intro_timestamp)),
        ("target_landing_timestamp", f(pair.target_landing_timestamp.or(pair.to_timestamp))),
        ("transition_notes", text(pair.transition_notes.clone())),
        ("source_url", text(pair.source_url.clone())),
        ("evidence_type", text(pair.evidence_type.clone())),
        ("verification_status", text(pair.verification_status.clone())),
    ]);
    let options = DumpOptions { ensure_ascii: false, ..DumpOptions::default() };
    dumps(&payload, &options).map_err(|error| server_error(&error))
}

/// Approved edges by `(from, to)`.
fn edge_map(pairs: Vec<PairRow>) -> BTreeMap<(i64, i64), PairRow> {
    pairs.into_iter().map(|pair| ((pair.from_track_id, pair.to_track_id), pair)).collect()
}

/// `_include_wordplay_candidates`: approved targets beyond the pool cap,
/// under the same filters, eligible only after their sources.
fn with_wordplay_targets(
    conn: &plumdeck_db::duckdb::Connection,
    mut pool: Vec<(PoolRow, Option<BTreeSet<i64>>)>,
    edges: &BTreeMap<(i64, i64), PairRow>,
    query: &PoolQuery<'_>,
) -> Result<Vec<(PoolRow, Option<BTreeSet<i64>>)>, DbError> {
    let present: BTreeSet<i64> = pool.iter().map(|(row, _)| row.id).collect();
    let missing: Vec<i64> = edges.keys().map(|(_, to)| *to).collect::<BTreeSet<_>>().difference(&present).copied().collect();
    if missing.is_empty() {
        return Ok(pool);
    }
    let extra = candidate_pool(conn, &PoolQuery { candidate_ids: Some(&missing), limit: missing.len() as i64, ..query.clone() })?;
    for row in extra {
        let sources: BTreeSet<i64> = edges.keys().filter(|(_, to)| *to == row.id).map(|(from, _)| *from).collect();
        pool.push((row, Some(sources)));
    }
    Ok(pool)
}

/// Python list slicing `items[:limit]`.
fn head<T>(mut items: Vec<T>, limit: i128) -> Vec<T> {
    let len = items.len() as i128;
    let end = if limit < 0 { (len + limit).max(0) } else { limit.min(len) };
    items.truncate(end as usize);
    items
}

fn track_not_found() -> Response {
    not_found("Track not found")
}

/// `GET /api/recommendations/next`.
pub async fn recommend_next(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let track_id = params.required_int("track_id");
    let limit = params.int_or("limit", 20);
    let genres = params.list("genres");
    let subgenres = params.list("subgenres");
    if let Err(response) = params.finish() {
        return response;
    }
    let track_id = BigInt(track_id.expect("validated"));
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(target) = get_track(&conn, track_id)? else { return Ok(Err(track_not_found())) };
            let targets = Targets { bpm: target.bpm, ..Targets::default() };
            let query = PoolQuery {
                targets,
                genres: genres.as_deref(),
                subgenres: subgenres.as_deref(),
                limit: 200,
                exclude_ids: &[target.id],
                candidate_ids: None,
            };
            let pool: Vec<_> = candidate_pool(&conn, &query)?.into_iter().map(|row| (row, None)).collect();
            let edges = edge_map(approved_edges(&conn, Some(target.id))?);
            let pool = with_wordplay_targets(&conn, pool, &edges, &query)?;
            let stored = embedding(&conn, target.id)?;
            let (target_model, target_vector) = match &stored {
                Some((model, json)) => (model.clone(), parse_embedding(json.as_deref())),
                None => (None, None),
            };
            let mut scored = Vec::with_capacity(pool.len());
            for (row, _) in &pool {
                let similarity =
                    cosine_similarity(target_vector.as_deref(), row.vector.as_deref(), target_model.as_deref(), row.embedding_model.as_deref());
                let mut score = mixability_score(target.bpm, row.bpm, similarity, DEFAULT_WEIGHTS);
                let mut item = pool_track(row, now);
                if let Some(pair) = edges.get(&(target.id, row.id)) {
                    score += APPROVED_WORDPLAY_BONUS;
                    match wordplay_payload(pair) {
                        Ok(payload) => set(&mut item, "wordplay_json", text(payload)),
                        Err(response) => return Ok(Err(response)),
                    }
                }
                scored.push((item, score));
            }
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let items = head(scored, limit).into_iter().map(|(item, _)| dict(item)).collect();
            Ok(Ok(PyValue::List(items)))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

/// `GET /api/recommendations/next/page`.
pub async fn recommend_next_page(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let track_id = params.required_int("track_id");
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    let genres = params.list("genres");
    let subgenres = params.list("subgenres");
    let raw = RawTargets {
        bpm: params.opt_float("target_bpm"),
        energy: params.opt_float("target_energy"),
        danceability: params.opt_float("target_danceability"),
        brightness: params.opt_float("target_brightness"),
        noisiness: params.opt_float("target_noisiness"),
        year_min: params.opt_int("year_min").map(|year| year as f64),
        year_max: params.opt_int("year_max").map(|year| year as f64),
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let track_id = BigInt(track_id.expect("validated"));
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(target) = get_track(&conn, track_id)? else { return Ok(Err(track_not_found())) };
            let mut targets = sanitize(&raw);
            if targets.bpm.is_none() {
                targets.bpm = target.bpm;
            }
            let (Ok(limit), Ok(offset)) = (i64::try_from(limit), i64::try_from(offset)) else {
                return Ok(Err(server_error(&"page bounds")));
            };
            let page = ranked_page(
                &conn,
                &RankTarget { id: target.id, bpm: target.bpm },
                &targets,
                genres.as_deref(),
                subgenres.as_deref(),
                limit,
                offset,
            )?;
            let edges = edge_map(approved_edges(&conn, Some(target.id))?);
            let mut items = Vec::with_capacity(page.items.len());
            for row in page.items {
                let id = row.iter().find(|(key, _)| key == "id").and_then(|(_, value)| match value {
                    PyValue::Int(id) => i64::try_from(*id).ok(),
                    _ => None,
                });
                let mut pairs: Vec<(PyValue, PyValue)> = row.into_iter().map(|(key, value)| (PyValue::Str(key), value)).collect();
                if let Some(pair) = id.and_then(|id| edges.get(&(target.id, id))) {
                    match wordplay_payload(pair) {
                        Ok(payload) => {
                            let key = PyValue::Str("wordplay_json".into());
                            match pairs.iter_mut().find(|(name, _)| *name == key) {
                                Some(slot) => slot.1 = text(payload),
                                None => pairs.push((key, text(payload))),
                            }
                        }
                        Err(response) => return Ok(Err(response)),
                    }
                }
                items.push(PyValue::Dict(pairs));
            }
            let count = items.len() as i64;
            Ok(Ok(dict([
                ("items", PyValue::List(items)),
                ("total", int(page.total)),
                ("limit", int(limit)),
                ("offset", int(offset)),
                ("has_more", boolean(offset + count < page.total)),
            ])))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

struct AutoBody {
    limit: Option<i128>,
    min_length: Option<i128>,
    max_length: Option<i128>,
    seed_track_ids: Option<Vec<i128>>,
    genres: Option<Vec<String>>,
    subgenres: Option<Vec<String>>,
}

impl BodyModel for AutoBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let limit = fields.optional_int("limit");
        let min_length = fields.optional_int("min_length");
        let max_length = fields.optional_int("max_length");
        let seed_track_ids = fields.optional_int_list("seed_track_ids");
        let genres = fields.optional_str_list("genres");
        let subgenres = fields.optional_str_list("subgenres");
        fields.finish(|| Some(Self { limit, min_length, max_length, seed_track_ids, genres, subgenres }))
    }
}

/// `random.randint(low, high)`; `Err` is its ValueError message.
fn randint(low: i128, high: i128) -> Result<i128, String> {
    if low > high {
        return Err(format!("empty range in randrange({low}, {})", high + 1));
    }
    let span = (high - low + 1) as u128;
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    Ok(low + (u128::from_le_bytes(bytes) % span) as i128)
}

/// A seed or path end loaded from the library: its dump, node and lyrics flag.
struct Loaded {
    track: TrackRow,
    node: Node,
    has_lyrics: bool,
}

/// `bool(ly and ly.content.strip())`; `Err` when the row's content is NULL
/// (Python's `None.strip()`, a 500).
fn lyrics_flag(conn: &plumdeck_db::duckdb::Connection, track_id: i64) -> Result<Result<bool, ()>, DbError> {
    let row = plumdeck_db::repos::lyrics::get_lyrics(conn, BigInt(track_id.into()))?;
    Ok(match row {
        None => Ok(false),
        Some(row) => match row.content {
            None => Err(()),
            Some(content) => Ok(!py_strip(&content).is_empty()),
        },
    })
}

fn load(conn: &plumdeck_db::duckdb::Connection, track: TrackRow) -> Result<Result<Loaded, Response>, DbError> {
    let stored = embedding(conn, track.id)?;
    let (model, vector) = match &stored {
        Some((model, json)) => (model.clone(), parse_embedding(json.as_deref())),
        None => (None, None),
    };
    let has_lyrics = match lyrics_flag(conn, track.id)? {
        Ok(flag) => flag,
        Err(()) => return Ok(Err(server_error(&"lyrics content is NULL"))),
    };
    let node = Node {
        id: track.id,
        artist: track.artist.clone(),
        bpm: track.bpm,
        energy: track.energy,
        danceability: track.danceability,
        brightness: track.brightness,
        noisiness: track.noisiness,
        vector,
        embedding_model: model,
        wordplay_only_from: None,
    };
    Ok(Ok(Loaded { track, node, has_lyrics }))
}

/// `_annotate_wordplay`: each item after an approved edge's source carries the pair.
fn annotate(items: &mut [Vec<(&'static str, PyValue)>], ids: &[i64], edges: &BTreeMap<(i64, i64), PairRow>) -> Result<(), Response> {
    for index in 1..ids.len() {
        if let Some(pair) = edges.get(&(ids[index - 1], ids[index])) {
            let payload = wordplay_payload(pair)?;
            set(&mut items[index], "wordplay_json", text(payload));
        }
    }
    Ok(())
}

/// `POST /api/recommendations/auto`: Setlist Auto.
pub async fn auto_setlist(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_embedded::<AutoBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let length = match (request.limit, request.min_length, request.max_length) {
                (Some(limit), _, _) => limit,
                (None, Some(low), Some(high)) => match randint(low, high) {
                    Ok(length) => length,
                    Err(message) => return Ok(Err(not_found(&message))),
                },
                (None, Some(low), None) => low,
                (None, None, Some(high)) => high,
                (None, None, None) => setting_value(&conn, "setlist_default_length")?
                    .map_or(Some(10), |value| int_from_str(&value))
                    .unwrap_or(10),
            };
            let seed_ids: Vec<i128> = request.seed_track_ids.clone().unwrap_or_default();
            let mut seeds: Vec<Loaded> = Vec::new();
            let mut seen = BTreeSet::new();
            let known: Vec<i64> = seed_ids.iter().filter_map(|id| i64::try_from(*id).ok()).collect();
            let rows: BTreeMap<i64, TrackRow> = tracks_by_ids(&conn, &known)?.into_iter().map(|row| (row.id, row)).collect();
            for id in &seed_ids {
                if !seen.insert(*id) {
                    continue;
                }
                let Some(row) = i64::try_from(*id).ok().and_then(|id| rows.get(&id)) else { continue };
                match load(&conn, row.clone())? {
                    Ok(loaded) => seeds.push(loaded),
                    Err(response) => return Ok(Err(response)),
                }
            }
            let exclude: Vec<i64> = seed_ids.iter().filter_map(|id| i64::try_from(*id).ok()).collect();
            let query = PoolQuery {
                targets: Targets::default(),
                genres: request.genres.as_deref(),
                subgenres: request.subgenres.as_deref(),
                limit: 300,
                exclude_ids: &exclude,
                candidate_ids: None,
            };
            let pool: Vec<_> = candidate_pool(&conn, &query)?.into_iter().map(|row| (row, None)).collect();
            let edges = edge_map(approved_edges(&conn, None)?);
            let pool = with_wordplay_targets(&conn, pool, &edges, &query)?;
            let nodes: Vec<Node> = pool.iter().map(|(row, only)| node(row, only.clone())).collect();
            let seed_nodes: Vec<Node> = seeds.iter().map(|seed| seed.node.clone()).collect();
            let edge_keys: Edges = edges.keys().copied().collect();
            let Ok(length) = i64::try_from(length) else { return Ok(Err(server_error(&"length out of range"))) };
            let picks = build_chain(&nodes, &seed_nodes, length, &Targets::default(), &edge_keys);
            let mut items = Vec::new();
            let mut ids = Vec::new();
            for pick in picks {
                let (id, mut item) = match pick {
                    Pick::Pool(index) => (pool[index].0.id, pool_track(&pool[index].0, now)),
                    Pick::Seed(index) => (seeds[index].track.id, track_dump(&seeds[index].track)),
                    Pick::Start | Pick::End => unreachable!("chains have no ends"),
                };
                // has_lyrics comes from the pool entry first, then the seed.
                let flag = pool
                    .iter()
                    .find(|(row, _)| row.id == id)
                    .map(|(row, _)| row.has_lyrics)
                    .or_else(|| seeds.iter().find(|seed| seed.track.id == id).map(|seed| seed.has_lyrics))
                    .unwrap_or(false);
                set(&mut item, "has_lyrics", boolean(flag));
                ids.push(id);
                items.push(item);
            }
            if let Err(response) = annotate(&mut items, &ids, &edges) {
                return Ok(Err(response));
            }
            Ok(Ok(PyValue::List(items.into_iter().map(dict).collect())))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

struct PathBody {
    start: i128,
    end: i128,
    length: i128,
    genres: Option<Vec<String>>,
    subgenres: Option<Vec<String>>,
}

impl BodyModel for PathBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let start = fields.required_int("start_track_id");
        let end = fields.required_int("end_track_id");
        let length = fields.int_or("length", 10);
        let genres = fields.optional_str_list("genres");
        let subgenres = fields.optional_str_list("subgenres");
        fields.finish(|| Some(Self { start: start?, end: end?, length, genres, subgenres }))
    }
}

/// `POST /api/recommendations/path`: a bridge between two tracks.
pub async fn path_setlist(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_embedded::<PathBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let start_row = get_track(&conn, BigInt(request.start))?;
            let end_row = get_track(&conn, BigInt(request.end))?;
            let (Some(start_row), Some(end_row)) = (start_row, end_row) else {
                return Ok(Err(not_found("Start or End track not found")));
            };
            let start = match load(&conn, start_row)? {
                Ok(loaded) => loaded,
                Err(response) => return Ok(Err(response)),
            };
            let end = match load(&conn, end_row)? {
                Ok(loaded) => loaded,
                Err(response) => return Ok(Err(response)),
            };
            let middle = (start.track.bpm.filter(|bpm| *bpm != 0.0).unwrap_or(0.0) + end.track.bpm.filter(|bpm| *bpm != 0.0).unwrap_or(0.0)) / 2.0;
            let exclude = [start.track.id, end.track.id];
            let query = PoolQuery {
                targets: Targets { bpm: Some(middle), ..Targets::default() },
                genres: request.genres.as_deref(),
                subgenres: request.subgenres.as_deref(),
                limit: 400,
                exclude_ids: &exclude,
                candidate_ids: None,
            };
            let pool: Vec<_> = candidate_pool(&conn, &query)?.into_iter().map(|row| (row, None)).collect();
            let edges = edge_map(approved_edges(&conn, None)?);
            let pool = with_wordplay_targets(&conn, pool, &edges, &query)?;
            let nodes: Vec<Node> = pool.iter().map(|(row, only)| node(row, only.clone())).collect();
            let edge_keys: Edges = edges.keys().copied().collect();
            let Ok(length) = i64::try_from(request.length) else { return Ok(Err(server_error(&"length out of range"))) };
            let Ok(picks) = build_path(&nodes, &start.node, &end.node, length, &edge_keys) else {
                return Ok(Err(server_error(&"arithmetic on a missing BPM or energy")));
            };
            let mut items = Vec::new();
            let mut ids = Vec::new();
            for pick in picks {
                let (id, mut item, fallback) = match pick {
                    Pick::Pool(index) => (pool[index].0.id, pool_track(&pool[index].0, now), false),
                    Pick::Start => (start.track.id, track_dump(&start.track), start.has_lyrics),
                    Pick::End => (end.track.id, track_dump(&end.track), end.has_lyrics),
                    Pick::Seed(_) => unreachable!("paths have no seeds"),
                };
                let flag = pool.iter().find(|(row, _)| row.id == id).map(|(row, _)| row.has_lyrics).unwrap_or(fallback);
                set(&mut item, "has_lyrics", boolean(flag));
                ids.push(id);
                items.push(item);
            }
            if let Err(response) = annotate(&mut items, &ids, &edges) {
                return Ok(Err(response));
            }
            Ok(Ok(PyValue::List(items.into_iter().map(dict).collect())))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

struct WordplayBody {
    wordplay_json: Option<String>,
}

impl BodyModel for WordplayBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let wordplay_json = fields.optional_str("wordplay_json");
        fields.finish(|| Some(Self { wordplay_json }))
    }
}

/// `PATCH /api/setlist-tracks/{id}/wordplay`.
pub async fn patch_entry_wordplay(State(state): State<AppState>, Path(entry_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_id(&mut params, "setlist_track_id", &entry_id);
    let update = match crate::body::validate_embedded::<WordplayBody>(body.as_ref()) {
        Ok(update) => Some(update),
        Err(errors) => {
            params.extend(errors);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, update) = (id.expect("validated"), update.expect("validated"));
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| set_entry_wordplay(conn, id, update.wordplay_json.as_deref()))).await {
        Ok(Some(row)) => PyJson(entry_json(&row)).into_response(),
        Ok(None) => not_found("Setlist track not found"),
        Err(response) => response,
    }
}
