//! Play mode (backend/api/routers/play.py; S7): local playlists over the
//! setlist tables, the imported rekordbox mirror, play sessions and history,
//! and the recorder's files (naming, WAV/FLAC conversion in-process, discard).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::types::{TimeUnit, Value};
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::rows::{json_rows, scalar_int};
use plumdeck_db::repos::setlists::{create_setlist, delete_setlist, get_setlist, update_setlist};
use plumdeck_db::repos::tracks::BigInt;
use plumdeck_db::DbError;

use crate::body::{parse_body, BodyModel, Fields, ValidationError};
use crate::query::Params;
use crate::response::{boolean, dict, int, text, PyJson};
use crate::routes::setlists::setlist_json;
use crate::{server_error, AppState, HttpError};

fn http(status: u16, detail: impl Into<String>) -> Response {
    HttpError(ApiError::new(status, detail.into())).into_response()
}

fn ts(at: NaiveDateTime) -> Value {
    Value::Timestamp(TimeUnit::Microsecond, at.and_utc().timestamp_micros())
}

fn opt_text(value: &Option<String>) -> Value {
    value.clone().map_or(Value::Null, Value::Text)
}

fn page(items: Vec<PyValue>, total: i64, limit: i128, offset: i128) -> PyValue {
    let count = items.len() as i128;
    dict([
        ("items", PyValue::List(items)),
        ("total", int(total)),
        ("limit", PyValue::Int(limit)),
        ("offset", PyValue::Int(offset)),
        ("has_more", boolean(offset + count < total as i128)),
    ])
}

fn with(mut value: PyValue, extra: Vec<(&str, PyValue)>) -> PyValue {
    if let PyValue::Dict(pairs) = &mut value {
        for (key, item) in extra {
            match pairs.iter_mut().find(|(k, _)| matches!(k, PyValue::Str(s) if s == key)) {
                Some(slot) => slot.1 = item,
                None => pairs.push((PyValue::Str(key.into()), item)),
            }
        }
    }
    value
}

fn paging(uri: &Uri) -> Result<(i128, i128), Response> {
    let mut params = Params::from_uri(uri);
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    params.finish()?;
    Ok((limit, offset))
}

fn path_int(name: &str, value: &str) -> Result<i128, Response> {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let parsed = params.path_int(name, value);
    params.finish()?;
    Ok(parsed.unwrap_or_default())
}

/// `min_length` / `max_length` on a string field.
fn bounded_str(fields: &Fields<'_>, name: &str, value: Option<String>, min: usize, max: usize) -> Option<String> {
    let value = value?;
    let length = value.chars().count();
    if length < min {
        fields.reject(name, "string_too_short", &format!("String should have at least {min} character{}", if min == 1 { "" } else { "s" }));
        return None;
    }
    if length > max {
        fields.reject(name, "string_too_long", &format!("String should have at most {max} characters"));
        return None;
    }
    Some(value)
}

/// An `Optional[datetime]` from JSON (ISO text or epoch seconds). An aware
/// value is stored as local wall time, like DuckDB's TIMESTAMPTZ to
/// TIMESTAMP cast of the datetime Python binds.
fn opt_datetime(fields: &Fields<'_>, name: &str) -> Option<Option<NaiveDateTime>> {
    match fields.value(name) {
        None | Some(PyValue::None) => Some(None),
        Some(PyValue::Str(text)) => match parse_datetime(text) {
            Some(at) => Some(Some(at)),
            None => {
                fields.reject(name, "datetime_from_date_parsing", "Input should be a valid datetime");
                None
            }
        },
        // pydantic makes an aware UTC datetime; DuckDB stores it as local wall time.
        Some(PyValue::Int(n)) => i64::try_from(*n)
            .ok()
            .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
            .map(|at| Some(at.with_timezone(&chrono::Local).naive_local())),
        Some(_) => {
            fields.reject(name, "datetime_type", "Input should be a valid datetime");
            None
        }
    }
}

fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(at.with_timezone(&chrono::Local).naive_local());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(at) = NaiveDateTime::parse_from_str(text, format) {
            return Some(at);
        }
    }
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0))
}

fn non_negative(fields: &Fields<'_>, name: &str, default: i128) -> i128 {
    let value = fields.int_or(name, default);
    if value < 0 {
        fields.reject(name, "greater_than_equal", "Input should be greater than or equal to 0");
    }
    value
}

// ------------------------------------------------------------------ playlists

struct NameBody {
    name: String,
}

impl BodyModel for NameBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let name = bounded_str(fields, "name", fields.required_str("name"), 1, 300);
        fields.finish(|| Some(Self { name: name? }))
    }
}

/// `GET /api/play/playlists`.
pub async fn local_playlists(State(state): State<AppState>, uri: Uri) -> Response {
    let (limit, offset) = match paging(&uri) {
        Ok(paging) => paging,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let total = scalar_int(&conn, "SELECT count(*) FROM setlists", vec![])?.unwrap_or(0);
            let items = json_rows(
                &conn,
                "SELECT s.*, 'plumdeck' AS source, true AS editable, \
                 (SELECT count(*) FROM setlist_tracks st JOIN tracks t ON t.id=st.track_id WHERE st.setlist_id=s.id) AS track_count \
                 FROM setlists s ORDER BY s.updated_at DESC, s.id DESC LIMIT ? OFFSET ?",
                vec![Value::HugeInt(limit), Value::HugeInt(offset)],
            )?;
            Ok(page(items, total, limit, offset))
        })
        .await;
    match result {
        Ok(value) => PyJson(value).into_response(),
        Err(response) => response,
    }
}

/// `POST /api/play/playlists`.
pub async fn create_local_playlist(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload = match parse_body::<NameBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let name = plumdeck_core::compat::text::py_strip(&payload.name).to_owned();
    if name.is_empty() {
        return http(422, "Playlist name cannot be blank");
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| create_setlist(conn, &name, now))).await {
        Ok(row) => PyJson(with(setlist_json(&row), vec![("source", text("plumdeck")), ("editable", boolean(true)), ("track_count", int(0))])).into_response(),
        Err(response) => response,
    }
}

/// `PATCH /api/play/playlists/{id}`.
pub async fn rename_local_playlist(State(state): State<AppState>, UrlPath(id): UrlPath<String>, headers: HeaderMap, body: Bytes) -> Response {
    let id = match path_int("playlist_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match parse_body::<NameBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let name = plumdeck_core::compat::text::py_strip(&payload.name).to_owned();
    if name.is_empty() {
        return http(422, "Playlist name cannot be blank");
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let changes = vec![("name".to_owned(), PyValue::Str(name))];
    match state.blocking(move || db.write(|conn| update_setlist(conn, BigInt(id), &changes, now))).await {
        Ok(Some(row)) => PyJson(with(setlist_json(&row), vec![("source", text("plumdeck")), ("editable", boolean(true))])).into_response(),
        Ok(None) => http(404, "Local playlist not found"),
        Err(response) => response,
    }
}

/// `DELETE /api/play/playlists/{id}`.
pub async fn delete_local_playlist(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let id = match path_int("playlist_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| delete_setlist(conn, BigInt(id)))).await {
        Ok(true) => PyJson(dict([("ok", boolean(true))])).into_response(),
        Ok(false) => http(404, "Local playlist not found"),
        Err(response) => response,
    }
}

/// `GET /api/play/playlists/{id}/tracks`.
pub async fn local_playlist_tracks(State(state): State<AppState>, UrlPath(id): UrlPath<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let id = params.path_int("playlist_id", &id);
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.unwrap_or_default();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            if get_setlist(&conn, BigInt(id))?.is_none() {
                return Ok(None);
            }
            let total = scalar_int(&conn, "SELECT count(*) FROM setlist_tracks st JOIN tracks t ON t.id=st.track_id WHERE st.setlist_id=?", vec![Value::HugeInt(id)])?
                .unwrap_or(0);
            let items = json_rows(
                &conn,
                "SELECT t.*, st.id AS setlist_track_id, st.position, st.transition_note, st.wordplay_json,st.in_ms,st.out_ms,\
                 st.playback_rate, st.extra_duration_ms,st.overlap_next_ms,st.revision, \
                 (l.content IS NOT NULL AND length(trim(l.content)) > 0) AS has_lyrics \
                 FROM setlist_tracks st JOIN tracks t ON t.id=st.track_id LEFT JOIN lyrics l ON l.track_id=t.id \
                 WHERE st.setlist_id=? ORDER BY st.position, st.id LIMIT ? OFFSET ?",
                vec![Value::HugeInt(id), Value::HugeInt(limit), Value::HugeInt(offset)],
            )?;
            Ok(Some(page(items, total, limit, offset)))
        })
        .await;
    match result {
        Ok(Some(value)) => PyJson(value).into_response(),
        Ok(None) => http(404, "Local playlist not found"),
        Err(response) => response,
    }
}

struct TrackAdd {
    track_id: i128,
    position: Option<i128>,
}

impl BodyModel for TrackAdd {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let track_id = fields.required_int("track_id");
        let position = fields.optional_int("position");
        if position.is_some_and(|p| p < 0) {
            fields.reject("position", "greater_than_equal", "Input should be greater than or equal to 0");
        }
        fields.finish(|| Some(Self { track_id: track_id?, position }))
    }
}

enum Added {
    NoPlaylist,
    NoTrack,
    Entry(i64),
}

/// `POST /api/play/playlists/{id}/tracks`.
pub async fn add_local_playlist_track(State(state): State<AppState>, UrlPath(id): UrlPath<String>, headers: HeaderMap, body: Bytes) -> Response {
    let id = match path_int("playlist_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match parse_body::<TrackAdd>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                if get_setlist(conn, BigInt(id))?.is_none() {
                    return Ok(Added::NoPlaylist);
                }
                if plumdeck_db::repos::tracks::get_track(conn, BigInt(payload.track_id))?.is_none() {
                    return Ok(Added::NoTrack);
                }
                let count = scalar_int(conn, "SELECT count(*) FROM setlist_tracks WHERE setlist_id=?", vec![Value::HugeInt(id)])?.unwrap_or(0) as i128;
                let target = payload.position.map_or(count, |p| p.min(count));
                if target > 0 {
                    conn.execute(
                        "UPDATE setlist_tracks SET overlap_next_ms=0,revision=revision+1 WHERE setlist_id=? AND position=?",
                        plumdeck_db::duckdb::params_from_iter([Value::HugeInt(id), Value::HugeInt(target - 1)]),
                    )?;
                }
                conn.execute(
                    "UPDATE setlist_tracks SET position=position+1 WHERE setlist_id=? AND position>=?",
                    plumdeck_db::duckdb::params_from_iter([Value::HugeInt(id), Value::HugeInt(target)]),
                )?;
                let entry: i64 = conn.query_row(
                    // created_at from the clock, not the column default (D-21).
                    "INSERT INTO setlist_tracks (setlist_id,track_id,position,created_at) VALUES (?,?,?,?) RETURNING id",
                    plumdeck_db::duckdb::params_from_iter([Value::HugeInt(id), Value::HugeInt(payload.track_id), Value::HugeInt(target), ts(now)]),
                    |row| row.get(0),
                )?;
                conn.execute("UPDATE setlists SET updated_at=? WHERE id=?", plumdeck_db::duckdb::params_from_iter([ts(now), Value::HugeInt(id)]))?;
                Ok(Added::Entry(entry))
            })
        })
        .await;
    match result {
        Ok(Added::Entry(entry)) => PyJson(dict([("setlist_track_id", int(entry))])).into_response(),
        Ok(Added::NoTrack) => http(404, "Track not found"),
        Ok(Added::NoPlaylist) => http(404, "Local playlist not found"),
        Err(response) => response,
    }
}

/// `DELETE /api/play/playlists/{id}/tracks/{entry_id}`.
pub async fn remove_local_playlist_track(State(state): State<AppState>, UrlPath((id, entry)): UrlPath<(String, String)>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = params.path_int("playlist_id", &id);
    let entry = params.path_int("entry_id", &entry);
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, entry) = (id.unwrap_or_default(), entry.unwrap_or_default());
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                if get_setlist(conn, BigInt(id))?.is_none() {
                    return Ok(false);
                }
                let p = |values: Vec<Value>| plumdeck_db::duckdb::params_from_iter(values);
                let current = scalar_int(conn, "SELECT position FROM setlist_tracks WHERE setlist_id=? AND id=?", vec![Value::HugeInt(id), Value::HugeInt(entry)])?;
                if let Some(position) = current.filter(|p| *p > 0) {
                    conn.execute(
                        "UPDATE setlist_tracks SET overlap_next_ms=0,revision=revision+1 WHERE setlist_id=? AND position=?",
                        p(vec![Value::HugeInt(id), Value::BigInt(position - 1)]),
                    )?;
                }
                let Some(position) = scalar_int(conn, "DELETE FROM setlist_tracks WHERE setlist_id=? AND id=? RETURNING position", vec![Value::HugeInt(id), Value::HugeInt(entry)])?
                else {
                    return Ok(false);
                };
                conn.execute("UPDATE setlist_tracks SET position=position-1 WHERE setlist_id=? AND position>?", p(vec![Value::HugeInt(id), Value::BigInt(position)]))?;
                conn.execute("UPDATE setlists SET updated_at=? WHERE id=?", p(vec![ts(now), Value::HugeInt(id)]))?;
                Ok(true)
            })
        })
        .await;
    match result {
        Ok(true) => PyJson(dict([("ok", boolean(true))])).into_response(),
        Ok(false) => http(404, "Playlist entry not found"),
        Err(response) => response,
    }
}

// ------------------------------------------------------------------ rekordbox mirror

struct MirrorPlaylist {
    external_id: String,
    parent: Option<String>,
    name: String,
    order: i128,
    kind: String,
}

struct MirrorMember {
    playlist: String,
    position: i128,
    external_track_id: Option<String>,
    local_track_id: Option<i128>,
    filepath: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    bpm: Option<f64>,
    key: Option<String>,
    duration: Option<f64>,
}

struct MirrorImport {
    source_id: String,
    source_name: String,
    playlists: Vec<MirrorPlaylist>,
    members: Vec<MirrorMember>,
}

fn opt_float(fields: &Fields<'_>, name: &str) -> Option<f64> {
    match fields.value(name) {
        None | Some(PyValue::None) => None,
        Some(value) => match crate::lax::to_float(value) {
            Some(x) => Some(x),
            None => {
                fields.reject(name, "float_type", "Input should be a valid number");
                None
            }
        },
    }
}

impl BodyModel for MirrorImport {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let source_id = bounded_str(fields, "source_id", fields.required_str("source_id"), 1, 200);
        let source_name = bounded_str(fields, "source_name", fields.required_str("source_name"), 1, 300);
        let playlists = fields.model_list("playlists", |item| {
            let external_id = item.required_str("external_id");
            let parent = item.optional_str("parent_external_id");
            let name = item.required_str("name");
            let order = item.int_or("order", 0);
            let kind = item.choice_or("kind", "playlist", &["folder", "playlist", "smart"]);
            Some(MirrorPlaylist { external_id: external_id?, parent, name: name?, order, kind })
        });
        let members = match fields.value("members") {
            None => Some(Vec::new()),
            Some(_) => fields.model_list("members", |item| {
                Some(MirrorMember {
                    playlist: item.required_str("playlist_external_id")?,
                    position: item.int_or("position", 0),
                    external_track_id: item.optional_str("external_track_id"),
                    local_track_id: item.optional_int("local_track_id"),
                    filepath: item.optional_str("filepath"),
                    title: item.optional_str("title"),
                    artist: item.optional_str("artist"),
                    bpm: opt_float(item, "bpm"),
                    key: item.optional_str("key"),
                    duration: opt_float(item, "duration"),
                })
            }),
        };
        fields.finish(|| Some(Self { source_id: source_id?, source_name: source_name?, playlists: playlists?, members: members? }))
    }
}

/// `_normalized_filepath`: NFC of the normcased real path.
fn normalized_filepath(path: &str) -> String {
    let expanded = plumdeck_core::paths::resolve_lenient(Path::new(&expand_user(path)));
    let text = expanded.to_string_lossy().into_owned();
    let text = if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text };
    plumdeck_core::compat::text::nfc(&text)
}

fn expand_user(path: &str) -> String {
    match (path.strip_prefix('~'), std::env::var("HOME")) {
        (Some(rest), Ok(home)) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_owned(),
    }
}

/// `POST /api/play/rekordbox/import`.
pub async fn import_rekordbox_mirror(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload = match parse_body::<MirrorImport>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let ids: BTreeSet<&str> = payload.playlists.iter().map(|p| p.external_id.as_str()).collect();
    if ids.len() != payload.playlists.len() {
        return http(422, "Playlist external IDs must be unique");
    }
    for item in &payload.playlists {
        if let Some(parent) = item.parent.as_deref().filter(|p| !p.is_empty()) {
            if !ids.contains(parent) {
                return http(400, format!("Unknown parent playlist: {parent}"));
            }
        }
    }
    if payload.members.iter().any(|m| !ids.contains(m.playlist.as_str())) {
        return http(400, "A member references an unknown playlist");
    }
    let parents: BTreeMap<&str, Option<&str>> =
        payload.playlists.iter().map(|p| (p.external_id.as_str(), p.parent.as_deref().filter(|p| !p.is_empty()))).collect();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    for id in &ids {
        let mut path: BTreeSet<&str> = BTreeSet::new();
        let mut current = Some(*id);
        while let Some(node) = current.filter(|n| !visited.contains(n)) {
            if path.contains(node) {
                return http(422, format!("Playlist parent cycle detected at: {node}"));
            }
            path.insert(node);
            current = parents.get(node).copied().flatten();
        }
        visited.extend(path);
    }
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                let p = |values: Vec<Value>| plumdeck_db::duckdb::params_from_iter(values);
                let mut statement = conn.prepare("SELECT id, filepath FROM tracks ORDER BY id")?;
                let library: Vec<(i64, Option<String>)> = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
                let local_ids: BTreeSet<i64> = library.iter().map(|(id, _)| *id).collect();
                let mut paths: BTreeMap<String, i64> = BTreeMap::new();
                for (id, path) in &library {
                    if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                        paths.entry(normalized_filepath(path)).or_insert(*id);
                    }
                }
                let source = Value::Text(payload.source_id.clone());
                conn.execute("DELETE FROM rekordbox_playlist_tracks WHERE source_id=?", p(vec![source.clone()]))?;
                conn.execute("DELETE FROM rekordbox_playlists WHERE source_id=?", p(vec![source.clone()]))?;
                conn.execute(
                    "INSERT INTO rekordbox_sources (id, name, imported_at) VALUES (?, ?, ?) \
                     ON CONFLICT (id) DO UPDATE SET name=excluded.name, imported_at=excluded.imported_at",
                    p(vec![source.clone(), Value::Text(payload.source_name.clone()), ts(now)]),
                )?;
                for item in &payload.playlists {
                    conn.execute(
                        "INSERT INTO rekordbox_playlists (source_id, external_id, parent_external_id, name, sort_order, kind) VALUES (?, ?, ?, ?, ?, ?)",
                        p(vec![
                            source.clone(),
                            Value::Text(item.external_id.clone()),
                            opt_text(&item.parent.clone().filter(|p| !p.is_empty())),
                            Value::Text(item.name.clone()),
                            Value::HugeInt(item.order),
                            Value::Text(item.kind.clone()),
                        ]),
                    )?;
                }
                let mut normalized: BTreeMap<String, String> = BTreeMap::new();
                let mut resolved = 0;
                for member in &payload.members {
                    let mut local = member.local_track_id.filter(|id| i64::try_from(*id).is_ok_and(|id| local_ids.contains(&id))).map(|id| id as i64);
                    if local.is_none() {
                        if let Some(path) = member.filepath.as_deref().filter(|p| !p.is_empty()) {
                            let key = normalized.entry(path.to_owned()).or_insert_with(|| normalized_filepath(path)).clone();
                            local = paths.get(&key).copied();
                        }
                    }
                    resolved += usize::from(local.is_some());
                    let f = |x: Option<f64>| x.map_or(Value::Null, |x| Value::Double(x));
                    conn.execute(
                        "INSERT INTO rekordbox_playlist_tracks (source_id, playlist_external_id, position, external_track_id, local_track_id, \
                         filepath, title, artist, bpm, musical_key, duration) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        p(vec![
                            source.clone(),
                            Value::Text(member.playlist.clone()),
                            Value::HugeInt(member.position),
                            opt_text(&member.external_track_id),
                            local.map_or(Value::Null, Value::BigInt),
                            opt_text(&member.filepath),
                            opt_text(&member.title),
                            opt_text(&member.artist),
                            f(member.bpm),
                            opt_text(&member.key),
                            f(member.duration),
                        ]),
                    )?;
                }
                Ok(dict([
                    ("source_id", text(payload.source_id.clone())),
                    ("playlists", int(payload.playlists.len() as i64)),
                    ("members", int(payload.members.len() as i64)),
                    ("resolved", int(resolved as i64)),
                    ("unmapped", int((payload.members.len() - resolved) as i64)),
                ]))
            })
        })
        .await;
    match result {
        Ok(value) => PyJson(value).into_response(),
        Err(response) => response,
    }
}

async fn rows(state: &AppState, sql: &'static str, params: Vec<Value>) -> Response {
    let db = state.db.clone();
    match state.blocking(move || json_rows(&*db.lease()?, sql, params)).await {
        Ok(items) => PyJson(PyValue::List(items)).into_response(),
        Err(response) => response,
    }
}

/// `GET /api/play/rekordbox/sources`.
pub async fn mirror_sources(State(state): State<AppState>) -> Response {
    rows(
        &state,
        "SELECT s.id, s.name, s.imported_at, count(DISTINCT p.external_id) AS playlist_count, count(m.position) AS member_count, \
         count(CASE WHEN m.position IS NOT NULL AND m.local_track_id IS NULL THEN 1 END) AS unmapped_count \
         FROM rekordbox_sources s LEFT JOIN rekordbox_playlists p ON p.source_id=s.id \
         LEFT JOIN rekordbox_playlist_tracks m ON m.source_id=p.source_id AND m.playlist_external_id=p.external_id \
         GROUP BY s.id, s.name, s.imported_at ORDER BY s.name",
        vec![],
    )
    .await
}

fn source_row(conn: &Connection, id: &str) -> Result<Option<PyValue>, DbError> {
    Ok(json_rows(conn, "SELECT id, name, imported_at FROM rekordbox_sources WHERE id=?", vec![Value::Text(id.into())])?.into_iter().next())
}

/// `GET /api/play/rekordbox/{source_id}/tree`.
pub async fn mirror_tree(State(state): State<AppState>, UrlPath(source_id): UrlPath<String>) -> Response {
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(source) = source_row(&conn, &source_id)? else { return Ok(None) };
            let items = json_rows(
                &conn,
                "SELECT p.*, count(m.position) AS track_count, \
                 count(CASE WHEN m.position IS NOT NULL AND m.local_track_id IS NULL THEN 1 END) AS unmapped_count \
                 FROM rekordbox_playlists p LEFT JOIN rekordbox_playlist_tracks m ON m.source_id=p.source_id AND m.playlist_external_id=p.external_id \
                 WHERE p.source_id=? GROUP BY p.source_id,p.external_id,p.parent_external_id,p.name,p.sort_order,p.kind ORDER BY p.sort_order,p.name",
                vec![Value::Text(source_id)],
            )?;
            Ok(Some(dict([("source", source), ("items", PyValue::List(items))])))
        })
        .await;
    match result {
        Ok(Some(value)) => PyJson(value).into_response(),
        Ok(None) => http(404, "Rekordbox mirror source not found"),
        Err(response) => response,
    }
}

/// `GET /api/play/rekordbox/{source_id}/tree/page`.
pub async fn mirror_tree_page(State(state): State<AppState>, UrlPath(source_id): UrlPath<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let parent = params.opt_str("parent_external_id");
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let Some(source) = source_row(&conn, &source_id)? else { return Ok(None) };
            let parent = opt_text(&parent);
            let total = scalar_int(
                &conn,
                "SELECT count(*) FROM rekordbox_playlists WHERE source_id=? AND parent_external_id IS NOT DISTINCT FROM ?",
                vec![Value::Text(source_id.clone()), parent.clone()],
            )?
            .unwrap_or(0);
            let items = json_rows(
                &conn,
                "SELECT p.*, false AS editable, 'rekordbox' AS source, \
                 (SELECT count(*) FROM rekordbox_playlist_tracks m WHERE m.source_id=p.source_id AND m.playlist_external_id=p.external_id) AS track_count, \
                 (SELECT count(*) FROM rekordbox_playlist_tracks m LEFT JOIN tracks t ON t.id=m.local_track_id \
                  WHERE m.source_id=p.source_id AND m.playlist_external_id=p.external_id AND t.id IS NULL) AS unmapped_count, \
                 (SELECT count(*) FROM rekordbox_playlists c WHERE c.source_id=p.source_id AND c.parent_external_id=p.external_id) AS child_count \
                 FROM rekordbox_playlists p WHERE p.source_id=? AND p.parent_external_id IS NOT DISTINCT FROM ? \
                 ORDER BY p.sort_order,p.name,p.external_id LIMIT ? OFFSET ?",
                vec![Value::Text(source_id), parent, Value::HugeInt(limit), Value::HugeInt(offset)],
            )?;
            let mut value = page(items, total, limit, offset);
            if let PyValue::Dict(pairs) = &mut value {
                pairs.insert(0, (PyValue::Str("source".into()), source));
            }
            Ok(Some(value))
        })
        .await;
    match result {
        Ok(Some(value)) => PyJson(value).into_response(),
        Ok(None) => http(404, "Rekordbox mirror source not found"),
        Err(response) => response,
    }
}

const MIRROR_TRACK_COLUMNS: &str = "SELECT m.position,m.external_track_id,m.local_track_id,m.filepath AS source_filepath, \
    coalesce(t.title,m.title,'') AS title,coalesce(t.artist,m.artist,'') AS artist, coalesce(t.bpm,m.bpm) AS bpm,coalesce(t.key,m.musical_key,'') AS key, \
    coalesce(t.duration,m.duration,0) AS duration,t.filepath,t.album,t.genre,t.subgenre, t.scale,t.energy,t.danceability,t.loudness,t.brightness,\
    t.noisiness,t.contrast, t.loudness_range,t.spectral_flux,t.spectral_rolloff,t.is_genre_verified,t.created_at, (t.id IS NOT NULL) AS resolved \
    FROM rekordbox_playlist_tracks m LEFT JOIN tracks t ON t.id=m.local_track_id WHERE m.source_id=? AND m.playlist_external_id=? ORDER BY m.position";

/// `GET /api/play/rekordbox/{source_id}/playlists/{playlist_id}/tracks`.
pub async fn mirror_tracks(State(state): State<AppState>, UrlPath((source, playlist)): UrlPath<(String, String)>) -> Response {
    rows(&state, MIRROR_TRACK_COLUMNS, vec![Value::Text(source), Value::Text(playlist)]).await
}

/// `GET /api/play/rekordbox/{source_id}/playlists/{playlist_id}/tracks/page`.
pub async fn mirror_tracks_page(State(state): State<AppState>, UrlPath((source, playlist)): UrlPath<(String, String)>, uri: Uri) -> Response {
    let (limit, offset) = match paging(&uri) {
        Ok(paging) => paging,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let key = vec![Value::Text(source.clone()), Value::Text(playlist.clone())];
            if scalar_int(&conn, "SELECT 1 FROM rekordbox_playlists WHERE source_id=? AND external_id=?", key.clone())?.is_none() {
                return Ok(None);
            }
            let total = scalar_int(&conn, "SELECT count(*) FROM rekordbox_playlist_tracks WHERE source_id=? AND playlist_external_id=?", key.clone())?.unwrap_or(0);
            let mut params = key;
            params.extend([Value::HugeInt(limit), Value::HugeInt(offset)]);
            let items = json_rows(&conn, &format!("{MIRROR_TRACK_COLUMNS} LIMIT ? OFFSET ?"), params)?;
            Ok(Some(page(items, total, limit, offset)))
        })
        .await;
    match result {
        Ok(Some(value)) => PyJson(value).into_response(),
        Ok(None) => http(404, "Rekordbox mirror playlist not found"),
        Err(response) => response,
    }
}

struct CopyBody {
    name: Option<String>,
}

impl BodyModel for CopyBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let name = match fields.value("name") {
            None | Some(PyValue::None) => None,
            Some(_) => {
                let value = bounded_str(fields, "name", fields.optional_str("name"), 1, 300);
                if value.is_none() {
                    return fields.finish(|| None);
                }
                value
            }
        };
        fields.finish(|| Some(Self { name }))
    }
}

/// `POST /api/play/rekordbox/{source_id}/playlists/{playlist_id}/copy`.
pub async fn copy_mirror_playlist(
    State(state): State<AppState>,
    UrlPath((source, playlist)): UrlPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = match parse_body::<CopyBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                let key = vec![Value::Text(source.clone()), Value::Text(playlist.clone())];
                let found: Option<String> = conn
                    .query_row(
                        "SELECT name FROM rekordbox_playlists WHERE source_id=? AND external_id=?",
                        plumdeck_db::duckdb::params_from_iter(key.clone()),
                        |r| r.get(0),
                    )
                    .ok();
                let Some(original) = found else { return Ok(None) };
                let name = payload.name.as_deref().map(|n| plumdeck_core::compat::text::py_strip(n).to_owned()).unwrap_or(original);
                let created = create_setlist(conn, &name, now)?;
                let total = scalar_int(conn, "SELECT count(*) FROM rekordbox_playlist_tracks WHERE source_id=? AND playlist_external_id=?", key.clone())?.unwrap_or(0);
                let copied = scalar_int(
                    conn,
                    "SELECT count(*) FROM rekordbox_playlist_tracks m JOIN tracks t ON t.id=m.local_track_id WHERE m.source_id=? AND m.playlist_external_id=?",
                    key.clone(),
                )?
                .unwrap_or(0);
                let mut params = vec![Value::BigInt(created.id), ts(now)];
                params.extend(key);
                conn.execute(
                    // created_at from the clock, not the column default (D-21).
                    "INSERT INTO setlist_tracks (setlist_id,track_id,position,created_at) SELECT ?,m.local_track_id,row_number() OVER (ORDER BY m.position)-1,? \
                     FROM rekordbox_playlist_tracks m JOIN tracks t ON t.id=m.local_track_id WHERE m.source_id=? AND m.playlist_external_id=? ORDER BY m.position",
                    plumdeck_db::duckdb::params_from_iter(params),
                )?;
                let local = get_setlist(conn, BigInt(created.id.into()))?.unwrap_or(created);
                let value = with(setlist_json(&local), vec![("source", text("plumdeck")), ("editable", boolean(true)), ("track_count", int(copied))]);
                Ok(Some(dict([("playlist", value), ("copied", int(copied)), ("skipped_unresolved", int(total - copied))])))
            })
        })
        .await;
    match result {
        Ok(Some(value)) => PyJson(value).into_response(),
        Ok(None) => http(404, "Rekordbox mirror playlist not found"),
        Err(response) => response,
    }
}

// ------------------------------------------------------------------ sessions and history

struct SessionBody {
    id: String,
    deck_count: i128,
}

impl BodyModel for SessionBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let id = bounded_str(fields, "id", fields.required_str("id"), 1, 200);
        let deck_count = fields.int_or("deck_count", 2);
        if deck_count != 2 && deck_count != 4 {
            fields.reject("deck_count", "literal_error", "Input should be 2 or 4");
        }
        fields.finish(|| Some(Self { id: id?, deck_count }))
    }
}

/// `POST /api/play/sessions`.
pub async fn start_session(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload = match parse_body::<SessionBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let (id, count) = (payload.id.clone(), payload.deck_count);
    let result = state
        .blocking(move || {
            db.write(|conn| {
                conn.execute(
                    "INSERT INTO play_sessions (id,deck_count,started_at) VALUES (?,?,?) ON CONFLICT (id) DO UPDATE SET deck_count=excluded.deck_count,ended_at=NULL",
                    plumdeck_db::duckdb::params_from_iter([Value::Text(id), Value::HugeInt(count), ts(now)]),
                )?;
                Ok(())
            })
        })
        .await;
    match result {
        Ok(()) => PyJson(dict([("id", text(payload.id)), ("deck_count", PyValue::Int(payload.deck_count))])).into_response(),
        Err(response) => response,
    }
}

/// `POST /api/play/sessions/{id}/end`.
pub async fn end_session(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let key = id.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                conn.execute("UPDATE play_sessions SET ended_at=? WHERE id=?", plumdeck_db::duckdb::params_from_iter([ts(now), Value::Text(key)]))?;
                Ok(())
            })
        })
        .await;
    match result {
        Ok(()) => PyJson(dict([("id", text(id)), ("ended", boolean(true))])).into_response(),
        Err(response) => response,
    }
}

struct HistoryBody {
    event_key: String,
    session_id: String,
    deck: String,
    track_id: i128,
    loaded_at: Option<NaiveDateTime>,
    first_played_at: Option<NaiveDateTime>,
    ended_at: Option<NaiveDateTime>,
    played_ms: i128,
    completed: bool,
    reason: Option<String>,
}

impl BodyModel for HistoryBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let event_key = bounded_str(fields, "event_key", fields.required_str("event_key"), 1, 300);
        let session_id = fields.required_str("session_id");
        let deck = fields.required_str("deck");
        if deck.as_deref().is_some_and(|d| !["A", "B", "C", "D"].contains(&d)) {
            fields.reject("deck", "literal_error", "Input should be 'A', 'B', 'C' or 'D'");
        }
        let track_id = fields.required_int("track_id");
        let loaded_at = opt_datetime(fields, "loaded_at");
        let first_played_at = opt_datetime(fields, "first_played_at");
        let ended_at = opt_datetime(fields, "ended_at");
        let played_ms = non_negative(fields, "played_ms", 0);
        let completed = fields.bool_or("completed", false);
        let reason = fields.optional_str("reason");
        fields.finish(|| {
            Some(Self {
                event_key: event_key?,
                session_id: session_id?,
                deck: deck?,
                track_id: track_id?,
                loaded_at: loaded_at?,
                first_played_at: first_played_at?,
                ended_at: ended_at?,
                played_ms,
                completed,
                reason,
            })
        })
    }
}

fn opt_ts(at: Option<NaiveDateTime>) -> Value {
    at.map_or(Value::Null, ts)
}

/// `PUT /api/play/history`.
pub async fn upsert_history(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload = match parse_body::<HistoryBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let key = payload.event_key.clone();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                conn.execute(
                    "INSERT INTO play_history (event_key,session_id,deck,track_id,loaded_at,first_played_at,ended_at,played_ms,completed,reason) \
                     VALUES (?,?,?,?,?,?,?,?,?,?) ON CONFLICT (event_key) DO UPDATE SET \
                     first_played_at=coalesce(play_history.first_played_at,excluded.first_played_at), \
                     ended_at=CASE WHEN play_history.completed THEN play_history.ended_at ELSE coalesce(excluded.ended_at,play_history.ended_at) END, \
                     played_ms=CASE WHEN play_history.completed THEN play_history.played_ms ELSE greatest(play_history.played_ms,excluded.played_ms) END, \
                     completed=play_history.completed OR excluded.completed, \
                     reason=CASE WHEN play_history.completed THEN play_history.reason ELSE coalesce(excluded.reason,play_history.reason) END",
                    plumdeck_db::duckdb::params_from_iter([
                        Value::Text(payload.event_key),
                        Value::Text(payload.session_id),
                        Value::Text(payload.deck),
                        Value::HugeInt(payload.track_id),
                        ts(payload.loaded_at.unwrap_or(now)),
                        opt_ts(payload.first_played_at),
                        opt_ts(payload.ended_at),
                        Value::HugeInt(payload.played_ms),
                        Value::Boolean(payload.completed),
                        opt_text(&payload.reason),
                    ]),
                )?;
                Ok(())
            })
        })
        .await;
    match result {
        Ok(()) => PyJson(dict([("event_key", text(key))])).into_response(),
        Err(response) => response,
    }
}

fn limit_only(uri: &Uri) -> Result<i128, Response> {
    let mut params = Params::from_uri(uri);
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    params.finish()?;
    Ok(limit)
}

/// `GET /api/play/history`.
pub async fn history(State(state): State<AppState>, uri: Uri) -> Response {
    let limit = match limit_only(&uri) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    rows(
        &state,
        "SELECT h.*,t.title,t.artist,t.filepath,t.bpm,t.key,t.genre,t.duration FROM play_history h LEFT JOIN tracks t ON t.id=h.track_id \
         ORDER BY coalesce(h.first_played_at,h.loaded_at) DESC LIMIT ?",
        vec![Value::HugeInt(limit)],
    )
    .await
}

// ------------------------------------------------------------------ recordings

const RECORDING_MEDIA_TYPES: [(&str, &str); 8] = [
    (".wav", "audio/wav"),
    (".wave", "audio/wav"),
    (".aif", "audio/aiff"),
    (".aiff", "audio/aiff"),
    (".flac", "audio/flac"),
    (".mp3", "audio/mpeg"),
    (".ogg", "audio/ogg"),
    (".oga", "audio/ogg"),
];

/// (name, extension, label, encoder).
/// (value, extension, label). WAV and FLAC only, written in-process (D-04).
const EXPORT_FORMATS: [(&str, &str, &str); 2] = [("wav", ".wav", "WAV（可逆・最大サイズ）"), ("flac", ".flac", "FLAC（可逆・圧縮）")];

fn recording_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn suffix_lower(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

fn media_type(path: &Path) -> Option<&'static str> {
    let suffix = suffix_lower(path);
    RECORDING_MEDIA_TYPES.iter().find(|(s, _)| *s == suffix).map(|(_, t)| *t)
}

struct RecordingBody {
    key: String,
    session_id: Option<String>,
    filepath: String,
    started_at: NaiveDateTime,
    ended_at: Option<NaiveDateTime>,
    duration_ms: i128,
    status: String,
    error: Option<String>,
    sample_rate_hz: Option<i128>,
    frame_count: Option<i128>,
    timeline_quality: String,
    timeline_dropped_events: i128,
}

impl BodyModel for RecordingBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let key = bounded_str(fields, "recording_key", fields.required_str("recording_key"), 1, 300);
        let session_id = fields.optional_str("session_id");
        let filepath = fields.required_str("filepath");
        let started_at = match fields.value("started_at") {
            None => {
                fields.reject("started_at", "missing", "Field required");
                None
            }
            Some(_) => opt_datetime(fields, "started_at").flatten(),
        };
        let ended_at = opt_datetime(fields, "ended_at");
        let duration_ms = non_negative(fields, "duration_ms", 0);
        let status = fields.choice_or("status", "recording", &["recording", "completed", "failed"]);
        let error = fields.optional_str("error");
        let sample_rate_hz = fields.optional_int("sample_rate_hz");
        if sample_rate_hz.is_some_and(|r| r <= 0) {
            fields.reject("sample_rate_hz", "greater_than", "Input should be greater than 0");
        }
        let frame_count = fields.optional_int("frame_count");
        if frame_count.is_some_and(|c| c < 0) {
            fields.reject("frame_count", "greater_than_equal", "Input should be greater than or equal to 0");
        }
        let timeline_quality = fields.str_or("timeline_quality", "not_recorded");
        let timeline_dropped_events = non_negative(fields, "timeline_dropped_events", 0);
        fields.finish(|| {
            Some(Self {
                key: key?,
                session_id,
                filepath: filepath?,
                started_at: started_at?,
                ended_at: ended_at?,
                duration_ms,
                status,
                error,
                sample_rate_hz,
                frame_count,
                timeline_quality,
                timeline_dropped_events,
            })
        })
    }
}

/// `PUT /api/play/recordings`.
pub async fn upsert_recording(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload = match parse_body::<RecordingBody>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let key = payload.key.clone();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let _guard = recording_lock().lock().unwrap_or_else(|e| e.into_inner());
            db.write(|conn| {
                let o = |x: Option<i128>| x.map_or(Value::Null, Value::HugeInt);
                conn.execute(
                    "INSERT INTO recordings (recording_key,session_id,filepath,started_at,ended_at,duration_ms,status,error, sample_rate_hz,frame_count,timeline_quality,timeline_dropped_events) \
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT (recording_key) DO UPDATE SET \
                     session_id=coalesce(recordings.session_id,excluded.session_id), \
                     filepath=CASE WHEN recordings.status IN ('completed','failed') THEN recordings.filepath WHEN excluded.filepath <> '' THEN excluded.filepath ELSE recordings.filepath END, \
                     started_at=least(recordings.started_at,excluded.started_at), \
                     ended_at=CASE WHEN recordings.status IN ('completed','failed') THEN recordings.ended_at ELSE coalesce(excluded.ended_at,recordings.ended_at) END, \
                     duration_ms=greatest(recordings.duration_ms,excluded.duration_ms), \
                     status=CASE WHEN recordings.status IN ('completed','failed') THEN recordings.status ELSE excluded.status END, \
                     error=CASE WHEN recordings.status IN ('completed','failed') THEN recordings.error ELSE excluded.error END, \
                     sample_rate_hz=coalesce(excluded.sample_rate_hz,recordings.sample_rate_hz), \
                     frame_count=greatest(coalesce(recordings.frame_count,0),coalesce(excluded.frame_count,0)), \
                     timeline_quality=CASE WHEN excluded.timeline_quality='not_recorded' THEN recordings.timeline_quality ELSE excluded.timeline_quality END, \
                     timeline_dropped_events=greatest(recordings.timeline_dropped_events,excluded.timeline_dropped_events)",
                    plumdeck_db::duckdb::params_from_iter([
                        Value::Text(payload.key.clone()),
                        opt_text(&payload.session_id),
                        Value::Text(payload.filepath),
                        ts(payload.started_at),
                        opt_ts(payload.ended_at),
                        Value::HugeInt(payload.duration_ms),
                        Value::Text(payload.status),
                        opt_text(&payload.error),
                        o(payload.sample_rate_hz),
                        o(payload.frame_count),
                        Value::Text(payload.timeline_quality),
                        Value::HugeInt(payload.timeline_dropped_events),
                    ]),
                )?;
                Ok(scalar_int(conn, "SELECT id FROM recordings WHERE recording_key=?", vec![Value::Text(payload.key)])?.unwrap_or(0))
            })
        })
        .await;
    match result {
        Ok(id) => PyJson(dict([("recording_key", text(key)), ("id", int(id))])).into_response(),
        Err(response) => response,
    }
}

fn recording_row(conn: &Connection, id: i128) -> Result<Option<PyValue>, DbError> {
    Ok(json_rows(conn, "SELECT * FROM recordings WHERE id=?", vec![Value::HugeInt(id)])?.into_iter().next())
}

fn field<'a>(row: &'a PyValue, key: &str) -> Option<&'a PyValue> {
    match row {
        PyValue::Dict(pairs) => pairs.iter().find(|(k, _)| matches!(k, PyValue::Str(s) if s == key)).map(|(_, v)| v),
        _ => None,
    }
}

fn row_path(row: &PyValue) -> PathBuf {
    let raw = match field(row, "filepath") {
        Some(PyValue::Str(s)) => s.clone(),
        _ => String::new(),
    };
    PathBuf::from(expand_user(&raw))
}

/// `_recording_file`.
fn recording_file(conn: &Connection, id: i128) -> Result<Result<(PyValue, PathBuf), Response>, DbError> {
    let Some(row) = recording_row(conn, id)? else { return Ok(Err(http(404, "録音が見つかりません"))) };
    if matches!(field(&row, "status"), Some(PyValue::Str(s)) if s == "recording") {
        return Ok(Err(http(409, "録音ファイルを書き込み中です。録音を停止してから再試行してください。")));
    }
    let path = row_path(&row);
    if media_type(&path).is_none() || !path.is_file() {
        return Ok(Err(http(404, "録音ファイルが見つかりません")));
    }
    Ok(Ok((row, path)))
}

/// `GET /api/play/recordings/formats`.
pub async fn recording_formats() -> Response {
    let items = EXPORT_FORMATS
        .iter()
        .map(|f| dict([("value", text(f.0)), ("extension", text(f.1)), ("label", text(f.2))]))
        .collect();
    PyJson(dict([("formats", PyValue::List(items))])).into_response()
}

/// `GET /api/play/recordings/{id}/audio`.
pub async fn recording_audio(State(state): State<AppState>, UrlPath(id): UrlPath<String>, headers: HeaderMap) -> Response {
    let id = match path_int("recording_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let found = state.blocking(move || recording_file(&*db.lease()?, id)).await;
    let path = match found {
        Ok(Ok((_, path))) => path,
        Ok(Err(response)) | Err(response) => return response,
    };
    let kind = media_type(&path).unwrap_or("application/octet-stream");
    let mut response = crate::routes::media::file_response(path, &headers).await;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

struct NameRecording {
    artist: String,
    title: String,
    format: Option<String>,
}

impl BodyModel for NameRecording {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let artist = bounded_str(fields, "artist", Some(fields.str_or("artist", "")), 0, 200);
        let title = bounded_str(fields, "title", fields.required_str("title"), 1, 200);
        let format = match fields.value("format") {
            None | Some(PyValue::None) => None,
            Some(PyValue::Str(s)) if ["wav", "flac"].contains(&s.as_str()) => Some(s.clone()),
            Some(_) => {
                fields.reject("format", "literal_error", "Input should be 'wav' or 'flac'");
                None
            }
        };
        fields.finish(|| Some(Self { artist: artist?, title: title?, format }))
    }
}

fn convert(path: &Path, target: &Path, format: &str) -> Result<(), Response> {
    let Some(details) = EXPORT_FORMATS.iter().find(|f| f.0 == format) else {
        return Err(http(503, format!("{} の書き出しには対応していません", format.to_uppercase())));
    };
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let directory = target.parent().unwrap_or(Path::new("."));
    let temporary = directory.join(format!(".{stem}-{}{}", uuid_like(), details.1));
    let result = (|| {
        crate::recording_export::export(path, &temporary, format).map_err(|message| http(422, format!("録音を {} に変換できません: {message}", format.to_uppercase())))?;
        match std::fs::hard_link(&temporary, target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(http(409, format!("同じ名前のファイルが既にあります: {}", target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())))
            }
            Err(error) => Err(http(500, format!("録音ファイルを書き出せません: {error}"))),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn uuid_like() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}

/// `PATCH /api/play/recordings/{id}`.
pub async fn name_recording(State(state): State<AppState>, UrlPath(id): UrlPath<String>, headers: HeaderMap, body: Bytes) -> Response {
    let id = match path_int("recording_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let payload = match parse_body::<NameRecording>(&headers, &body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking_result(move || -> Result<PyValue, Response> {
            let _guard = recording_lock().lock().unwrap_or_else(|e| e.into_inner());
            let conn = db.lease().map_err(|e| server_error(&e))?;
            let (row, path) = recording_file(&conn, id).map_err(|e| server_error(&e))??;
            drop(conn);
            let strip = plumdeck_core::compat::text::py_strip;
            let (artist, title) = (strip(&payload.artist).to_owned(), strip(&payload.title).to_owned());
            if title.is_empty() {
                return Err(http(422, "ミックス名を入力してください"));
            }
            let stem = if artist.is_empty() { title.clone() } else { format!("{artist} - {title}") };
            let replaced: String = stem.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
            let safe = replaced.trim_matches(|c| c == ' ' || c == '.').to_owned();
            let suffix = match &payload.format {
                Some(format) => EXPORT_FORMATS.iter().find(|f| f.0 == format).map(|f| f.1.to_owned()).unwrap_or_default(),
                None => suffix_lower(&path),
            };
            let target = if safe.is_empty() { path.with_extension(suffix.trim_start_matches('.')) } else { path.with_file_name(format!("{safe}{suffix}")) };
            let mut promoted = false;
            if target != path {
                let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if target.exists() {
                    return Err(http(409, format!("同じ名前のファイルが既にあります: {name}")));
                }
                if suffix_lower(&target) == suffix_lower(&path) {
                    match std::fs::hard_link(&path, &target) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(http(409, format!("同じ名前のファイルが既にあります: {name}"))),
                        Err(error) => return Err(http(500, format!("録音ファイルの名前を変更できません: {error}"))),
                    }
                } else {
                    let format = payload.format.clone().unwrap_or_else(|| suffix_lower(&target).trim_start_matches('.').to_owned());
                    convert(&path, &target, &format)?;
                }
                promoted = true;
            }
            let target_text = target.to_string_lossy().into_owned();
            let artist_value = if artist.is_empty() { Value::Null } else { Value::Text(artist.clone()) };
            let written = db.write(|conn| {
                conn.execute(
                    "UPDATE recordings SET artist=?,title=?,filepath=? WHERE id=?",
                    plumdeck_db::duckdb::params_from_iter([artist_value, Value::Text(title.clone()), Value::Text(target_text.clone()), Value::HugeInt(id)]),
                )?;
                Ok(())
            });
            if let Err(error) = written {
                if promoted && path.is_file() {
                    let _ = std::fs::remove_file(&target);
                }
                return Err(server_error(&error));
            }
            if promoted {
                let _ = std::fs::remove_file(&path);
            }
            Ok(with(row, vec![("artist", if artist.is_empty() { PyValue::None } else { text(artist) }), ("title", text(title)), ("filepath", text(target_text))]))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

/// `DELETE /api/play/recordings/{id}`.
pub async fn discard_recording(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let id = match path_int("recording_id", &id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let _guard = recording_lock().lock().unwrap_or_else(|e| e.into_inner());
            let Some(row) = recording_row(&*db.lease()?, id)? else { return Ok(Err(http(404, "録音が見つかりません"))) };
            if matches!(field(&row, "status"), Some(PyValue::Str(s)) if s == "recording") {
                return Ok(Err(http(409, "録音中のファイルは破棄できません。先に録音を停止してください。")));
            }
            let path = row_path(&row);
            if media_type(&path).is_some() && path.is_file() {
                let _ = std::fs::remove_file(&path);
            }
            db.write(|conn| {
                conn.execute("DELETE FROM recordings WHERE id=?", plumdeck_db::duckdb::params_from_iter([Value::HugeInt(id)]))?;
                Ok(())
            })?;
            Ok(Ok(()))
        })
        .await;
    match result {
        Ok(Ok(())) => PyJson(dict([("deleted", PyValue::Int(id))])).into_response(),
        Ok(Err(response)) | Err(response) => response,
    }
}

/// `GET /api/play/recordings`.
pub async fn recordings(State(state): State<AppState>, uri: Uri) -> Response {
    let limit = match limit_only(&uri) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    rows(&state, "SELECT * FROM recordings ORDER BY started_at DESC LIMIT ?", vec![Value::HugeInt(limit)]).await
}
