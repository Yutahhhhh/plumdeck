//! Library and metadata CSV export / import (backend/api/routers/settings.py
//! over backend/app/services/csv_app_service.py; S8).
//!
//! CSV text is written as Python's `csv.writer` writes it (minimal quoting,
//! `\r\n`, `str()` of each value) and read as `csv.DictReader` reads it.
//! An uploaded file is decoded as UTF-8, else as Shift_JIS with errors dropped.

use std::collections::BTreeMap;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::body::Bytes;
use axum::http::HeaderMap;
use plumdeck_core::compat::pyfmt::{float_repr, py_float_from_str};
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::compat::pyjson_loads::loads;
use plumdeck_core::compat::text::{nfc, py_lower, py_strip};
use plumdeck_db::repos::csv_io::{all_tracks, analysis_extra, export_rows, insert_track, track_at, write_analysis, write_track, NewTrack};
use plumdeck_db::repos::tracks::TrackRow;
use plumdeck_db::DbError;

use crate::body::{parse_body, BodyModel, Fields, ValidationError};
use crate::models::track_dict;
use crate::response::{dict, int, pydantic, text, PyJson};
use crate::AppState;

// ------------------------------------------------------------------ csv text

fn cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn write_row(out: &mut String, cells: &[String]) {
    out.push_str(&cells.iter().map(|c| cell(c)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
}

fn s(value: &Option<String>) -> String {
    value.clone().unwrap_or_default()
}

fn f(value: Option<f64>) -> String {
    value.map(float_repr).unwrap_or_default()
}

fn i(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn b(value: Option<bool>) -> String {
    value.map(|v| if v { "True".into() } else { "False".into() }).unwrap_or_default()
}

/// A `csv.DictReader` row: a short row's missing cells are `None` (restval).
type CsvRow = BTreeMap<String, Option<String>>;

/// `csv.DictReader` rows (a leading BOM removed).
fn read_rows(text: &str) -> Vec<CsvRow> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                any = false;
            }
            other => field.push(other),
        }
    }
    if any {
        record.push(field);
        records.push(record);
    }
    let mut iter = records.into_iter().filter(|r| !(r.len() == 1 && r[0].is_empty()));
    let Some(header) = iter.next() else { return Vec::new() };
    iter.map(|values| header.iter().cloned().zip(values.into_iter().map(Some).chain(std::iter::repeat(None))).collect()).collect()
}

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::SHIFT_JIS.decode_without_bom_handling(bytes).0.replace('\u{fffd}', ""),
    }
}

fn csv_response(body: String, filename: &str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename={filename}")).expect("ascii"));
    response
}

fn json_list(values: &[f64]) -> String {
    dumps(&PyValue::List(values.iter().map(|v| PyValue::Float(*v)).collect()), &DumpOptions::default()).unwrap_or_else(|_| "[]".into())
}

// ------------------------------------------------------------------ exports

/// `GET /api/settings/export/csv`.
pub async fn export_csv(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    let rows = match state.blocking(move || db.lease().and_then(|conn| export_rows(&conn))).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut out = String::new();
    let headers = [
        "filepath", "title", "artist", "album", "genre", "subgenre", "year", "bpm", "key", "energy", "danceability", "brightness",
        "loudness", "noisiness", "contrast", "duration", "loudness_range", "spectral_flux", "spectral_rolloff", "bpm_confidence",
        "key_strength", "bpm_raw", "beat_positions", "waveform_peaks",
    ];
    write_row(&mut out, &headers.map(str::to_owned));
    for row in rows {
        let t = &row.track;
        let extras = row.features_extra_json.as_ref().map(|extra| extra.as_deref().and_then(|e| loads(e).ok()).unwrap_or(PyValue::Dict(Vec::new())));
        let extra = |key: &str| match &extras {
            Some(PyValue::Dict(pairs)) => pairs
                .iter()
                .find(|(k, _)| matches!(k, PyValue::Str(n) if n == key))
                .map(|(_, v)| plumdeck_core::compat::pyobj::py_str(v))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let (beats, peaks) = if row.features_extra_json.is_some() { (json_list(&row.beats), json_list(&row.waveform)) } else { ("[]".into(), "[]".into()) };
        write_row(
            &mut out,
            &[
                t.filepath.clone(), s(&t.title), s(&t.artist), s(&t.album), s(&t.genre), s(&t.subgenre), i(t.year), f(t.bpm), s(&t.key),
                f(t.energy), f(t.danceability), f(t.brightness), f(t.loudness), f(t.noisiness), f(t.contrast), f(t.duration),
                f(t.loudness_range), f(t.spectral_flux), f(t.spectral_rolloff), extra("bpm_confidence"), extra("key_strength"),
                extra("bpm_raw"), beats, peaks,
            ],
        );
    }
    csv_response(out, "plumdeck_library.csv")
}

/// `GET /api/settings/metadata/export`.
pub async fn export_metadata(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    let tracks = match state.blocking(move || db.lease().and_then(|conn| all_tracks(&conn))).await {
        Ok(tracks) => tracks,
        Err(response) => return response,
    };
    let mut out = String::new();
    write_row(&mut out, &["filepath", "title", "artist", "album", "genre", "subgenre", "year", "bpm", "key", "is_genre_verified"].map(str::to_owned));
    for t in tracks {
        write_row(&mut out, &[t.filepath.clone(), s(&t.title), s(&t.artist), s(&t.album), s(&t.genre), s(&t.subgenre), i(t.year), f(t.bpm), s(&t.key), b(t.is_genre_verified)]);
    }
    csv_response(out, "plumdeck_metadata.csv")
}

// ------------------------------------------------------------------ metadata rules

fn norm_path(path: &str) -> String {
    if path.is_empty() {
        String::new()
    } else {
        nfc(py_strip(path))
    }
}

/// `_apply_track_metadata_safely`; `true` when anything changed.
fn apply_safely(track: &mut TrackRow, data: &[(PyValue, PyValue)]) -> bool {
    let get = |key: &str| data.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v);
    let mut changed = false;
    for (field, slot) in [
        ("title", &mut track.title),
        ("artist", &mut track.artist),
        ("album", &mut track.album),
        ("genre", &mut track.genre),
        ("subgenre", &mut track.subgenre),
        ("key", &mut track.key),
    ] {
        if let Some(value) = get(field).filter(|v| **v != PyValue::None) {
            let cleaned = py_strip(&plumdeck_core::compat::pyobj::py_str(value)).to_owned();
            if !cleaned.is_empty() && py_lower(&cleaned) != "unknown" && slot.as_deref() != Some(cleaned.as_str()) {
                *slot = Some(cleaned);
                changed = true;
            }
        }
    }
    if let Some(PyValue::Int(year)) = get("year") {
        if *year > 0 && track.year != i64::try_from(*year).ok() {
            track.year = i64::try_from(*year).ok();
            changed = true;
        }
    }
    let bpm = match get("bpm") {
        Some(PyValue::Int(n)) => Some(*n as f64),
        Some(PyValue::Float(x)) => Some(*x),
        _ => None,
    };
    if let Some(bpm) = bpm.filter(|bpm| *bpm > 0.0) {
        if track.bpm != Some(bpm) {
            track.bpm = Some(bpm);
            changed = true;
        }
    }
    if let Some(PyValue::Bool(verified)) = get("is_genre_verified") {
        if track.is_genre_verified != Some(*verified) {
            track.is_genre_verified = Some(*verified);
            changed = true;
        }
    }
    for (field, slot) in [
        ("energy", &mut track.energy),
        ("danceability", &mut track.danceability),
        ("brightness", &mut track.brightness),
        ("loudness", &mut track.loudness),
        ("noisiness", &mut track.noisiness),
        ("contrast", &mut track.contrast),
    ] {
        let value = match get(field) {
            None | Some(PyValue::None) => continue,
            Some(PyValue::Int(n)) => Some(*n as f64),
            Some(PyValue::Float(x)) => Some(*x),
            Some(PyValue::Bool(flag)) => Some(f64::from(u8::from(*flag))),
            Some(PyValue::Str(text)) => py_float_from_str(text),
            _ => None,
        };
        if let Some(value) = value {
            if *slot != Some(value) {
                *slot = Some(value);
                changed = true;
            }
        }
    }
    changed
}

fn dict_get<'a>(pairs: &'a [(PyValue, PyValue)], key: &str) -> Option<&'a PyValue> {
    pairs.iter().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v)
}

fn dict_set(pairs: &mut Vec<(PyValue, PyValue)>, key: &str, value: PyValue) {
    match pairs.iter_mut().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)) {
        Some(slot) => slot.1 = value,
        None => pairs.push((PyValue::Str(key.into()), value)),
    }
}

/// `json.dumps(value_sources.with_sources(analysis.features_extra, {field: "manual"}))`
/// (`features_extra` is `{}` when the stored text is not a JSON object).
fn with_manual(extra: &str, fields: &[&str]) -> String {
    let mut updated = match loads(extra) {
        Ok(PyValue::Dict(pairs)) => pairs,
        _ => Vec::new(),
    };
    // stored_sources: recorded bpm/key string sources, then the legacy marker.
    let mut merged: Vec<(PyValue, PyValue)> = Vec::new();
    if let Some(PyValue::Dict(components)) = dict_get(&updated, "analysis_components") {
        if let Some(PyValue::Dict(recorded)) = dict_get(components, "sources") {
            for (key, source) in recorded {
                if matches!(key, PyValue::Str(k) if k == "bpm" || k == "key") && matches!(source, PyValue::Str(_)) {
                    merged.push((key.clone(), source.clone()));
                }
            }
        }
    }
    let authority = dict_get(&updated, "bpm_authority").is_some_and(plumdeck_core::compat::pyobj::truthy);
    if authority && dict_get(&merged, "bpm").is_none() {
        merged.push((PyValue::Str("bpm".into()), PyValue::Str("rekordbox".into())));
    }
    for field in fields {
        dict_set(&mut merged, field, PyValue::Str("manual".into()));
    }
    let mut components = match dict_get(&updated, "analysis_components") {
        Some(PyValue::Dict(components)) => components.clone(),
        _ => Vec::new(),
    };
    dict_set(&mut components, "sources", PyValue::Dict(merged));
    dict_set(&mut updated, "analysis_components", PyValue::Dict(components));
    if fields.contains(&"bpm") {
        updated.retain(|(k, _)| !matches!(k, PyValue::Str(n) if n == "bpm_authority"));
    }
    dumps(&PyValue::Dict(updated), &DumpOptions::default()).unwrap_or_else(|_| "{}".into())
}

/// `_apply_with_provenance`: apply, then record edited BPM/key as manual.
/// The caller writes the track (one UPDATE of the changed columns).
fn apply_with_provenance(conn: &plumdeck_db::duckdb::Connection, track: &mut TrackRow, data: &[(PyValue, PyValue)]) -> Result<bool, DbError> {
    let before = (track.bpm, track.key.clone());
    if !apply_safely(track, data) {
        return Ok(false);
    }
    let mut edited = Vec::new();
    if track.bpm != before.0 {
        edited.push("bpm");
    }
    if track.key != before.1 {
        edited.push("key");
    }
    if !edited.is_empty() {
        let extra = analysis_extra(conn, track.id)?.unwrap_or_else(|| "{}".into());
        write_analysis(conn, track.id, &with_manual(&extra, &edited), None, None)?;
    }
    Ok(true)
}

// ------------------------------------------------------------------ uploads

async fn uploaded_text(mut multipart: Multipart) -> Option<String> {
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            return field.bytes().await.ok().map(|bytes| decode(&bytes));
        }
    }
    None
}

fn missing_file() -> Response {
    crate::body::validation_response(&[crate::body::ValidationError::new(
        "missing",
        vec![crate::body::Loc::from("body"), crate::body::Loc::from("file")],
        "Field required",
        PyValue::None,
    )])
}

/// `row.get(key)`: `None` when the column is absent or the row is short.
fn cell_of<'a>(row: &'a CsvRow, key: &str) -> Option<&'a str> {
    row.get(key).and_then(|v| v.as_deref())
}

fn metadata_row(row: &CsvRow) -> PyValue {
    let opt = |key: &str| cell_of(row, key).map_or(PyValue::None, |v| PyValue::Str(v.to_owned()));
    let year = cell_of(row, "year").filter(|y| !y.is_empty() && y.chars().all(|c| c.is_ascii_digit())).and_then(|y| y.parse::<i128>().ok());
    // _parse_bpm(None) is float("None"): no BPM.
    let bpm = cell_of(row, "bpm").and_then(|v| py_float_from_str(py_strip(v))).filter(|bpm| bpm.is_finite() && *bpm > 0.0);
    let verified = cell_of(row, "is_genre_verified").filter(|v| !v.is_empty()).map(|v| ["true", "1", "yes", "on"].contains(&py_lower(v).as_str()));
    dict([
        ("filepath", text(cell_of(row, "filepath").unwrap_or_default())),
        ("title", opt("title")),
        ("artist", opt("artist")),
        ("album", opt("album")),
        ("genre", opt("genre")),
        ("subgenre", opt("subgenre")),
        ("year", year.map_or(PyValue::None, PyValue::Int)),
        ("bpm", bpm.map_or(PyValue::None, PyValue::Float)),
        ("key", opt("key")),
        ("is_genre_verified", verified.map_or(PyValue::None, PyValue::Bool)),
    ])
}

fn pairs(value: &PyValue) -> &[(PyValue, PyValue)] {
    match value {
        PyValue::Dict(pairs) => pairs,
        _ => &[],
    }
}

/// `POST /api/settings/metadata/import/analyze`.
pub async fn analyze_metadata(State(state): State<AppState>, multipart: Result<Multipart, MultipartRejection>) -> Response {
    let Ok(multipart) = multipart else { return missing_file() };
    let Some(text_body) = uploaded_text(multipart).await else { return missing_file() };
    let db = state.db.clone();
    let tracks = match state.blocking(move || db.lease().and_then(|conn| all_tracks(&conn))).await {
        Ok(tracks) => tracks,
        Err(response) => return response,
    };
    let by_path: BTreeMap<String, &TrackRow> = tracks.iter().map(|t| (norm_path(&t.filepath), t)).collect();
    let (mut updates, mut not_found) = (Vec::new(), Vec::new());
    for row in read_rows(&text_body) {
        let path = norm_path(cell_of(&row, "filepath").unwrap_or(""));
        if path.is_empty() {
            continue;
        }
        let import = metadata_row(&row);
        match by_path.get(&path) {
            Some(current) => {
                let mut dummy = (*current).clone();
                if apply_safely(&mut dummy, pairs(&import)) {
                    updates.push(dict([("current", track_dict(current, Vec::new())), ("new", import)]));
                }
            }
            None => not_found.push(import),
        }
    }
    let total = (updates.len() + not_found.len()) as i64;
    pydantic(dict([("total_rows", int(total)), ("updates", PyValue::List(updates)), ("not_found", PyValue::List(not_found))])).into_response()
}

/// A `Dict[str, Any]` item rebuilt from its fields (the last of duplicate keys).
fn whole(fields: &Fields<'_>) -> PyValue {
    PyValue::Dict(fields.names().into_iter().map(|name| (PyValue::Str(name.into()), fields.value(name).cloned().unwrap_or(PyValue::None))).collect())
}

/// `MetadataImportExecuteRequest`: `updates: List[Dict[str, Any]]`.
struct MetadataExecuteBody {
    updates: Vec<PyValue>,
}

impl BodyModel for MetadataExecuteBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let updates = fields.model_list("updates", |item| Some(whole(item)));
        fields.finish(|| updates.map(|updates| Self { updates }))
    }
}

/// `POST /api/settings/metadata/import/execute`.
pub async fn execute_metadata(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let updates = match parse_body::<MetadataExecuteBody>(&headers, &body) {
        Ok(request) => request.updates,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                let mut count = 0i64;
                // no_autoflush: lookups read the stored rows (a track already
                // edited is the session's object); the UPDATEs run at commit,
                // in primary-key order.
                let mut pending: BTreeMap<i64, (TrackRow, TrackRow)> = BTreeMap::new();
                for item in &updates {
                    let item = pairs(item);
                    let get = |key: &str| item.iter().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v);
                    let Some(data) = get("new").filter(|v| plumdeck_core::compat::pyobj::truthy(v)).or_else(|| get("data")) else { continue };
                    if !plumdeck_core::compat::pyobj::truthy(data) {
                        continue;
                    }
                    let path = match pairs(data).iter().find(|(k, _)| matches!(k, PyValue::Str(n) if n == "filepath")) {
                        Some((_, PyValue::Str(path))) => norm_path(path),
                        _ => String::new(),
                    };
                    if let Some(stored) = track_at(conn, &path)? {
                        let (before, mut track) = match pending.get(&stored.id) {
                            Some((before, track)) => (before.clone(), track.clone()),
                            None => (stored.clone(), stored),
                        };
                        if apply_with_provenance(conn, &mut track, pairs(data))? {
                            count += 1;
                        }
                        pending.insert(track.id, (before, track));
                    }
                }
                for (before, track) in pending.values() {
                    write_track(conn, before, track)?;
                }
                Ok(count)
            })
        })
        .await;
    match result {
        Ok(count) => PyJson(dict([
            ("status", text("success")),
            ("updated", int(count)),
            ("message", text(format!("Successfully updated {count} tracks."))),
        ]))
        .into_response(),
        Err(response) => response,
    }
}

/// A library CSV row as `CsvImportRow`; `None` when Python's
/// `except: continue` skips it (an unparsable number, a short row's
/// missing filepath/title/artist, a JSON array cell that is not a list of
/// numbers).
fn import_row(row: &CsvRow) -> Option<PyValue> {
    let required = |key: &str| row.get(key).map_or(Some(String::new()), |v| v.clone());
    let optional = |key: &str| row.get(key).map_or(PyValue::Str(String::new()), |v| v.clone().map_or(PyValue::None, PyValue::Str));
    // safe_f: float(v) if v else 0.0
    let number = |key: &str| match cell_of(row, key) {
        None | Some("") => Some(0.0),
        Some(value) => py_float_from_str(value),
    };
    // safe_j, then Optional[List[float]]
    let list = |key: &str| -> Option<PyValue> {
        let value = cell_of(row, key).unwrap_or_default();
        if value.is_empty() {
            return Some(PyValue::List(Vec::new()));
        }
        match loads(value) {
            Ok(PyValue::List(items)) => items
                .iter()
                .map(|item| match item {
                    PyValue::Str(_) | PyValue::Int(_) | PyValue::Float(_) | PyValue::Bool(_) => crate::lax::to_float(item).map(PyValue::Float),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .map(PyValue::List),
            Ok(PyValue::None) => Some(PyValue::None),
            Ok(_) => None,
            Err(_) => Some(PyValue::List(Vec::new())),
        }
    };
    let year = cell_of(row, "year").filter(|y| !y.is_empty() && y.chars().all(|c| c.is_ascii_digit())).and_then(|y| y.parse::<i128>().ok());
    let mut fields = vec![
        ("filepath", text(required("filepath")?)),
        ("title", text(required("title")?)),
        ("artist", text(required("artist")?)),
        ("album", optional("album")),
        ("genre", optional("genre")),
        ("subgenre", optional("subgenre")),
        ("year", year.map_or(PyValue::None, PyValue::Int)),
        ("bpm", PyValue::Float(number("bpm")?)),
        ("key", optional("key")),
    ];
    for key in [
        "energy", "danceability", "brightness", "loudness", "noisiness", "contrast", "duration", "loudness_range", "spectral_flux",
        "spectral_rolloff", "bpm_confidence", "key_strength", "bpm_raw",
    ] {
        fields.push((key, PyValue::Float(number(key)?)));
    }
    fields.push(("beat_positions", list("beat_positions")?));
    fields.push(("waveform_peaks", list("waveform_peaks")?));
    Some(dict(fields))
}

fn field<'a>(value: &'a PyValue, key: &str) -> Option<&'a PyValue> {
    pairs(value).iter().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v)
}

fn field_str(value: &PyValue, key: &str) -> String {
    match field(value, key) {
        Some(PyValue::Str(text)) => text.clone(),
        _ => String::new(),
    }
}

/// `POST /api/settings/import/analyze`.
pub async fn analyze_import(State(state): State<AppState>, multipart: Result<Multipart, MultipartRejection>) -> Response {
    let Ok(multipart) = multipart else { return missing_file() };
    let Some(text_body) = uploaded_text(multipart).await else { return missing_file() };
    let db = state.db.clone();
    let tracks = match state.blocking(move || db.lease().and_then(|conn| all_tracks(&conn))).await {
        Ok(tracks) => tracks,
        Err(response) => return response,
    };
    let paths: std::collections::BTreeSet<String> = tracks.iter().map(|t| norm_path(&t.filepath)).collect();
    let mut by_meta: BTreeMap<(String, String), Vec<&TrackRow>> = BTreeMap::new();
    for track in &tracks {
        if let (Some(title), Some(artist)) = (track.title.as_deref().filter(|t| !t.is_empty()), track.artist.as_deref().filter(|a| !a.is_empty())) {
            by_meta.entry((py_strip(&py_lower(title)).to_owned(), py_strip(&py_lower(artist)).to_owned())).or_default().push(track);
        }
    }
    let (mut new_tracks, mut duplicates, mut path_updates) = (Vec::new(), Vec::new(), Vec::new());
    let mut matched = std::collections::BTreeSet::new();
    for row in read_rows(&text_body) {
        let Some(import) = import_row(&row) else { continue };
        let path = norm_path(&field_str(&import, "filepath"));
        if path.is_empty() {
            continue;
        }
        if paths.contains(&path) {
            duplicates.push(import);
            continue;
        }
        let key = (py_strip(&py_lower(&field_str(&import, "title"))).to_owned(), py_strip(&py_lower(&field_str(&import, "artist"))).to_owned());
        let original = by_meta.get(&key).and_then(|candidates| candidates.iter().find(|t| !matched.contains(&t.id)).copied());
        match original {
            Some(original) => {
                matched.insert(original.id);
                path_updates.push(dict([
                    ("old_path", text(original.filepath.clone())),
                    ("new_path", text(field_str(&import, "filepath"))),
                    ("track", import),
                    ("original_id", int(original.id)),
                ]));
            }
            None => new_tracks.push(import),
        }
    }
    let total = (new_tracks.len() + duplicates.len() + path_updates.len()) as i64;
    pydantic(dict([
        ("total_rows", int(total)),
        ("new_tracks", PyValue::List(new_tracks)),
        ("duplicates", PyValue::List(duplicates)),
        ("path_updates", PyValue::List(path_updates)),
    ]))
    .into_response()
}

fn floats(value: Option<&PyValue>) -> Vec<f64> {
    match value {
        Some(PyValue::List(items)) => items.iter().filter_map(|v| match v {
            PyValue::Int(n) => Some(*n as f64),
            PyValue::Float(x) => Some(*x),
            _ => None,
        }).collect(),
        _ => Vec::new(),
    }
}

fn num(value: Option<&PyValue>) -> Option<f64> {
    match value {
        Some(PyValue::Int(n)) => Some(*n as f64),
        Some(PyValue::Float(x)) => Some(*x),
        _ => None,
    }
}

/// `Optional[float] = 0.0` in lax mode.
fn lax_float(fields: &Fields<'_>, name: &str) -> PyValue {
    match fields.value(name) {
        None => PyValue::Float(0.0),
        Some(PyValue::None) => PyValue::None,
        Some(value @ (PyValue::Str(_) | PyValue::Int(_) | PyValue::Float(_) | PyValue::Bool(_))) => match crate::lax::to_float(value) {
            Some(number) => PyValue::Float(number),
            None => {
                fields.reject(name, "float_parsing", "Input should be a valid number, unable to parse string as a number");
                PyValue::None
            }
        },
        Some(_) => {
            fields.reject(name, "float_type", "Input should be a valid number");
            PyValue::None
        }
    }
}

/// `Optional[List[float]] = []`.
fn float_list(fields: &Fields<'_>, name: &str) -> PyValue {
    match fields.value(name) {
        None => PyValue::List(Vec::new()),
        Some(PyValue::None) => PyValue::None,
        Some(PyValue::List(items)) => {
            let parsed: Option<Vec<PyValue>> = items
                .iter()
                .map(|item| match item {
                    PyValue::Str(_) | PyValue::Int(_) | PyValue::Float(_) | PyValue::Bool(_) => crate::lax::to_float(item).map(PyValue::Float),
                    _ => None,
                })
                .collect();
            parsed.map(PyValue::List).unwrap_or_else(|| {
                fields.reject(name, "float_parsing", "Input should be a valid number");
                PyValue::None
            })
        }
        Some(_) => {
            fields.reject(name, "list_type", "Input should be a valid list");
            PyValue::None
        }
    }
}

/// `CsvImportRow.model_dump()`: every field, defaults filled in.
fn csv_import_row(fields: &Fields<'_>) -> Option<PyValue> {
    let optional_text = |name: &str| match fields.value(name) {
        None => PyValue::Str(String::new()),
        Some(_) => fields.optional_str(name).map_or(PyValue::None, PyValue::Str),
    };
    let filepath = fields.required_str("filepath");
    let title = fields.required_str("title");
    let artist = fields.required_str("artist");
    let mut out = vec![
        ("filepath", filepath.map(PyValue::Str).unwrap_or(PyValue::None)),
        ("title", title.map(PyValue::Str).unwrap_or(PyValue::None)),
        ("artist", artist.map(PyValue::Str).unwrap_or(PyValue::None)),
        ("album", optional_text("album")),
        ("genre", optional_text("genre")),
        ("subgenre", optional_text("subgenre")),
        ("year", fields.optional_int("year").map_or(PyValue::None, PyValue::Int)),
        ("bpm", lax_float(fields, "bpm")),
        ("key", optional_text("key")),
    ];
    for name in [
        "energy", "danceability", "brightness", "loudness", "noisiness", "contrast", "duration", "loudness_range", "spectral_flux",
        "spectral_rolloff", "bpm_confidence", "key_strength", "bpm_raw",
    ] {
        out.push((name, lax_float(fields, name)));
    }
    out.push(("beat_positions", float_list(fields, "beat_positions")));
    out.push(("waveform_peaks", float_list(fields, "waveform_peaks")));
    Some(dict(out))
}

/// `ImportExecuteRequest`.
struct ImportExecuteBody {
    new_tracks: Vec<PyValue>,
    path_updates: Vec<PyValue>,
}

impl BodyModel for ImportExecuteBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let new_tracks = fields.model_list("new_tracks", csv_import_row);
        let path_updates = fields.model_list("path_updates", |item| Some(whole(item)));
        fields.finish(|| Some(Self { new_tracks: new_tracks?, path_updates: path_updates? }))
    }
}

/// `POST /api/settings/import/execute`.
pub async fn execute_import(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_body::<ImportExecuteBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                let (mut imported, mut updated) = (0i64, 0i64);
                {
                    // As in the metadata import: UPDATEs at the first flush, by id.
                    let mut pending: BTreeMap<i64, (TrackRow, TrackRow)> = BTreeMap::new();
                    for item in &request.path_updates {
                        let (old, new) = (field_str(item, "old_path"), field_str(item, "new_path"));
                        let Some(stored) = track_at(conn, &old)? else { continue };
                        let (before, mut track) = match pending.get(&stored.id) {
                            Some((before, track)) => (before.clone(), track.clone()),
                            None => (stored.clone(), stored),
                        };
                        track.filepath = new;
                        if let Some(data) = field(item, "track").filter(|v| plumdeck_core::compat::pyobj::truthy(v)) {
                            if apply_with_provenance(conn, &mut track, pairs(data))? {
                                let extra = analysis_extra(conn, track.id)?.unwrap_or_else(|| "{}".into());
                                let beats = floats(field(data, "beat_positions"));
                                let peaks = floats(field(data, "waveform_peaks"));
                                write_analysis(
                                    conn,
                                    track.id,
                                    &extra,
                                    (!beats.is_empty()).then_some(beats.as_slice()),
                                    (!peaks.is_empty()).then_some(peaks.as_slice()),
                                )?;
                            }
                        }
                        pending.insert(track.id, (before, track));
                        updated += 1;
                    }
                    for (before, track) in pending.values() {
                        write_track(conn, before, track)?;
                    }
                }
                {
                    for row in &request.new_tracks {
                        let path = norm_path(&field_str(row, "filepath"));
                        if track_at(conn, &path)?.is_some() {
                            continue;
                        }
                        let opt = |key: &str| match field(row, key) {
                            Some(PyValue::Str(text)) => Some(text.clone()),
                            _ => None,
                        };
                        let title = field_str(row, "title");
                        let artist = field_str(row, "artist");
                        let new = NewTrack {
                            filepath: field_str(row, "filepath"),
                            title: if title.is_empty() { "Unknown".into() } else { title },
                            artist: if artist.is_empty() { "Unknown".into() } else { artist },
                            album: opt("album"),
                            genre: opt("genre"),
                            subgenre: opt("subgenre"),
                            year: match field(row, "year") {
                                Some(PyValue::Int(n)) => i64::try_from(*n).ok(),
                                _ => None,
                            },
                            bpm: num(field(row, "bpm")),
                            key: opt("key"),
                            energy: num(field(row, "energy")),
                            danceability: num(field(row, "danceability")),
                            brightness: num(field(row, "brightness")),
                            loudness: num(field(row, "loudness")),
                            noisiness: num(field(row, "noisiness")),
                            contrast: num(field(row, "contrast")),
                            duration: num(field(row, "duration")),
                            loudness_range: num(field(row, "loudness_range")),
                            spectral_flux: num(field(row, "spectral_flux")),
                            spectral_rolloff: num(field(row, "spectral_rolloff")),
                        };
                        let mut supplied = Vec::new();
                        if new.bpm.is_some_and(|bpm| bpm.is_finite() && bpm > 0.0) {
                            supplied.push("bpm");
                        }
                        if new.key.as_deref().is_some_and(|key| !py_strip(key).is_empty() && py_lower(py_strip(key)) != "unknown") {
                            supplied.push("key");
                        }
                        let extra = if supplied.is_empty() { "{}".to_owned() } else { with_manual("{}", &supplied) };
                        let id = insert_track(conn, &new, now)?;
                        let array = |key: &str| match field(row, key) {
                            Some(PyValue::None) => None,
                            other => Some(floats(other)),
                        };
                        let (beats, peaks) = (array("beat_positions"), array("waveform_peaks"));
                        write_analysis(conn, id, &extra, beats.as_deref(), peaks.as_deref())?;
                        imported += 1;
                    }
                }
                Ok((imported, updated))
            })
        })
        .await;
    match result {
        Ok((imported, updated)) => PyJson(dict([
            ("status", text("success")),
            ("imported", int(imported)),
            ("updated", int(updated)),
            ("message", text(format!("Successfully imported {imported} new tracks and updated {updated} paths."))),
        ]))
        .into_response(),
        Err(response) => response,
    }
}
