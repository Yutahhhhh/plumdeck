//! The wordplay pair routes (backend/api/routers/wordplay.py over
//! backend/app/services/wordplay_app_service.py, S4): list, approve,
//! update and reject. `POST /api/wordplay-pairs` is not ported (only tests
//! called it; the MCP tool keeps proposing pairs, deviation D-17); its
//! service rules are [`propose`] for the MCP port.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::{casefold, py_strip};
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::tracks::{get_track, BigInt, TrackRow};
use plumdeck_db::repos::wordplay::{delete_pair, find_pair, get_pair, insert_pair, list_pairs, update_pair, PairRow};
use plumdeck_db::DbError;
use plumdeck_domain::wordplay::{
    fit, is_http_url, normalize_keyword, Cues, TrackFacts, EVIDENCE_TYPES, SECTION_POSITIONS, SOURCE_CUE_MODES,
    VERIFICATION_STATUSES,
};

use crate::body::{parse_body_for, BodyModel, Fields, ValidationError};
use crate::models::track_dict;
use crate::query::Params;
use crate::response::{boolean, datetime, dict, float, int, opt, pydantic, text};
use crate::{server_error, AppState, HttpError};

/// The fields a client may set, in the service's order.
pub const ALLOWED: [&str; 17] = [
    "from_track_id",
    "to_track_id",
    "keyword",
    "source_phrase",
    "target_phrase",
    "source_section_position",
    "target_section_position",
    "source_cue_mode",
    "from_timestamp",
    "source_cue_end_timestamp",
    "to_timestamp",
    "target_intro_timestamp",
    "target_landing_timestamp",
    "transition_notes",
    "source_url",
    "evidence_type",
    "verification_status",
];

const TIMESTAMPS: [&str; 5] =
    ["from_timestamp", "source_cue_end_timestamp", "to_timestamp", "target_intro_timestamp", "target_landing_timestamp"];

const TRANSITION_FIELDS: [&str; 13] = [
    "from_track_id",
    "to_track_id",
    "keyword",
    "source_phrase",
    "target_phrase",
    "source_section_position",
    "target_section_position",
    "source_cue_mode",
    "from_timestamp",
    "source_cue_end_timestamp",
    "to_timestamp",
    "target_intro_timestamp",
    "target_landing_timestamp",
];

/// A service failure and the status the router maps it to.
#[derive(Debug)]
pub enum WordplayError {
    /// `WordplayValidationError`: 422.
    Validation(String),
    /// `WordplayNotFoundError`: 404.
    NotFound(String),
    /// `WordplayConflictError`: 409.
    Conflict(String),
    /// Anything else: 500.
    Server(String),
    Db(DbError),
}

impl From<DbError> for WordplayError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl WordplayError {
    fn response(self) -> Response {
        match self {
            Self::Validation(detail) => HttpError(ApiError::new(422, detail)).into_response(),
            Self::NotFound(detail) => HttpError(ApiError::new(404, detail)).into_response(),
            Self::Conflict(detail) => HttpError(ApiError::conflict(detail)).into_response(),
            Self::Server(reason) => server_error(&reason),
            Self::Db(error) => server_error(&error),
        }
    }
}

fn invalid(message: impl Into<String>) -> WordplayError {
    WordplayError::Validation(message.into())
}

fn facts(track: &TrackRow) -> TrackFacts {
    TrackFacts {
        genre: track.genre.clone(),
        subgenre: track.subgenre.clone(),
        bpm: track.bpm,
        duration: track.duration,
        energy: track.energy,
        danceability: track.danceability,
        brightness: track.brightness,
        noisiness: track.noisiness,
    }
}

fn cues(pair: &PairRow) -> Cues {
    Cues {
        source_section_position: pair.source_section_position.clone(),
        target_section_position: pair.target_section_position.clone(),
        source_cue_mode: pair.source_cue_mode.clone(),
        from_timestamp: pair.from_timestamp,
        source_cue_end_timestamp: pair.source_cue_end_timestamp,
        to_timestamp: pair.to_timestamp,
        target_intro_timestamp: pair.target_intro_timestamp,
        target_landing_timestamp: pair.target_landing_timestamp,
    }
}

/// `_item`: the pair (without `normalized_keyword`), both tracks and the
/// fit flags. Also returns `boundary_fit`.
pub fn item(conn: &Connection, pair: &PairRow) -> Result<(PyValue, bool), DbError> {
    let from = get_track(conn, BigInt(pair.from_track_id.into()))?;
    let to = get_track(conn, BigInt(pair.to_track_id.into()))?;
    let (from_facts, to_facts) = (from.as_ref().map(facts), to.as_ref().map(facts));
    let flags = fit(&cues(pair), from_facts.as_ref(), to_facts.as_ref());
    let f = |value: Option<f64>| opt(value, float);
    let track = |row: &Option<TrackRow>| row.as_ref().map_or(PyValue::None, |row| track_dict(row, Vec::new()));
    let value = dict([
        ("id", int(pair.id)),
        ("from_track_id", int(pair.from_track_id)),
        ("to_track_id", int(pair.to_track_id)),
        ("keyword", text(pair.keyword.clone())),
        ("source_phrase", text(pair.source_phrase.clone())),
        ("target_phrase", text(pair.target_phrase.clone())),
        ("source_section_position", text(pair.source_section_position.clone())),
        ("target_section_position", text(pair.target_section_position.clone())),
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
        ("status", text(pair.status.clone())),
        ("created_at", opt(pair.created_at.as_ref(), datetime)),
        ("updated_at", opt(pair.updated_at.as_ref(), datetime)),
        ("from_track", track(&from)),
        ("to_track", track(&to)),
        ("bpm_delta_percent", f(flags.bpm_delta_percent)),
        ("style_fit", boolean(flags.style_fit)),
        ("source_cue_fit", boolean(flags.source_cue_fit)),
        ("target_timing_fit", boolean(flags.target_timing_fit)),
        ("boundary_fit", boolean(flags.boundary_fit)),
    ]);
    Ok((value, flags.boundary_fit))
}

fn get<'a>(value: &'a PyValue, key: &str) -> Option<&'a PyValue> {
    match value {
        PyValue::Dict(pairs) => pairs.iter().find(|(name, _)| matches!(name, PyValue::Str(k) if k == key)).map(|(_, v)| v),
        _ => None,
    }
}

/// `str(item.get(key) or "")` for the search.
fn search_text(value: Option<&PyValue>) -> String {
    match value {
        Some(PyValue::Str(text)) => text.clone(),
        Some(PyValue::Int(number)) if *number != 0 => number.to_string(),
        _ => String::new(),
    }
}

/// `GET /api/wordplay-pairs`.
pub async fn list(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let status = params.opt_str("status");
    if let Some(status) = &status {
        if status != "pending" && status != "approved" {
            params.choice("status", "pending", &["pending", "approved"]);
        }
    }
    let from_track_id = params.opt_int("from_track_id");
    let query = params.opt_str("query");
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let pairs = list_pairs(&conn, status.as_deref(), from_track_id.map(BigInt))?;
            let mut items = Vec::with_capacity(pairs.len());
            for pair in &pairs {
                items.push(item(&conn, pair)?.0);
            }
            Ok(items)
        })
        .await;
    let mut items = match result {
        Ok(items) => items,
        Err(response) => return response,
    };
    if let Some(needle) = query.as_deref().map(py_strip).filter(|needle| !needle.is_empty()) {
        let needle = casefold(needle);
        items.retain(|item| {
            let mut values: Vec<String> =
                ["keyword", "source_phrase", "target_phrase", "transition_notes", "source_url"].iter().map(|key| search_text(get(item, key))).collect();
            for track_key in ["from_track", "to_track"] {
                if let Some(track) = get(item, track_key) {
                    values.extend(["title", "artist", "album"].iter().map(|key| search_text(get(track, key))));
                }
            }
            values.iter().any(|value| casefold(value).contains(&needle))
        });
    }
    let total = items.len() as i64;
    let (start, end) = (offset.max(0) as usize, (offset + limit).max(0) as usize);
    let page: Vec<PyValue> = items.into_iter().skip(start).take(end.saturating_sub(start)).collect();
    pydantic(dict([("items", PyValue::List(page)), ("total", int(total))])).into_response()
}

/// `_validate_tracks`.
fn validate_tracks(conn: &Connection, from: &PyValue, to: &PyValue) -> Result<(TrackRow, TrackRow), WordplayError> {
    let (PyValue::Int(from), PyValue::Int(to)) = (from, to) else {
        return Err(invalid("from_track_id and to_track_id are required integers"));
    };
    if from == to {
        return Err(invalid("from_track_id and to_track_id must be distinct"));
    }
    let from_track = get_track(conn, BigInt(*from))?;
    let to_track = get_track(conn, BigInt(*to))?;
    let missing: Vec<String> =
        [(from, &from_track), (to, &to_track)].iter().filter(|(_, track)| track.is_none()).map(|(id, _)| id.to_string()).collect();
    if !missing.is_empty() {
        return Err(WordplayError::NotFound(format!("Track(s) not found: {}", missing.join(", "))));
    }
    Ok((from_track.expect("checked"), to_track.expect("checked")))
}

/// A number field as Python holds it (`None` for anything else).
fn number(value: Option<&PyValue>) -> Option<f64> {
    match value {
        Some(PyValue::Int(number)) => Some(*number as f64),
        Some(PyValue::Float(number)) => Some(*number),
        _ => None,
    }
}


fn str_of<'a>(values: &'a BTreeMap<&str, PyValue>, key: &str) -> Option<&'a str> {
    match values.get(key) {
        Some(PyValue::Str(text)) => Some(text),
        _ => None,
    }
}

/// `_validate_fields`.
fn validate_fields(values: &BTreeMap<&str, PyValue>) -> Result<(), WordplayError> {
    let keyword = values.get("keyword").cloned().unwrap_or(PyValue::Str(String::new()));
    match &keyword {
        PyValue::Str(text) if !normalize_keyword(text).is_empty() => {}
        _ => return Err(invalid("keyword must not be blank")),
    }
    for field in ["source_phrase", "target_phrase"] {
        if !str_of(values, field).is_some_and(|text| !py_strip(text).is_empty()) {
            return Err(invalid(format!("{field} must not be blank")));
        }
    }
    if values.get("transition_notes").is_some_and(|value| !matches!(value, PyValue::Str(_))) {
        return Err(invalid("transition_notes must be a string"));
    }
    if values.get("source_url").is_some_and(|value| !matches!(value, PyValue::Str(_))) {
        return Err(invalid("source_url must be a string"));
    }
    for field in TIMESTAMPS {
        match values.get(field) {
            None | Some(PyValue::None) => {}
            Some(PyValue::Int(number)) if *number >= 0 => {}
            Some(PyValue::Float(number)) if number.is_finite() && *number >= 0.0 => {}
            Some(_) => return Err(invalid(format!("{field} must be a finite, nonnegative number"))),
        }
    }
    let url = str_of(values, "source_url").unwrap_or("");
    if !url.is_empty() {
        match is_http_url(url) {
            Ok(true) => {}
            Ok(false) => return Err(invalid("source_url must be empty or an http/https URL")),
            Err(_) => return Err(WordplayError::Server("Invalid IPv6 URL".into())),
        }
    }
    let member = |field: &str, choices: &[&str]| str_of(values, field).is_some_and(|value| choices.contains(&value));
    if !member("evidence_type", &EVIDENCE_TYPES) {
        return Err(invalid("invalid evidence_type"));
    }
    if !member("verification_status", &VERIFICATION_STATUSES) {
        return Err(invalid("invalid verification_status"));
    }
    for field in ["source_section_position", "target_section_position"] {
        if !member(field, &SECTION_POSITIONS) {
            return Err(invalid(format!("invalid {field}")));
        }
    }
    if !member("source_cue_mode", &SOURCE_CUE_MODES) {
        return Err(invalid("invalid source_cue_mode"));
    }
    Ok(())
}

/// `_validate_cues`.
fn validate_cues(values: &BTreeMap<&str, PyValue>, from: &TrackRow, to: &TrackRow) -> Result<(), WordplayError> {
    let (from, to) = (facts(from), facts(to));
    for (field, track) in [
        ("from_timestamp", &from),
        ("source_cue_end_timestamp", &from),
        ("to_timestamp", &to),
        ("target_intro_timestamp", &to),
        ("target_landing_timestamp", &to),
    ] {
        if let Some(timestamp) = number(values.get(field)) {
            if !plumdeck_domain::wordplay::cue_in_range(Some(timestamp), Some(track)) {
                return Err(invalid(format!("{field} must be before the selected track duration")));
            }
        }
    }
    if let (Some(start), Some(end)) = (number(values.get("from_timestamp")), number(values.get("source_cue_end_timestamp"))) {
        if start >= end {
            return Err(invalid("source_cue_end_timestamp must be after from_timestamp"));
        }
    }
    if let (Some(intro), Some(landing)) = (number(values.get("target_intro_timestamp")), number(values.get("target_landing_timestamp"))) {
        if intro >= landing {
            return Err(invalid("target_intro_timestamp must be before target_landing_timestamp"));
        }
    }
    Ok(())
}

/// Python `==` between two field values (numbers by value).
fn same(a: &PyValue, b: &PyValue) -> bool {
    match (number(Some(a)), number(Some(b))) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// `_sync_target_landing_alias`.
fn sync_landing(values: &mut BTreeMap<&str, PyValue>, supplied: &[&str]) -> Result<(), WordplayError> {
    let has = |name: &str| supplied.contains(&name);
    let to = values.get("to_timestamp").cloned().unwrap_or(PyValue::None);
    let landing = values.get("target_landing_timestamp").cloned().unwrap_or(PyValue::None);
    if has("to_timestamp") && has("target_landing_timestamp") {
        if !same(&to, &landing) {
            return Err(invalid("to_timestamp and target_landing_timestamp must match"));
        }
    } else if has("to_timestamp") {
        values.insert("target_landing_timestamp", to);
    } else if has("target_landing_timestamp") {
        values.insert("to_timestamp", landing);
    } else if landing == PyValue::None {
        values.insert("target_landing_timestamp", to);
    }
    Ok(())
}

fn pair_values(pair: &PairRow) -> BTreeMap<&'static str, PyValue> {
    let f = |value: Option<f64>| value.map_or(PyValue::None, PyValue::Float);
    let s = |value: &str| PyValue::Str(value.to_owned());
    BTreeMap::from([
        ("from_track_id", PyValue::Int(pair.from_track_id.into())),
        ("to_track_id", PyValue::Int(pair.to_track_id.into())),
        ("keyword", s(&pair.keyword)),
        ("source_phrase", s(&pair.source_phrase)),
        ("target_phrase", s(&pair.target_phrase)),
        ("source_section_position", s(&pair.source_section_position)),
        ("target_section_position", s(&pair.target_section_position)),
        ("source_cue_mode", s(&pair.source_cue_mode)),
        ("from_timestamp", f(pair.from_timestamp)),
        ("source_cue_end_timestamp", f(pair.source_cue_end_timestamp)),
        ("to_timestamp", f(pair.to_timestamp)),
        ("target_intro_timestamp", f(pair.target_intro_timestamp)),
        ("target_landing_timestamp", f(pair.target_landing_timestamp)),
        ("transition_notes", s(&pair.transition_notes)),
        ("source_url", s(&pair.source_url)),
        ("evidence_type", s(&pair.evidence_type)),
        ("verification_status", s(&pair.verification_status)),
    ])
}

/// Validated values into the row (every string field is a string by now).
fn apply(pair: &mut PairRow, values: &BTreeMap<&str, PyValue>) {
    let s = |key: &str| str_of(values, key).unwrap_or_default().to_owned();
    let id = |key: &str| match values.get(key) {
        Some(PyValue::Int(id)) => i64::try_from(*id).unwrap_or_default(),
        _ => 0,
    };
    pair.from_track_id = id("from_track_id");
    pair.to_track_id = id("to_track_id");
    pair.keyword = s("keyword");
    pair.source_phrase = s("source_phrase");
    pair.target_phrase = s("target_phrase");
    pair.source_section_position = s("source_section_position");
    pair.target_section_position = s("target_section_position");
    pair.source_cue_mode = s("source_cue_mode");
    pair.from_timestamp = number(values.get("from_timestamp"));
    pair.source_cue_end_timestamp = number(values.get("source_cue_end_timestamp"));
    pair.to_timestamp = number(values.get("to_timestamp"));
    pair.target_intro_timestamp = number(values.get("target_intro_timestamp"));
    pair.target_landing_timestamp = number(values.get("target_landing_timestamp"));
    pair.transition_notes = s("transition_notes");
    pair.source_url = s("source_url");
    pair.evidence_type = s("evidence_type");
    pair.verification_status = s("verification_status");
    pair.normalized_keyword = normalize_keyword(&pair.keyword);
}

fn strip_texts(values: &mut BTreeMap<&str, PyValue>) {
    for field in ["keyword", "source_phrase", "target_phrase", "transition_notes", "source_url"] {
        if let Some(PyValue::Str(text)) = values.get_mut(field) {
            *text = py_strip(text).to_owned();
        }
    }
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::DuckDb(inner) if inner.to_string().contains("onstraint"))
}

/// `update_pair`; `Ok(None)` when the pair does not exist.
pub fn update(conn: &Connection, id: BigInt, changes: &[(String, PyValue)], now: NaiveDateTime) -> Result<Option<PyValue>, WordplayError> {
    let supplied: Vec<&str> =
        changes.iter().filter_map(|(key, _)| ALLOWED.iter().find(|field| **field == key.as_str()).copied()).collect();
    if changes.is_empty() || supplied.len() != changes.len() {
        return Err(invalid("no valid fields to update"));
    }
    let Some(mut pair) = get_pair(conn, id)? else { return Ok(None) };
    let original = pair_values(&pair);
    let mut merged = original.clone();
    for (key, value) in changes {
        let field = ALLOWED.iter().find(|field| **field == key.as_str()).expect("allowed");
        merged.insert(field, value.clone());
    }
    sync_landing(&mut merged, &supplied)?;
    strip_texts(&mut merged);
    validate_fields(&merged)?;
    let (from, to) = validate_tracks(conn, &merged["from_track_id"], &merged["to_track_id"])?;
    validate_cues(&merged, &from, &to)?;
    let changed = TRANSITION_FIELDS.iter().any(|field| supplied.contains(field) && !same(&merged[field], &original[field]));
    if changed {
        merged.insert("verification_status", PyValue::Str("unverified".into()));
    }
    apply(&mut pair, &merged);
    if supplied != ["verification_status"] {
        pair.status = "pending".into();
    }
    pair.updated_at = Some(now);
    let stored = match update_pair(conn, &pair) {
        Ok(stored) => stored,
        Err(error) if is_unique_violation(&error) => {
            return Err(WordplayError::Conflict("wordplay pair conflicts with an existing proposal".into()));
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Some(item(conn, &stored)?.0))
}

/// `propose_pair` (the MCP tool's rules; no HTTP route, D-17).
pub fn propose(conn: &Connection, values: &[(String, PyValue)], now: NaiveDateTime) -> Result<PyValue, WordplayError> {
    let supplied: Vec<&str> = values.iter().filter_map(|(key, _)| ALLOWED.iter().find(|field| **field == key.as_str()).copied()).collect();
    let mut unexpected: Vec<&str> =
        values.iter().map(|(key, _)| key.as_str()).filter(|key| !ALLOWED.contains(key)).collect();
    if !unexpected.is_empty() {
        unexpected.sort_unstable();
        unexpected.dedup();
        return Err(invalid(format!("unexpected fields: {}", unexpected.join(", "))));
    }
    let mut merged: BTreeMap<&str, PyValue> = BTreeMap::new();
    for (key, value) in values {
        merged.insert(ALLOWED.iter().find(|field| **field == key.as_str()).expect("allowed"), value.clone());
    }
    for (field, default) in [
        ("transition_notes", ""),
        ("source_url", ""),
        ("source_section_position", "unknown"),
        ("target_section_position", "unknown"),
        ("source_cue_mode", "section_end"),
        ("evidence_type", "hypothesis"),
        ("verification_status", "unverified"),
    ] {
        merged.entry(field).or_insert_with(|| PyValue::Str(default.into()));
    }
    sync_landing(&mut merged, &supplied)?;
    strip_texts(&mut merged);
    validate_fields(&merged)?;
    let normalized = normalize_keyword(str_of(&merged, "keyword").unwrap_or_default());
    let none = PyValue::None;
    let (from, to) = validate_tracks(
        conn,
        merged.get("from_track_id").unwrap_or(&none),
        merged.get("to_track_id").unwrap_or(&none),
    )?;
    validate_cues(&merged, &from, &to)?;
    if let Some(existing) = find_pair(conn, from.id, to.id, &normalized)? {
        return Ok(item(conn, &existing)?.0);
    }
    let mut pair = PairRow {
        id: 0,
        from_track_id: from.id,
        to_track_id: to.id,
        keyword: String::new(),
        normalized_keyword: String::new(),
        source_phrase: String::new(),
        target_phrase: String::new(),
        source_section_position: String::new(),
        target_section_position: String::new(),
        source_cue_mode: String::new(),
        from_timestamp: None,
        source_cue_end_timestamp: None,
        to_timestamp: None,
        target_intro_timestamp: None,
        target_landing_timestamp: None,
        transition_notes: String::new(),
        source_url: String::new(),
        evidence_type: String::new(),
        verification_status: String::new(),
        status: "pending".into(),
        created_at: Some(now),
        updated_at: Some(now),
    };
    apply(&mut pair, &merged);
    match insert_pair(conn, &pair) {
        Ok(stored) => Ok(item(conn, &stored)?.0),
        Err(error) if is_unique_violation(&error) => Err(WordplayError::Conflict("wordplay pair conflicts with an existing proposal".into())),
        Err(error) => Err(error.into()),
    }
}

/// `approve_pair`; `Ok(None)` when the pair does not exist.
pub fn approve(conn: &Connection, id: BigInt, now: NaiveDateTime) -> Result<Option<PyValue>, WordplayError> {
    let Some(mut pair) = get_pair(conn, id)? else { return Ok(None) };
    if pair.status == "approved" {
        return Ok(Some(item(conn, &pair)?.0));
    }
    validate_tracks(conn, &PyValue::Int(pair.from_track_id.into()), &PyValue::Int(pair.to_track_id.into()))?;
    if !item(conn, &pair)?.1 {
        return Err(invalid(
            "approval requires a usable source cue, ordered target intro and landing timestamps, compatible musical \
             style, and actual BPM delta at or below 2%",
        ));
    }
    pair.status = "approved".into();
    pair.updated_at = Some(now);
    let stored = update_pair(conn, &pair)?;
    Ok(Some(item(conn, &stored)?.0))
}

/// `POST /api/wordplay-pairs/{id}/approve`.
pub async fn approve_route(State(state): State<AppState>, Path(pair_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = params.path_int("pair_id", &pair_id).map(BigInt);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state.blocking_result(move || db.write(|conn| Ok(approve(conn, id, now)))).await;
    answer(result)
}

fn answer(result: Result<Result<Result<Option<PyValue>, WordplayError>, DbError>, Response>) -> Response {
    match result {
        Ok(Ok(Ok(Some(value)))) => pydantic(value).into_response(),
        Ok(Ok(Ok(None))) => HttpError(ApiError::new(404, "Wordplay pair not found")).into_response(),
        Ok(Ok(Err(error))) => error.response(),
        Ok(Err(error)) => server_error(&error),
        Err(response) => response,
    }
}

/// `WordplayPairUpdate`: every field optional and strict, extra keys
/// forbidden. Holds the supplied fields (`exclude_unset`) in order.
struct UpdateBody {
    changes: Vec<(String, PyValue)>,
}

impl BodyModel for UpdateBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let mut changes = Vec::new();
        for name in fields.names() {
            let value = fields.value(name).expect("present").clone();
            if !ALLOWED.contains(&name) {
                fields.reject(name, "extra_forbidden", "Extra inputs are not permitted");
                continue;
            }
            let ok = match (name, &value) {
                (_, PyValue::None) => true,
                ("from_track_id" | "to_track_id", PyValue::Int(_)) => true,
                ("keyword" | "source_phrase" | "target_phrase", PyValue::Str(text)) => !py_strip(text).is_empty(),
                ("source_section_position" | "target_section_position", PyValue::Str(text)) => SECTION_POSITIONS.contains(&text.as_str()),
                ("source_cue_mode", PyValue::Str(text)) => SOURCE_CUE_MODES.contains(&text.as_str()),
                ("evidence_type", PyValue::Str(text)) => plumdeck_domain::wordplay::EVIDENCE_TYPES.contains(&text.as_str()),
                ("verification_status", PyValue::Str(text)) => VERIFICATION_STATUSES.contains(&text.as_str()),
                ("transition_notes", PyValue::Str(_)) => true,
                ("source_url", PyValue::Str(text)) => {
                    py_strip(text).is_empty() || is_http_url(py_strip(text)).unwrap_or(false)
                }
                (field, PyValue::Int(number)) if TIMESTAMPS.contains(&field) => *number >= 0,
                (field, PyValue::Float(number)) if TIMESTAMPS.contains(&field) => number.is_finite() && *number >= 0.0,
                _ => false,
            };
            if ok {
                changes.push((name.to_owned(), value));
            } else {
                fields.reject(name, "value_error", "Value error");
            }
        }
        fields.finish(|| Some(Self { changes }))
    }
}

/// `PATCH /api/wordplay-pairs/{id}`.
pub async fn patch_route(State(state): State<AppState>, Path(pair_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = match parse_body_for::<UpdateBody>(&headers, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = params.path_int("pair_id", &pair_id).map(BigInt);
    let update_body = match parsed {
        Ok(update_body) => Some(update_body),
        Err(errors) => {
            params.extend(errors);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, changes) = (id.expect("validated"), update_body.expect("validated").changes);
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state.blocking_result(move || db.write(|conn| Ok(update(conn, id, &changes, now)))).await;
    answer(result)
}

/// `DELETE /api/wordplay-pairs/{id}`.
pub async fn delete_route(State(state): State<AppState>, Path(pair_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = params.path_int("pair_id", &pair_id).map(BigInt);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| delete_pair(conn, id))).await {
        Ok(true) => pydantic(dict([("ok", boolean(true))])).into_response(),
        Ok(false) => HttpError(ApiError::new(404, "Wordplay pair not found")).into_response(),
        Err(response) => response,
    }
}
