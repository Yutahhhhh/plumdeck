//! Assist mode (backend/api/routers/assist.py over
//! app/services/assist_app_service.py and assist_bridge.py; S6): deck
//! resolution, preset recommendations with alternatives and the DJ's
//! history, routes to a destination, artists, search and the agent bridge.
//!
//! Nothing here writes to either library. rekordbox is opened read-only per
//! request ([`Rekordbox`]); the "like X" references and agent-picked tracks
//! are MCP-only and come with S9.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyfmt::format_float;
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::{casefold, py_split};
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::recommendations::{candidate_pool, embedding, PoolQuery, PoolRow};
use plumdeck_db::repos::tracks::{get_track, BigInt, TrackRow};
use plumdeck_db::repos::wordplay::approved_edges;
use plumdeck_db::DbError;
use plumdeck_domain::assist::{
    self, evaluate, history_fits, normalize_text, Evidence, Facts, Filters, Likeness, PairFacts, PlayHistory, RawFilters, Scored,
    TransitionFit,
};
use plumdeck_domain::mixability::{cosine_similarity, embedding_space, parse_embedding};
use plumdeck_domain::targets::Targets;
use plumdeck_rekordbox::paths::RegisteredPaths;
use plumdeck_rekordbox::MasterDb;
use serde_json::{json, Map, Value};

use crate::models::track_dump;
use crate::response::PyJson;
use crate::routes::setlists::pool_track;
use crate::{server_error, AppState, HttpError};

const MAX_CANDIDATE_POOL: i64 = 400;
const MAX_SUGGESTIONS: usize = 50;
const MAX_ROUTE_INTERMEDIATE: i64 = 4;
const MAX_ROUTE_BEAM: usize = 18;
const MAX_ROUTE_POOL: i64 = 120;
const ALTERNATIVE_MIN_SCORE: f64 = 0.6;
const MAX_HISTORY_RELATED: usize = 200;

fn g(x: f64) -> String {
    format_float(x, "g").unwrap_or_else(|_| x.to_string())
}

fn py_json(value: &PyValue) -> Value {
    match value {
        PyValue::None => Value::Null,
        PyValue::Bool(b) => json!(b),
        PyValue::Int(n) => json!(*n as i64),
        PyValue::Float(x) => json!(x),
        PyValue::Str(s) => json!(s),
        PyValue::List(items) => Value::Array(items.iter().map(py_json).collect()),
        PyValue::Dict(pairs) => Value::Object(
            pairs.iter().map(|(k, v)| (match k { PyValue::Str(s) => s.clone(), other => format!("{other:?}") }, py_json(v))).collect(),
        ),
        PyValue::DateTime(at) => json!(plumdeck_core::time::py_isoformat(at)),
        PyValue::Date(d) => json!(plumdeck_core::time::py_date_isoformat(d)),
    }
}

fn answer(value: Value) -> Response {
    // json.dumps after jsonable_encoder; serde_json writes the same values.
    let text = serde_json::to_string(&value).unwrap_or_default();
    match plumdeck_core::compat::pyjson_loads::loads(&text) {
        Ok(parsed) => PyJson(parsed).into_response(),
        Err(_) => server_error(&"assist answer"),
    }
}

/// The rekordbox library as Assist reads it, or why it cannot.
struct Rekordbox {
    db: Option<MasterDb>,
    error: Option<String>,
}

impl Rekordbox {
    fn open() -> Self {
        let path = plumdeck_rekordbox::locate::default_master_db_path().filter(|path| path.is_file());
        let Some(path) = path else { return Self { db: None, error: Some("rekordbox のライブラリ (master.db) が見つかりません".into()) } };
        match MasterDb::open(&path) {
            Ok(db) => Self { db: Some(db), error: None },
            Err(_) => Self { db: None, error: Some("rekordbox のライブラリを読み取れませんでした".into()) },
        }
    }

    fn registered(&self) -> Result<RegisteredPaths, String> {
        match &self.db {
            Some(db) => db.registered_paths().map_err(|_| "rekordbox のライブラリを読み取れませんでした".into()),
            None => Err(self.error.clone().unwrap_or_default()),
        }
    }
}

/// `_recording_identity`.
fn identity(title: Option<&str>, artist: Option<&str>) -> Option<(String, String)> {
    let (title, artist) = (normalize_text(title), normalize_text(artist));
    (!title.is_empty() && !artist.is_empty()).then_some((title, artist))
}

fn exists(path: &str) -> bool {
    !path.is_empty() && !path.contains('\0') && std::path::Path::new(path).exists()
}

fn available(filepath: &str, registered: &RegisteredPaths) -> bool {
    !filepath.is_empty() && registered.contains(filepath) && exists(filepath)
}

/// A track payload (`_track_payload`): the model dump plus `has_lyrics`.
#[derive(Clone, Debug)]
struct Payload {
    dump: Map<String, Value>,
    facts: Facts,
}

impl Payload {
    fn from_dump(pairs: Vec<(&'static str, PyValue)>, has_lyrics: bool) -> Self {
        let mut dump = Map::new();
        for (key, value) in &pairs {
            dump.insert((*key).to_owned(), py_json(value));
        }
        dump.insert("has_lyrics".into(), json!(has_lyrics));
        let number = |key: &str| dump.get(key).and_then(Value::as_f64);
        let text = |key: &str| dump.get(key).and_then(Value::as_str).map(str::to_owned);
        let facts = Facts {
            id: dump.get("id").and_then(Value::as_i64).unwrap_or(0),
            title: text("title"),
            artist: text("artist"),
            genre: text("genre"),
            subgenre: text("subgenre"),
            bpm: number("bpm"),
            key: text("key"),
            year: number("year"),
            energy: number("energy"),
            danceability: number("danceability"),
            brightness: number("brightness"),
            noisiness: number("noisiness"),
        };
        Self { dump, facts }
    }

    fn of_track(track: &TrackRow, has_lyrics: bool) -> Self {
        Self::from_dump(track_dump(track), has_lyrics)
    }

    fn with_bpm(mut self, bpm: f64) -> Self {
        self.dump.insert("bpm".into(), json!(bpm));
        self.facts.bpm = Some(bpm);
        self
    }

    fn value(&self) -> Value {
        Value::Object(self.dump.clone())
    }
}

/// A pool candidate: the pool row, its payload and its vector.
#[derive(Clone, Debug)]
struct Candidate {
    row: PoolRow,
    payload: Payload,
}

fn candidates(rows: Vec<PoolRow>, now: chrono::NaiveDateTime) -> Vec<Candidate> {
    rows.into_iter().map(|row| Candidate { payload: Payload::from_dump(pool_track(&row, now), row.has_lyrics), row }).collect()
}

/// Approved wordplay edges out of the source, keyed by target.
fn approved_pairs(conn: &Connection, source: i64) -> Result<BTreeMap<i64, (PairFacts, Value)>, DbError> {
    let mut map = BTreeMap::new();
    for pair in approved_edges(conn, Some(source))? {
        let facts = PairFacts {
            keyword: pair.keyword.clone(),
            source_phrase: pair.source_phrase.clone(),
            target_phrase: pair.target_phrase.clone(),
            tested: pair.verification_status == "tested",
        };
        let value = json!({
            "pair_id": pair.id, "keyword": pair.keyword, "source_phrase": pair.source_phrase,
            "target_phrase": pair.target_phrase, "source_cue_mode": pair.source_cue_mode,
            "from_timestamp": pair.from_timestamp, "source_cue_end_timestamp": pair.source_cue_end_timestamp,
            "target_intro_timestamp": pair.target_intro_timestamp,
            "target_landing_timestamp": pair.target_landing_timestamp.or(pair.to_timestamp),
            "transition_notes": pair.transition_notes, "evidence_type": pair.evidence_type,
            "verification_status": pair.verification_status,
        });
        map.entry(pair.to_track_id).or_insert((facts, value));
    }
    Ok(map)
}

/// Lightweight rows for scope and filters (`_metadata`).
#[derive(Clone, Debug)]
struct Meta {
    facts: Facts,
    album: Option<String>,
    filepath: String,
}

fn metadata(conn: &Connection) -> Result<Vec<Meta>, DbError> {
    let mut statement = conn.prepare("SELECT tracks.id, tracks.title, tracks.artist, tracks.album, tracks.genre, tracks.subgenre, tracks.bpm, tracks.key, tracks.year, tracks.filepath FROM tracks")?;
    let rows = statement
        .query_map([], |row| {
            Ok(Meta {
                facts: Facts {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    genre: row.get(4)?,
                    subgenre: row.get(5)?,
                    bpm: row.get::<_, Option<f32>>(6)?.map(f64::from),
                    key: row.get(7)?,
                    year: row.get::<_, Option<i64>>(8)?.map(|y| y as f64),
                    ..Facts::default()
                },
                album: row.get(3)?,
                filepath: row.get(9)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn lyrics_row_exists(conn: &Connection, id: i64) -> Result<bool, DbError> {
    Ok(plumdeck_db::repos::lyrics::get_lyrics(conn, BigInt(id.into()))?.is_some())
}

fn excluded_with_duplicates(rows: &[Meta], source: Option<i64>, exclude: &[i64]) -> BTreeSet<i64> {
    let mut excluded: BTreeSet<i64> = exclude.iter().copied().collect();
    if let Some(source) = source {
        excluded.insert(source);
    }
    if excluded.is_empty() {
        return excluded;
    }
    let identities: BTreeSet<(String, String)> = rows
        .iter()
        .filter(|row| excluded.contains(&row.facts.id))
        .filter_map(|row| identity(row.facts.title.as_deref(), row.facts.artist.as_deref()))
        .collect();
    for row in rows {
        if identity(row.facts.title.as_deref(), row.facts.artist.as_deref()).is_some_and(|id| identities.contains(&id)) {
            excluded.insert(row.facts.id);
        }
    }
    excluded
}

/// The scoring context of one source.
struct Context {
    source: Facts,
    model: Option<String>,
    vector: Option<Vec<f64>>,
    pairs: BTreeMap<i64, (PairFacts, Value)>,
    transition: bool,
    history: Option<Arc<PlayHistory>>,
    history_source: Option<(String, String)>,
    tolerance: Option<f64>,
}

struct Entry {
    payload: Payload,
    result: Scored,
    history: Option<Evidence>,
    history_fits: bool,
}

fn evaluate_candidate(context: &Context, candidate: &Candidate, intent: &str, fit: Option<&TransitionFit>, use_pairs: bool) -> Scored {
    let similarity = match (&context.vector, &candidate.row.vector) {
        (Some(source), Some(vector)) => cosine_similarity(Some(source.as_slice()), Some(vector.as_slice()), context.model.as_deref(), candidate.row.embedding_model.as_deref()),
        _ => None,
    };
    let pair = if use_pairs { context.pairs.get(&candidate.row.id).map(|(facts, _)| facts) } else { None };
    evaluate(
        &context.source,
        &candidate.payload.facts,
        intent,
        similarity,
        pair,
        context.vector.is_some() && candidate.row.vector.is_some(),
        fit,
        context.tolerance,
    )
}

/// `_score_pool`: (entries, unavailable, filtered).
fn score_pool(pool: &[Candidate], intent: &str, registered: &RegisteredPaths, context: &Context) -> (Vec<Entry>, usize, usize) {
    let (mut entries, mut unavailable, mut filtered) = (Vec::new(), 0, 0);
    for candidate in pool {
        if !available(&candidate.row.filepath, registered) {
            unavailable += 1;
            continue;
        }
        let payload = &candidate.payload.facts;
        let mut fit = None;
        if context.transition {
            fit = assist::transition_fit(context.source.bpm, payload);
            if fit.is_none() || !assist::passes_transition_filter(intent, &context.source, payload) {
                filtered += 1;
                continue;
            }
        } else if !assist::passes_intent_filter(intent, &context.source, payload, context.tolerance) {
            filtered += 1;
            continue;
        }
        let result = evaluate_candidate(context, candidate, intent, fit.as_ref(), true);
        let evidence = context.history.as_ref().and_then(|history| {
            history.evidence(context.history_source.as_ref(), identity(payload.title.as_deref(), payload.artist.as_deref()).as_ref())
        });
        let fits = evidence.is_some() && history_fits(&context.source, payload, context.transition);
        entries.push(Entry { payload: candidate.payload.clone(), result, history: evidence, history_fits: fits });
    }
    (entries, unavailable, filtered)
}

fn order_key(intent: &str, entry: &Entry) -> f64 {
    let score = entry.result.score;
    match (&entry.history, entry.history_fits) {
        (Some(evidence), true) => score + assist::history_weight(intent) * evidence.affinity,
        _ => score,
    }
}

/// `_rank`: by key, then id; one entry per recording.
fn rank(mut entries: Vec<Entry>, key: impl Fn(&Entry) -> f64) -> Vec<Entry> {
    entries.sort_by(|a, b| {
        (-key(a)).partial_cmp(&-key(b)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.payload.facts.id.cmp(&b.payload.facts.id))
    });
    let mut seen = BTreeSet::new();
    entries
        .into_iter()
        .filter(|entry| match identity(entry.payload.facts.title.as_deref(), entry.payload.facts.artist.as_deref()) {
            Some(id) => seen.insert(id),
            None => true,
        })
        .collect()
}

fn round4(x: f64) -> f64 {
    plumdeck_core::compat::pyfmt::round_ndigits(x, 4)
}

fn reasons_json(scored: &Scored) -> Vec<Value> {
    scored.reasons.iter().map(|r| json!({"kind": r.kind, "tone": r.tone, "text": r.text})).collect()
}

fn evidence_json(evidence: &Evidence) -> Value {
    json!({
        "followed": evidence.followed,
        "nearby": evidence.nearby,
        "plays": evidence.plays,
        "days_since": evidence.days_since.map(|d| plumdeck_core::compat::pyfmt::round_ndigits(d, 1)),
        "affinity": round4(evidence.affinity),
    })
}

/// `_entry_payload`.
fn entry_payload(entry: &Entry, pairs: &BTreeMap<i64, (PairFacts, Value)>) -> Value {
    let mut summary = entry.result.summary.clone();
    let mut reasons = reasons_json(&entry.result);
    if let Some(evidence) = &entry.history {
        if evidence.related() {
            summary = format!("{}／{summary}", assist::history_headline(evidence));
        }
        let mut text = assist::describe_history(evidence);
        if evidence.related() && !entry.history_fits {
            text.push_str("。テンポかエネルギーが離れているため、順位には反映していません");
        }
        reasons.insert(0, json!({"kind": "history", "tone": if evidence.related() { "good" } else { "neutral" }, "text": text}));
    }
    let mut value = entry.payload.dump.clone();
    value.insert("score".into(), json!(round4(entry.result.score)));
    value.insert("components".into(), Value::Object(entry.result.components.iter().map(|(k, v)| (k.clone(), json!(round4(*v)))).collect()));
    value.insert("like".into(), Value::Null);
    value.insert("history".into(), entry.history.as_ref().map_or(Value::Null, evidence_json));
    value.insert("summary".into(), json!(summary));
    value.insert("strengths".into(), json!(entry.result.strengths));
    value.insert("reasons".into(), Value::Array(reasons));
    value.insert("wordplay".into(), pairs.get(&entry.payload.facts.id).map_or(Value::Null, |(_, v)| v.clone()));
    Value::Object(value)
}

fn input_error(detail: String) -> Response {
    HttpError(ApiError::new(422, detail)).into_response()
}

fn not_found(detail: &str) -> Response {
    HttpError(ApiError::new(404, detail)).into_response()
}

// ------------------------------------------------------------------ requests

fn get<'a>(pairs: &'a [(PyValue, PyValue)], key: &str) -> Option<&'a PyValue> {
    pairs.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v)
}

/// A loose reader for the request models: `Err` is a 422 (shape compared only).
struct Body<'a>(&'a [(PyValue, PyValue)]);

impl Body<'_> {
    fn check_keys(&self, allowed: &[&str]) -> Result<(), ()> {
        self.0.iter().all(|(k, _)| matches!(k, PyValue::Str(n) if allowed.contains(&n.as_str()))).then_some(()).ok_or(())
    }

    fn int(&self, key: &str, default: Option<i128>, min: i128, max: i128) -> Result<Option<i128>, ()> {
        match get(self.0, key) {
            None => Ok(default),
            Some(PyValue::None) if default.is_none() => Ok(None),
            Some(value) => {
                let n = crate::lax::to_int(value).ok_or(())?;
                (min..=max).contains(&n).then_some(Some(n)).ok_or(())
            }
        }
    }

    fn float(&self, key: &str, gt: f64, le: f64) -> Result<Option<f64>, ()> {
        match get(self.0, key) {
            None | Some(PyValue::None) => Ok(None),
            Some(value) => {
                let x = crate::lax::to_float(value).ok_or(())?;
                (x > gt && x <= le).then_some(Some(x)).ok_or(())
            }
        }
    }

    fn boolean(&self, key: &str) -> Result<bool, ()> {
        match get(self.0, key) {
            None => Ok(false),
            Some(value) => crate::lax::to_bool(value).ok_or(()),
        }
    }

    fn choice(&self, key: &str, default: &str, choices: &[&str]) -> Result<String, ()> {
        match get(self.0, key) {
            None => Ok(default.into()),
            Some(PyValue::Str(s)) if choices.contains(&s.as_str()) => Ok(s.clone()),
            _ => Err(()),
        }
    }

    fn strings(&self, key: &str, max: usize) -> Result<Vec<String>, ()> {
        match get(self.0, key) {
            None => Ok(Vec::new()),
            Some(PyValue::List(items)) if items.len() <= max => items.iter().map(|i| match i {
                PyValue::Str(s) => Ok(s.clone()),
                _ => Err(()),
            }).collect(),
            _ => Err(()),
        }
    }

    fn ints(&self, key: &str, max: usize) -> Result<Vec<i64>, ()> {
        match get(self.0, key) {
            None => Ok(Vec::new()),
            Some(PyValue::List(items)) if items.len() <= max => {
                items.iter().map(|i| crate::lax::to_int(i).and_then(|n| i64::try_from(n).ok()).ok_or(())).collect()
            }
            _ => Err(()),
        }
    }

    fn filters(&self) -> Result<RawFilters, ()> {
        let pairs = match get(self.0, "filters") {
            None => return Ok(RawFilters::default()),
            Some(PyValue::Dict(pairs)) => pairs,
            _ => return Err(()),
        };
        let body = Body(pairs);
        body.check_keys(&["bpm_min", "bpm_max", "genres", "subgenres", "artists", "keys", "year_min", "year_max", "query"])?;
        let query = match get(pairs, "query") {
            None => String::new(),
            Some(PyValue::Str(s)) if s.chars().count() <= 200 => s.clone(),
            _ => return Err(()),
        };
        Ok(RawFilters {
            bpm_min: body.float("bpm_min", 0.0, 400.0)?,
            bpm_max: body.float("bpm_max", 0.0, 400.0)?,
            genres: body.strings("genres", 32)?,
            subgenres: body.strings("subgenres", 32)?,
            artists: body.strings("artists", 32)?,
            keys: body.strings("keys", 24)?,
            year_min: body.int("year_min", None, 1900, 2100)?.map(|y| y as i64),
            year_max: body.int("year_max", None, 1900, 2100)?.map(|y| y as i64),
            query,
        })
    }
}

fn parse<T>(headers: &HeaderMap, bytes: &[u8], read: impl FnOnce(&Body<'_>) -> Result<T, ()>) -> Result<T, Response> {
    let body = crate::body::read_body(headers, bytes)?;
    let invalid = |input: PyValue| {
        crate::body::validation_response(&[crate::body::ValidationError::new("value_error", vec![crate::body::Loc::from("body")], "Invalid request", input)])
    };
    match &body {
        Some(PyValue::Dict(pairs)) => read(&Body(pairs)).map_err(|_| invalid(body.clone().unwrap_or(PyValue::None))),
        other => Err(invalid(other.clone().unwrap_or(PyValue::None))),
    }
}

const INTENTS: [&str; 9] = ["keep", "hype", "dance", "calm", "emotional", "bright", "throwback", "wordplay", "groove"];
const SCOPES: [&str; 3] = ["any", "same_genre", "same_subgenre"];

// ------------------------------------------------------------------ routes

/// `GET /api/assist/library-status`.
pub async fn library_status() -> Response {
    let result = tokio::task::spawn_blocking(|| Rekordbox::open().registered()).await;
    match result {
        Ok(Ok(paths)) => answer(json!({"available": true, "message": null, "registered_tracks": paths.len()})),
        Ok(Err(message)) => answer(json!({"available": false, "message": message, "registered_tracks": 0})),
        Err(error) => server_error(&error),
    }
}

/// `GET /api/assist/artists`.
pub async fn artists(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    let rows = state
        .blocking(move || {
            let conn = db.lease()?;
            let mut statement = conn.prepare("SELECT tracks.artist, count(tracks.id) FROM tracks GROUP BY tracks.artist")?;
            let rows: Vec<(Option<String>, i64)> = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut merged: Vec<(String, String, i64, i64)> = Vec::new();
    for (artist, count) in rows {
        let name = py_split(artist.as_deref().unwrap_or("")).join(" ");
        let key = normalize_text(Some(&name));
        if key.is_empty() {
            continue;
        }
        match merged.iter_mut().find(|(k, ..)| *k == key) {
            Some(entry) => {
                entry.2 += count;
                if count > entry.3 || (count == entry.3 && name < entry.1) {
                    entry.1 = name;
                    entry.3 = count;
                }
            }
            None => merged.push((key, name, count, count)),
        }
    }
    merged.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| casefold(&a.1).cmp(&casefold(&b.1))));
    answer(Value::Array(merged.into_iter().map(|(_, name, tracks, _)| json!({"name": name, "tracks": tracks})).collect()))
}

/// The agent bridge (`assist_bridge`): window reports, agent settings and lists.
#[derive(Clone, Default)]
pub struct AssistBridge {
    inner: Arc<Mutex<BridgeState>>,
}

#[derive(Default)]
struct BridgeState {
    revision: i64,
    window: Option<Value>,
    window_seen_at: Option<f64>,
    settings: Option<Value>,
    list: Option<Value>,
}

impl AssistBridge {
    pub fn report_window(&self, state: Value, now: f64) {
        let mut inner = self.inner.lock().expect("bridge");
        inner.window = Some(state);
        inner.window_seen_at = Some(now);
    }

    pub fn pending(&self) -> Value {
        let inner = self.inner.lock().expect("bridge");
        json!({"revision": inner.revision, "settings": inner.settings, "list": inner.list})
    }

    /// `window()`: whether the window reported in the last 30 seconds.
    pub fn window(&self, now: f64) -> Value {
        let inner = self.inner.lock().expect("bridge");
        let seen = inner.window_seen_at;
        json!({
            "open": seen.is_some_and(|seen| now - seen <= 30.0),
            "reported_seconds_ago": seen.map(|seen| plumdeck_core::compat::pyfmt::round_ndigits(now - seen, 1)),
            "state": inner.window.clone(),
            "agent_list": inner.list.clone(),
        })
    }

    pub fn apply_settings(&self, settings: Map<String, Value>, agent_name: Option<String>, now: f64) -> Value {
        let mut inner = self.inner.lock().expect("bridge");
        inner.revision += 1;
        let mut applied: Map<String, Value> = settings.into_iter().filter(|(_, v)| !v.is_null()).collect();
        applied.insert("revision".into(), json!(inner.revision));
        applied.insert("agent_name".into(), json!(agent_name));
        applied.insert("created_at".into(), json!(now));
        inner.settings = Some(Value::Object(applied.clone()));
        Value::Object(applied)
    }

    pub fn show_list(&self, title: &str, tracks: Vec<Value>, rejected: Vec<Value>, note: Option<&str>, agent_name: Option<String>, now: f64) -> Value {
        let mut inner = self.inner.lock().expect("bridge");
        inner.revision += 1;
        let cut = |text: &str, max: usize| plumdeck_core::compat::text::py_strip(text).chars().take(max).collect::<String>();
        let title = cut(title, 120);
        let note = note.map(|n| cut(n, 1000)).filter(|n| !n.is_empty());
        let list = json!({
            "revision": inner.revision,
            "title": if title.is_empty() { "エージェントの提案".to_owned() } else { title },
            "note": note, "tracks": tracks, "rejected": rejected, "agent_name": agent_name, "created_at": now,
        });
        inner.list = Some(list.clone());
        list
    }

    pub fn dismiss_list(&self) {
        let mut inner = self.inner.lock().expect("bridge");
        if inner.list.is_some() {
            inner.list = None;
            inner.revision += 1;
        }
    }
}

/// `PUT /api/assist/window`.
pub async fn report_window(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&headers, &body, |b| {
        b.check_keys(&[
            "deck_slot", "source_track_id", "destination_track_id", "intent", "transition", "genre_scope", "limit", "filters",
            "tempo_tolerance", "exclude_track_ids", "shown_track_ids",
        ])?;
        let filters = b.filters()?;
        Ok(json!({
            "deck_slot": b.int("deck_slot", None, 1, 4)?.map(|v| v as i64),
            "source_track_id": b.int("source_track_id", None, i64::MIN.into(), i64::MAX.into())?.map(|v| v as i64),
            "destination_track_id": b.int("destination_track_id", None, i64::MIN.into(), i64::MAX.into())?.map(|v| v as i64),
            "intent": b.choice("intent", "keep", &INTENTS)?,
            "transition": b.boolean("transition")?,
            "genre_scope": b.choice("genre_scope", "any", &SCOPES)?,
            "limit": b.int("limit", Some(12), 1, 50)?.map(|v| v as i64),
            "filters": {
                "bpm_min": filters.bpm_min, "bpm_max": filters.bpm_max, "genres": filters.genres, "subgenres": filters.subgenres,
                "artists": filters.artists, "keys": filters.keys, "year_min": filters.year_min, "year_max": filters.year_max,
                "query": filters.query,
            },
            "tempo_tolerance": b.float("tempo_tolerance", 0.0, 12.0)?,
            "exclude_track_ids": b.ints("exclude_track_ids", 10_000)?,
            "shown_track_ids": b.ints("shown_track_ids", 100)?,
        }))
    });
    match parsed {
        Ok(window) => {
            state.assist.report_window(window, state.clock.epoch_seconds());
            answer(json!({"ok": true}))
        }
        Err(response) => response,
    }
}

/// `GET /api/assist/agent`.
pub async fn agent_updates(State(state): State<AppState>) -> Response {
    answer(state.assist.pending())
}

/// `DELETE /api/assist/agent/list`.
pub async fn dismiss_agent_list(State(state): State<AppState>) -> Response {
    state.assist.dismiss_list();
    answer(json!({"dismissed": true}))
}

// ------------------------------------------------------------------ decks

/// `POST /api/assist/decks/resolve`.
pub async fn resolve_decks(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    struct Observation {
        slot: i64,
        loaded: bool,
        title: Option<String>,
        artist: Option<String>,
        track_bpm: Option<f64>,
        tempo_bpm: Option<f64>,
        display_key: Option<String>,
    }
    let parsed = parse(&headers, &body, |b| {
        b.check_keys(&["decks", "open_audio_paths"])?;
        let decks = match get(b.0, "decks") {
            None => Vec::new(),
            Some(PyValue::List(items)) if items.len() <= 4 => items
                .iter()
                .map(|item| {
                    let PyValue::Dict(pairs) = item else { return Err(()) };
                    let d = Body(pairs);
                    d.check_keys(&["slot", "loaded", "title", "artist", "track_bpm", "tempo_bpm", "display_key"])?;
                    let text = |key: &str, max: usize| match get(pairs, key) {
                        None | Some(PyValue::None) => Ok(None),
                        Some(PyValue::Str(s)) if s.chars().count() <= max => Ok(Some(s.clone())),
                        _ => Err(()),
                    };
                    let number = |key: &str| match get(pairs, key) {
                        None | Some(PyValue::None) => Ok(None),
                        Some(value) => crate::lax::to_float(value).map(Some).ok_or(()),
                    };
                    Ok(Observation {
                        slot: d.int("slot", None, 1, 4)?.ok_or(())? as i64,
                        loaded: d.boolean("loaded")?,
                        title: text("title", 512)?,
                        artist: text("artist", 512)?,
                        track_bpm: number("track_bpm")?,
                        tempo_bpm: number("tempo_bpm")?,
                        display_key: text("display_key", 32)?,
                    })
                })
                .collect::<Result<Vec<_>, ()>>()?,
            _ => return Err(()),
        };
        let paths = b.strings("open_audio_paths", 256)?;
        Ok((decks, paths))
    });
    let (decks, paths) = match parsed {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let library = Rekordbox::open();
            let mut library_error = library.error.clone();
            let mut entries: Vec<plumdeck_rekordbox::masterdb::Content> = Vec::new();
            let mut identity_entries: Vec<plumdeck_rekordbox::masterdb::Content> = Vec::new();
            if let Some(rb) = &library.db {
                let by_path = rb.contents_by_paths(&paths);
                let identities: Vec<(Option<&str>, Option<&str>)> =
                    decks.iter().filter(|d| d.loaded).map(|d| (d.title.as_deref(), d.artist.as_deref())).collect();
                let by_identity = rb.contents_by_identities(&identities);
                match (by_path, by_identity) {
                    (Ok(by_path), Ok(by_identity)) => {
                        entries = by_path.into_values().collect();
                        identity_entries = by_identity;
                    }
                    _ => library_error = Some("rekordbox のライブラリを読み取れませんでした".into()),
                }
            }
            let mut all_paths: Vec<String> = Vec::new();
            for entry in identity_entries.iter().chain(&entries) {
                if let Some(path) = &entry.folder_path {
                    if !all_paths.contains(path) {
                        all_paths.push(path.clone());
                    }
                }
            }
            let tracks = tracks_by_filepath(&conn, &all_paths)?;
            let mut out = Vec::new();
            for deck in &decks {
                let base = json!({
                    "slot": deck.slot,
                    "observed": {"title": deck.title, "artist": deck.artist, "track_bpm": deck.track_bpm, "tempo_bpm": deck.tempo_bpm, "display_key": deck.display_key},
                    "track": null, "rekordbox_id": null, "filepath": null, "match_confidence": null, "candidates": [],
                });
                let mut deck_value = base.as_object().cloned().unwrap_or_default();
                let finish = |mut map: Map<String, Value>, status: &str, message: Value| {
                    map.insert("status".into(), json!(status));
                    map.insert("message".into(), message);
                    Value::Object(map)
                };
                if !deck.loaded || deck.title.as_deref().map(plumdeck_core::compat::text::py_strip).unwrap_or("").is_empty() {
                    out.push(finish(deck_value, "empty", json!("曲が読み込まれていません")));
                    continue;
                }
                if let Some(error) = &library_error {
                    out.push(finish(deck_value, "unresolved", json!(format!("{error}のため、曲を特定できません"))));
                    continue;
                }
                let (title, artist) = (normalize_text(deck.title.as_deref()), normalize_text(deck.artist.as_deref()));
                let matching = |rows: &[plumdeck_rekordbox::masterdb::Content]| -> Vec<plumdeck_rekordbox::masterdb::Content> {
                    rows.iter().filter(|e| normalize_text(e.title.as_deref()) == title && normalize_text(e.artist.as_deref()) == artist).cloned().collect()
                };
                let mut exact = matching(&entries);
                if exact.is_empty() {
                    exact = matching(&identity_entries);
                }
                if exact.is_empty() {
                    out.push(finish(deck_value, "unresolved", json!("デッキの曲に対応するファイルが rekordbox の開いているファイルの中に見つかりませんでした")));
                    continue;
                }
                let entry = exact
                    .iter()
                    .min_by_key(|e| {
                        let path = e.folder_path.clone().unwrap_or_default();
                        (!tracks.contains_key(&path), plumdeck_rekordbox::paths::normalize_path(&path), e.id.clone())
                    })
                    .expect("non-empty")
                    .clone();
                let path = entry.folder_path.clone().unwrap_or_default();
                deck_value.insert("rekordbox_id".into(), json!(entry.id));
                deck_value.insert("filepath".into(), json!(path));
                deck_value.insert("match_confidence".into(), json!("title_and_artist"));
                match tracks.get(&path) {
                    None => out.push(finish(deck_value, "not_in_library", json!("この曲は plumdeck のライブラリに未登録のため、提案できません"))),
                    Some(track) => {
                        let has = lyrics_row_exists(&conn, track.id)?;
                        deck_value.insert("track".into(), Payload::of_track(track, has).value());
                        out.push(finish(deck_value, "resolved", Value::Null));
                    }
                }
            }
            Ok(json!({"decks": out, "library_error": library_error, "inspected_paths": paths.len()}))
        })
        .await;
    match result {
        Ok(value) => answer(value),
        Err(response) => response,
    }
}

/// `_tracks_by_filepath`: each path's one library track (by its spellings).
fn tracks_by_filepath(conn: &Connection, paths: &[String]) -> Result<HashMap<String, TrackRow>, DbError> {
    let mut matches = HashMap::new();
    if paths.is_empty() {
        return Ok(matches);
    }
    let rows = plumdeck_db::repos::csv_io::all_tracks(conn)?;
    for path in paths {
        let candidates: Vec<&TrackRow> = rows.iter().filter(|t| plumdeck_rekordbox::paths::same_path(path, &t.filepath)).collect();
        if candidates.len() == 1 {
            matches.insert(path.clone(), candidates[0].clone());
        }
    }
    Ok(matches)
}

// ------------------------------------------------------------------ recommendation

fn history_from(library: &Rekordbox, conn: &Connection, now: chrono::NaiveDateTime) -> Result<(Option<Arc<PlayHistory>>, HashMap<(String, String), Vec<i64>>), DbError> {
    let rows = metadata(conn)?;
    let mut by_path: HashMap<String, (String, String)> = HashMap::new();
    let mut ids: HashMap<(String, String), Vec<i64>> = HashMap::new();
    for row in &rows {
        if let Some(id) = identity(row.facts.title.as_deref(), row.facts.artist.as_deref()) {
            by_path.insert(plumdeck_rekordbox::paths::normalize_path(&row.filepath), id.clone());
            ids.entry(id).or_default().push(row.facts.id);
        }
    }
    let Some(db) = &library.db else { return Ok((None, ids)) };
    let Ok(sessions) = db.play_sessions() else { return Ok((None, ids)) };
    if sessions.is_empty() {
        return Ok((None, ids));
    }
    let parse_time = |text: &Option<String>| {
        text.as_deref().and_then(|t| chrono::NaiveDateTime::parse_from_str(&t.trim().chars().take(19).collect::<String>(), "%Y-%m-%d %H:%M:%S").ok().or_else(|| {
            chrono::NaiveDate::parse_from_str(&t.trim().chars().take(10).collect::<String>(), "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).expect("midnight"))
        }))
    };
    let history = PlayHistory::new(
        sessions.iter().map(|s| (parse_time(&s.date_created), s.filepaths.iter().map(|p| by_path.get(&plumdeck_rekordbox::paths::normalize_path(p)).cloned()).collect())),
        now,
    );
    Ok((Some(Arc::new(history)), ids))
}

#[allow(clippy::too_many_arguments)]
fn candidate_pool_for(
    conn: &Connection,
    source: &TrackRow,
    intent: &str,
    excluded: &[i64],
    genres: Option<&[String]>,
    pairs: &BTreeMap<i64, (PairFacts, Value)>,
    eligible: &[i64],
    conditions: &Filters,
    transition: bool,
    source_bpm: Option<f64>,
    pool_limit: i64,
    tolerance: Option<f64>,
    now: chrono::NaiveDateTime,
) -> Result<Vec<Candidate>, DbError> {
    let pool = |targets: Targets, genres: Option<&[String]>, limit: i64, ids: &[i64]| -> Result<Vec<Candidate>, DbError> {
        Ok(candidates(candidate_pool(conn, &PoolQuery { targets, genres, subgenres: None, limit, exclude_ids: excluded, candidate_ids: Some(ids) })?, now))
    };
    if intent == "wordplay" {
        let ids: Vec<i64> = pairs.keys().copied().filter(|id| eligible.contains(id)).collect();
        return pool(Targets::default(), None, pairs.len() as i64, &ids);
    }
    if transition {
        let ids: Vec<i64> = eligible.iter().take(pool_limit as usize).copied().collect();
        return pool(Targets::default(), None, pool_limit, &ids);
    }
    let mut eligible: Vec<i64> = eligible.to_vec();
    if intent == "throwback" {
        if let Some(year) = source.year.filter(|y| *y != 0) {
            let mut statement = conn.prepare("SELECT tracks.id FROM tracks WHERE tracks.year > 0 AND tracks.year <= ?")?;
            let older: BTreeSet<i64> = statement.query_map([year - assist::THROWBACK_MIN_YEARS as i64], |r| r.get(0))?.collect::<Result<_, _>>()?;
            eligible.retain(|id| older.contains(id));
        }
    }
    let preset = assist::preset(intent);
    let mut targets = Targets::default();
    if let Some(tempo) = source_bpm.or(source.bpm).filter(|t| *t != 0.0) {
        let rule = assist::tempo_rule(intent, tolerance);
        targets.bpm = Some(tempo * (1.0 + rule.center / 100.0));
        if tolerance.is_some_and(|t| t != 0.0) {
            let center = 1.0 + rule.center / 100.0;
            targets.max_tempo_distance =
                Some([rule.low, rule.high].iter().map(|edge| ((1.0 + edge / 100.0) / center).log2().abs()).fold(f64::MIN, f64::max) + 1e-9);
        }
    }
    let source_facts = Payload::of_track(source, false).facts;
    for (feature, direction) in &preset.directions {
        let Some(value) = source_facts.feature(feature) else { continue };
        let step = assist::feature_step(feature);
        let offset = match direction {
            assist::Direction::Up => step,
            assist::Direction::Down => -step,
            _ => 0.0,
        };
        match *feature {
            "energy" if matches!(direction, assist::Direction::Up | assist::Direction::Down) => targets.energy = Some((value + offset).clamp(0.0, 1.0)),
            "danceability" => targets.danceability = Some(value + offset),
            "brightness" => targets.brightness = Some(value + offset),
            "noisiness" => targets.noisiness = Some(value + offset),
            _ => {}
        }
    }
    if !conditions.is_empty() {
        if eligible.len() as i64 <= MAX_CANDIDATE_POOL {
            targets = Targets::default();
        } else if conditions.bpm_min.is_some() || conditions.bpm_max.is_some() {
            targets.bpm = None;
        }
    }
    let mut result = pool(targets, genres, pool_limit, &eligible)?;
    let present: BTreeSet<i64> = result.iter().map(|c| c.row.id).collect();
    let missing: Vec<i64> = pairs.keys().copied().filter(|id| eligible.contains(id) && !present.contains(id) && !excluded.contains(id)).collect();
    if !missing.is_empty() {
        result.extend(pool(Targets::default(), None, missing.len() as i64, &missing)?);
    }
    Ok(result)
}

fn with_related(
    conn: &Connection,
    mut pool: Vec<Candidate>,
    context: &Context,
    eligible: &[i64],
    ids: &HashMap<(String, String), Vec<i64>>,
    now: chrono::NaiveDateTime,
) -> Result<Vec<Candidate>, DbError> {
    let (Some(history), Some(source)) = (&context.history, &context.history_source) else { return Ok(pool) };
    let present: BTreeSet<i64> = pool.iter().map(|c| c.row.id).collect();
    let mut missing = Vec::new();
    for identity in history.related(source) {
        missing.extend(ids.get(&identity).into_iter().flatten().filter(|id| eligible.contains(id) && !present.contains(id)));
        if missing.len() >= MAX_HISTORY_RELATED {
            break;
        }
    }
    if missing.is_empty() {
        return Ok(pool);
    }
    missing.truncate(MAX_HISTORY_RELATED);
    pool.extend(candidates(
        candidate_pool(conn, &PoolQuery { targets: Targets::default(), genres: None, subgenres: None, limit: missing.len() as i64, exclude_ids: &[], candidate_ids: Some(&missing) })?,
        now,
    ));
    Ok(pool)
}

struct RecommendRequest {
    source: i64,
    intent: String,
    transition: bool,
    limit: usize,
    exclude: Vec<i64>,
    scope: String,
    genres: Option<Vec<String>>,
    filters: RawFilters,
    tolerance: Option<f64>,
}

/// `POST /api/assist/recommendations`.
pub async fn recommendations(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&headers, &body, |b| {
        b.check_keys(&["source_track_id", "intent", "transition", "limit", "exclude_track_ids", "genre_scope", "genres", "filters", "tempo_tolerance"])?;
        let genres = match get(b.0, "genres") {
            None | Some(PyValue::None) => None,
            Some(_) => Some(b.strings("genres", 32)?),
        };
        Ok(RecommendRequest {
            source: b.int("source_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            intent: b.choice("intent", "keep", &INTENTS)?,
            transition: b.boolean("transition")?,
            limit: b.int("limit", Some(12), 1, 50)?.unwrap_or(12) as usize,
            exclude: b.ints("exclude_track_ids", 10_000)?,
            scope: b.choice("genre_scope", "any", &SCOPES)?,
            genres,
            filters: b.filters()?,
            tolerance: b.float("tempo_tolerance", 0.0, 12.0)?,
        })
    });
    let request = match parsed {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state.blocking(move || { let conn = db.lease()?; Ok(recommend(&conn, request, now)) }).await;
    match result {
        Ok(Ok(value)) => answer(value),
        Ok(Err(response)) | Err(response) => response,
    }
}

fn recommend(conn: &Connection, request: RecommendRequest, now: chrono::NaiveDateTime) -> Result<Value, Response> {
    let db_error = |error: DbError| server_error(&error);
    let intent = assist::resolve_intent(&request.intent).map_err(input_error)?;
    let transition = request.transition && intent != "wordplay";
    let conditions = Filters::from_raw(&request.filters).map_err(input_error)?;
    let source = get_track(conn, BigInt(request.source.into())).map_err(db_error)?.ok_or_else(|| not_found("Track not found"))?;
    let preset = assist::preset(intent);
    let caveats: Vec<&str> = if transition { assist::TRANSITION_CAVEATS.to_vec() } else { Vec::new() };
    let response = |candidates: Vec<Value>, notes: Vec<String>, unavailable: usize, alternatives: Vec<Value>| {
        json!({
            "intent": intent, "intent_label": preset.label, "transition": transition,
            "tempo_tolerance": if transition || intent == "wordplay" { None } else { request.tolerance },
            "source_track_id": request.source, "basis": assist::describe_weights(intent), "reference": null,
            "filters": conditions.describe(), "candidates": candidates, "alternatives": alternatives,
            "notes": notes, "caveats": caveats, "unavailable_originals": unavailable,
        })
    };
    let scope_field = match request.scope.as_str() {
        "same_genre" => Some("genre"),
        "same_subgenre" => Some("subgenre"),
        _ => None,
    };
    let field_of = |facts: &Facts, field: &str| if field == "genre" { facts.genre.clone() } else { facts.subgenre.clone() };
    let source_payload = Payload::of_track(&source, lyrics_row_exists(conn, source.id).map_err(db_error)?);
    let scope_value = scope_field.map(|f| normalize_text(field_of(&source_payload.facts, f).as_deref()));
    if let (Some(field), Some(value)) = (scope_field, &scope_value) {
        if value.is_empty() {
            let label = if field == "genre" { "ジャンル" } else { "サブジャンル" };
            return Ok(response(Vec::new(), vec![format!("元曲の{label}が未登録のため、同じ{label}の候補を提案できません")], 0, Vec::new()));
        }
    }
    let mut source_payload = source_payload;
    let mut notes: Vec<String> = Vec::new();
    if let Some((start, end, _)) = assist::parse_transition(source.title.as_deref()) {
        source_payload = source_payload.with_bpm(end);
        notes.push(format!("基準曲はタイトル上 {}→{} BPM のトランジション曲のため、出口の {} BPM を基準にしています", g(start), g(end), g(end)));
    }
    let source_bpm = source_payload.facts.bpm;
    if !source_bpm.is_some_and(|b| b.is_finite() && b > 0.0) && intent != "wordplay" {
        return Ok(response(Vec::new(), vec!["基準曲の BPM が未解析のため、つなぎを判定できません".into()], 0, Vec::new()));
    }
    if intent == "throwback" && !source.year.is_some_and(|y| y > 0) {
        return Ok(response(Vec::new(), vec!["基準曲のリリース年が未登録のため、「時代を戻す」は使えません".into()], 0, Vec::new()));
    }
    let rows = metadata(conn).map_err(db_error)?;
    let excluded_set = excluded_with_duplicates(&rows, Some(source.id), &request.exclude);
    let excluded: Vec<i64> = excluded_set.iter().copied().collect();
    let mut eligible: Vec<&Meta> = rows
        .iter()
        .filter(|row| !excluded_set.contains(&row.facts.id))
        .filter(|row| match (scope_field, &scope_value) {
            (Some(field), Some(value)) => normalize_text(field_of(&row.facts, field).as_deref()) == *value,
            _ => true,
        })
        .filter(|row| conditions.matches(&row.facts, row.album.as_deref()))
        .collect();
    let pairs = approved_pairs(conn, source.id).map_err(db_error)?;
    if intent == "wordplay" && pairs.is_empty() {
        return Ok(response(Vec::new(), vec!["この曲を起点とする承認済みワードプレイはまだありません。ワードプレイ画面で候補を承認すると出てきます".into()], 0, Vec::new()));
    }
    if !conditions.is_empty() && eligible.is_empty() {
        return Ok(response(Vec::new(), vec!["指定した条件に合う曲がライブラリにありません".into()], 0, Vec::new()));
    }
    if transition {
        let mut fitted: Vec<(&Meta, f64)> = eligible
            .iter()
            .filter_map(|row| assist::transition_fit(source_bpm, &row.facts).map(|fit| (*row, fit.score)))
            .collect();
        fitted.sort_by(|a, b| (-a.1).partial_cmp(&-b.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.facts.id.cmp(&b.0.facts.id)));
        eligible = fitted.into_iter().map(|(row, _)| row).collect();
        if eligible.is_empty() {
            let bpm = g(source_bpm.unwrap_or_default());
            return Ok(response(Vec::new(), vec![format!("基準 {bpm} BPM から入れるトランジション曲・倍テン・ハーフテンの曲が見つかりません")], 0, Vec::new()));
        }
    }
    let library = Rekordbox::open();
    let registered = match library.registered() {
        Ok(registered) => registered,
        Err(error) => return Ok(response(Vec::new(), vec![format!("{error}。登録済みの原本を確認できないため、提案を停止しました")], 0, Vec::new())),
    };
    let (history, identity_ids) = if intent != "wordplay" { history_from(&library, conn, now).map_err(db_error)? } else { (None, HashMap::new()) };
    let stored = embedding(conn, source.id).map_err(db_error)?;
    let context = Context {
        source: source_payload.facts.clone(),
        model: stored.as_ref().and_then(|(m, _)| m.clone()),
        vector: stored.as_ref().and_then(|(_, j)| parse_embedding(j.as_deref())),
        pairs,
        transition,
        history,
        history_source: identity(source.title.as_deref(), source.artist.as_deref()),
        tolerance: request.tolerance,
    };
    let eligible_ids: Vec<i64> = eligible.iter().map(|row| row.facts.id).collect();
    let pool = candidate_pool_for(
        conn, &source, intent, &excluded, request.genres.as_deref(), &context.pairs, &eligible_ids, &conditions, transition, source_bpm,
        MAX_CANDIDATE_POOL, request.tolerance, now,
    )
    .map_err(db_error)?;
    let pool = with_related(conn, pool, &context, &eligible_ids, &identity_ids, now).map_err(db_error)?;
    let (entries, unavailable, filtered) = score_pool(&pool, intent, &registered, &context);
    let had_entries = !entries.is_empty();
    let ranked: Vec<Entry> = rank(entries, |e| order_key(intent, e)).into_iter().take(request.limit.min(MAX_SUGGESTIONS)).collect();
    if unavailable > 0 {
        notes.push(format!("rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"));
    }
    if !had_entries && filtered > 0 && !transition && intent != "wordplay" {
        let tempo = assist::tempo_rule(intent, request.tolerance);
        let hint = if request.tolerance.is_some() {
            "テンポ幅を広げるか、テンポを大きく変えたいときは「トランジション」をオンにしてください"
        } else {
            "テンポを大きく変えたいときは「トランジション」をオンにしてください"
        };
        let signed = |x: f64| format_float(x, "+g").unwrap_or_default();
        notes.push(format!("「{}」のテンポ範囲（{}〜{}%）に合う曲がありません。{hint}", preset.label, signed(tempo.low), signed(tempo.high)));
    }
    let general = if intent == "throwback" && !transition {
        let ids: Vec<i64> = rows
            .iter()
            .filter(|row| !excluded_set.contains(&row.facts.id))
            .filter(|row| match (scope_field, &scope_value) {
                (Some(field), Some(value)) => normalize_text(field_of(&row.facts, field).as_deref()) == *value,
                _ => true,
            })
            .filter(|row| conditions.matches(&row.facts, row.album.as_deref()))
            .map(|row| row.facts.id)
            .collect();
        let general = candidate_pool_for(
            conn, &source, "keep", &excluded, request.genres.as_deref(), &context.pairs, &ids, &conditions, false, source_bpm,
            MAX_CANDIDATE_POOL, request.tolerance, now,
        )
        .map_err(db_error)?;
        with_related(conn, general, &context, &eligible_ids, &identity_ids, now).map_err(db_error)?
    } else {
        pool.clone()
    };
    let alternatives = alternatives_for(intent, &ranked, &general, &registered, &context);
    Ok(response(ranked.iter().map(|e| entry_payload(e, &context.pairs)).collect(), notes, unavailable, alternatives))
}

fn alternatives_for(intent: &str, ranked: &[Entry], pool: &[Candidate], registered: &RegisteredPaths, context: &Context) -> Vec<Value> {
    let mut taken: BTreeSet<i64> = ranked.iter().map(|e| e.payload.facts.id).collect();
    let mut taken_recordings: BTreeSet<Option<(String, String)>> =
        ranked.iter().map(|e| identity(e.payload.facts.title.as_deref(), e.payload.facts.artist.as_deref())).collect();
    let mut out = Vec::new();
    for name in assist::MOOD_PRESETS {
        if name == intent {
            continue;
        }
        if name == "throwback" && !context.source.year.is_some_and(|y| y > 0.0) {
            continue;
        }
        let (entries, _, _) = score_pool(pool, name, registered, context);
        for entry in rank(entries, |e| order_key(name, e)) {
            if entry.result.score < ALTERNATIVE_MIN_SCORE {
                continue;
            }
            let id = identity(entry.payload.facts.title.as_deref(), entry.payload.facts.artist.as_deref());
            if taken.contains(&entry.payload.facts.id) || (id.is_some() && taken_recordings.contains(&id)) {
                continue;
            }
            taken.insert(entry.payload.facts.id);
            taken_recordings.insert(id);
            out.push(json!({"intent": name, "label": format!("{}なら", assist::preset(name).label), "track": entry_payload(&entry, &context.pairs)}));
            break;
        }
    }
    out
}

// ------------------------------------------------------------------ routes

struct RouteRequest {
    source: i64,
    target: i64,
    intent: String,
    transition: bool,
    max_intermediate: i64,
    limit: usize,
    exclude: Vec<i64>,
    scope: String,
    filters: RawFilters,
    tolerance: Option<f64>,
}

/// `POST /api/assist/routes`.
pub async fn routes(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&headers, &body, |b| {
        b.check_keys(&["source_track_id", "target_track_id", "intent", "transition", "max_intermediate", "limit", "exclude_track_ids", "genre_scope", "filters", "tempo_tolerance"])?;
        Ok(RouteRequest {
            source: b.int("source_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            target: b.int("target_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            intent: b.choice("intent", "keep", &INTENTS)?,
            transition: b.boolean("transition")?,
            max_intermediate: b.int("max_intermediate", Some(2), 0, 4)?.unwrap_or(2) as i64,
            limit: b.int("limit", Some(3), 1, 5)?.unwrap_or(3) as usize,
            exclude: b.ints("exclude_track_ids", 10_000)?,
            scope: b.choice("genre_scope", "any", &SCOPES)?,
            filters: b.filters()?,
            tolerance: b.float("tempo_tolerance", 0.0, 12.0)?,
        })
    });
    let request = match parsed {
        Ok(request) => request,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state.blocking(move || { let conn = db.lease()?; Ok(route(&conn, request, now)) }).await;
    match result {
        Ok(Ok(value)) => answer(value),
        Ok(Err(response)) | Err(response) => response,
    }
}

#[allow(dead_code)]
struct RouteState {
    facts: Facts,
    track_id: i64,
    track: Option<TrackRow>,
    pool_row: Option<PoolRow>,
    ids: BTreeSet<i64>,
    steps: Vec<(i64, i64, Scored, Payload)>,
    scores: Vec<f64>,
    priority: f64,
}

fn route_payload(track: &TrackRow, conn: &Connection) -> Result<Payload, DbError> {
    let payload = Payload::of_track(track, lyrics_row_exists(conn, track.id)?);
    Ok(match assist::parse_transition(track.title.as_deref()) {
        Some((_, end, _)) => payload.with_bpm(end),
        None => payload,
    })
}

fn route(conn: &Connection, request: RouteRequest, now: chrono::NaiveDateTime) -> Result<Value, Response> {
    let db_error = |error: DbError| server_error(&error);
    let intent = assist::resolve_intent(&request.intent).map_err(input_error)?;
    let transition = request.transition && intent != "wordplay";
    let label = assist::preset(intent).label;
    if intent == "wordplay" {
        return Ok(json!({
            "source": null, "target": null, "intent": intent, "intent_label": label, "transition": false,
            "max_intermediate": request.max_intermediate, "routes": [],
            "notes": ["ワードプレイは承認済みの直接ペアを使う機能のため、ルート提案には対応していません"], "caveats": [],
        }));
    }
    let max_intermediate = request.max_intermediate.clamp(0, MAX_ROUTE_INTERMEDIATE);
    let conditions = Filters::from_raw(&request.filters).map_err(input_error)?;
    let source = get_track(conn, BigInt(request.source.into())).map_err(db_error)?.ok_or_else(|| not_found("Source track not found"))?;
    let target = get_track(conn, BigInt(request.target.into())).map_err(db_error)?.ok_or_else(|| not_found("Target track not found"))?;
    let caveats: Vec<&str> = if transition { assist::TRANSITION_CAVEATS.to_vec() } else { Vec::new() };
    let source_dump = Payload::of_track(&source, lyrics_row_exists(conn, source.id).map_err(db_error)?);
    let target_dump = Payload::of_track(&target, lyrics_row_exists(conn, target.id).map_err(db_error)?);
    let base = |routes: Vec<Value>, notes: Vec<String>| {
        json!({
            "source": source_dump.value(), "target": target_dump.value(), "intent": intent, "intent_label": label,
            "transition": transition, "max_intermediate": max_intermediate, "caveats": caveats, "routes": routes, "notes": notes,
        })
    };
    if source.id == target.id || identity(source.title.as_deref(), source.artist.as_deref()) == identity(target.title.as_deref(), target.artist.as_deref()) {
        return Ok(base(Vec::new(), vec!["出発曲と目的曲が同じ録音のため、ルートを作れません".into()]));
    }
    let library = Rekordbox::open();
    let registered = match library.registered() {
        Ok(registered) => registered,
        Err(error) => return Ok(base(Vec::new(), vec![format!("{error}。登録済みの原本を確認できないため、ルート提案を停止しました")])),
    };
    if !available(&source.filepath, &registered) {
        return Ok(base(Vec::new(), vec!["出発曲が rekordbox に登録されていないか、ファイルが見つかりません".into()]));
    }
    if !available(&target.filepath, &registered) {
        return Ok(base(Vec::new(), vec!["目的曲が rekordbox に登録されていないか、ファイルが見つかりません".into()]));
    }
    let scope_field = match request.scope.as_str() {
        "same_genre" => Some("genre"),
        "same_subgenre" => Some("subgenre"),
        _ => None,
    };
    let value_of = |track: &TrackRow, field: &str| normalize_text(if field == "genre" { track.genre.as_deref() } else { track.subgenre.as_deref() });
    if let Some(field) = scope_field {
        let label = if field == "genre" { "ジャンル" } else { "サブジャンル" };
        if value_of(&source, field).is_empty() {
            return Ok(base(Vec::new(), vec![format!("元曲の{label}が未登録のため、ルートを作れません")]));
        }
        if value_of(&target, field) != value_of(&source, field) {
            return Ok(base(Vec::new(), vec![format!("目的曲が元曲と同じ{label}ではないため、現在の範囲では到達できません")]));
        }
    }
    let rows = metadata(conn).map_err(db_error)?;
    let mut excluded = excluded_with_duplicates(&rows, Some(source.id), &request.exclude);
    excluded.remove(&target.id);
    let source_scope = scope_field.map(|f| value_of(&source, f));
    let eligible_ids: Vec<i64> = rows
        .iter()
        .filter(|row| !excluded.contains(&row.facts.id) && row.facts.id != target.id)
        .filter(|row| match (scope_field, &source_scope) {
            (Some(field), Some(value)) => normalize_text(if field == "genre" { row.facts.genre.as_deref() } else { row.facts.subgenre.as_deref() }) == *value,
            _ => true,
        })
        .filter(|row| conditions.matches(&row.facts, row.album.as_deref()))
        .map(|row| row.facts.id)
        .collect();
    if !conditions.is_empty() && eligible_ids.is_empty() {
        return Ok(base(Vec::new(), vec!["中継曲に指定した条件に合う曲がライブラリにありません".into()]));
    }
    let source_payload = route_payload(&source, conn).map_err(db_error)?;
    let target_info = candidate_pool(conn, &PoolQuery { targets: Targets::default(), genres: None, subgenres: None, limit: 1, exclude_ids: &[], candidate_ids: Some(&[target.id]) })
        .map_err(db_error)?;
    let Some(target_info) = candidates(target_info, now).into_iter().next() else {
        return Ok(base(Vec::new(), vec!["目的曲の解析情報を読み込めませんでした".into()]));
    };
    let route_excluded: Vec<i64> = excluded.iter().copied().chain([target.id]).collect::<BTreeSet<_>>().into_iter().collect();
    let target_route_payload = route_payload(&target, conn).map_err(db_error)?;
    let mut route_pool: Vec<Candidate> = Vec::new();
    for (track, bpm) in [(&source, source_payload.facts.bpm), (&target, target_route_payload.facts.bpm)] {
        for candidate in candidate_pool_for(
            conn, track, intent, &route_excluded, None, &BTreeMap::new(), &eligible_ids, &conditions, transition, bpm, MAX_ROUTE_POOL, request.tolerance, now,
        )
        .map_err(db_error)?
        {
            if let Some(slot) = route_pool.iter_mut().find(|c| c.row.id == candidate.row.id) {
                *slot = candidate;
            } else {
                route_pool.push(candidate);
            }
        }
    }
    let stored_target = embedding(conn, target.id).map_err(db_error)?;
    let target_anchor = (target_route_payload.facts.clone(), stored_target.as_ref().and_then(|(_, j)| parse_embedding(j.as_deref())), stored_target.and_then(|(m, _)| m));
    let mut states = vec![RouteState {
        facts: source_payload.facts.clone(),
        track_id: source.id,
        track: Some(source.clone()),
        pool_row: None,
        ids: [source.id].into_iter().collect(),
        steps: Vec::new(),
        scores: Vec::new(),
        priority: 1.0,
    }];
    let mut routes: Vec<(f64, Vec<i64>, Value)> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if eligible_ids.is_empty() && max_intermediate > 0 {
        notes.push("条件に合う中継曲がありません。出発曲から目的曲への直接接続だけを確認しました".into());
    }
    for depth in 0..=max_intermediate {
        let mut next: Vec<RouteState> = Vec::new();
        for state in &states {
            let state_embedding = embedding(conn, state.track_id).map_err(db_error)?;
            let context = Context {
                source: state.facts.clone(),
                model: state_embedding.as_ref().and_then(|(m, _)| m.clone()),
                vector: state_embedding.as_ref().and_then(|(_, j)| parse_embedding(j.as_deref())),
                pairs: BTreeMap::new(),
                transition,
                history: None,
                history_source: identity(state.facts.title.as_deref(), state.facts.artist.as_deref()),
                tolerance: request.tolerance,
            };
            // The direct edge to the destination.
            let fit = if transition { assist::transition_fit(state.facts.bpm, &target_info.payload.facts) } else { None };
            let passes = if transition {
                fit.is_some() && assist::passes_transition_filter(intent, &state.facts, &target_info.payload.facts)
            } else {
                assist::passes_intent_filter(intent, &state.facts, &target_info.payload.facts, request.tolerance)
            };
            if passes {
                let result = evaluate_candidate(&context, &target_info, intent, fit.as_ref(), false);
                let mut steps = state.steps.clone();
                steps.push((state.track_id, target.id, result, target_info.payload.clone()));
                let scores: Vec<f64> = steps.iter().map(|s| s.2.score).collect();
                let quality = scores.iter().sum::<f64>() / scores.len() as f64 - 0.03 * (steps.len() as f64 - 1.0).max(0.0);
                let mut tracks = vec![source_dump.value()];
                tracks.extend(steps[..steps.len() - 1].iter().map(|s| s.3.value()));
                tracks.push(target_dump.value());
                let ids: Vec<i64> = tracks.iter().map(|t| t["id"].as_i64().unwrap_or(0)).collect();
                let score = round4(quality.max(0.0));
                let value = json!({
                    "score": score,
                    "tracks": tracks,
                    "steps": steps.iter().map(|s| json!({
                        "from_track_id": s.0, "to_track_id": s.1, "score": round4(s.2.score), "summary": s.2.summary, "reasons": reasons_json(&s.2),
                    })).collect::<Vec<_>>(),
                });
                routes.push((score, ids, value));
            }
            if depth >= max_intermediate {
                continue;
            }
            let blocked: BTreeSet<i64> = excluded.iter().chain(&state.ids).copied().chain([target.id]).collect();
            let (entries, _, _) = score_pool(&route_pool, intent, &registered, &context);
            for entry in entries {
                let id = entry.payload.facts.id;
                if blocked.contains(&id) {
                    continue;
                }
                let Some(info) = route_pool.iter().find(|c| c.row.id == id) else { continue };
                let similarity = match (&target_anchor.1, &info.row.vector) {
                    (Some(anchor), Some(vector)) if embedding_space(target_anchor.2.as_deref()) == embedding_space(info.row.embedding_model.as_deref()) => {
                        cosine_similarity(Some(anchor.as_slice()), Some(vector.as_slice()), target_anchor.2.as_deref(), info.row.embedding_model.as_deref())
                    }
                    _ => None,
                };
                let like: Option<Likeness> = if target_anchor.0.id == id { None } else { assist::likeness(&target_anchor.0, &entry.payload.facts, similarity) };
                let mut scores = state.scores.clone();
                scores.push(entry.result.score);
                let average = scores.iter().sum::<f64>() / scores.len() as f64;
                let mut payload = info.payload.clone();
                if let Some((_, end, _)) = assist::parse_transition(info.row.title.as_deref()) {
                    payload = payload.with_bpm(end);
                }
                let mut ids = state.ids.clone();
                ids.insert(id);
                let mut steps = state.steps.clone();
                steps.push((state.track_id, id, entry.result.clone(), entry.payload.clone()));
                next.push(RouteState {
                    facts: payload.facts.clone(),
                    track_id: id,
                    track: None,
                    pool_row: Some(info.row.clone()),
                    ids,
                    steps,
                    scores,
                    priority: 0.65 * average + 0.35 * like.map_or(0.0, |l| l.score),
                });
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| (-a.priority).partial_cmp(&-b.priority).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.ids.iter().cmp(b.ids.iter())));
        next.truncate(MAX_ROUTE_BEAM);
        states = next;
    }
    let mut unique: Vec<(f64, Vec<i64>, Value)> = Vec::new();
    for route in routes {
        if !unique.iter().any(|r| r.1 == route.1) {
            unique.push(route);
        }
    }
    unique.sort_by(|a, b| {
        (-a.0).partial_cmp(&-b.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(&b.1))
    });
    if unique.is_empty() {
        notes.push("指定した条件では、目的曲までのつなぎやすいルートが見つかりません".into());
        if request.scope == "any" {
            notes.push("BPM・キー・系統の条件を緩めるか、中継曲の条件を減らして試してください".into());
        }
    } else if request.scope == "any" {
        let changes = unique.iter().any(|(_, _, value)| {
            let tracks = value["tracks"].as_array().cloned().unwrap_or_default();
            tracks.len() > 1 && {
                let facts = |v: &Value| Facts { genre: v["genre"].as_str().map(str::to_owned), subgenre: v["subgenre"].as_str().map(str::to_owned), ..Facts::default() };
                assist::genre_continuity_score(&facts(&tracks[0]), &facts(&tracks[1])).is_some_and(|s| s < 0.75)
            }
        });
        if changes {
            notes.push("一部のルートでジャンルが変わります。各ステップの連続性を確認してください".into());
        }
    }
    Ok(base(unique.into_iter().take(request.limit).map(|(_, _, v)| v).collect(), notes))
}

// ------------------------------------------------------------------ search

/// `POST /api/assist/search`.
pub async fn search(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = parse(&headers, &body, |b| {
        b.check_keys(&["filters", "limit", "exclude_track_ids"])?;
        Ok((b.filters()?, b.int("limit", Some(12), 1, 50)?.unwrap_or(12) as usize, b.ints("exclude_track_ids", 10_000)?))
    });
    let (raw, limit, exclude) = match parsed {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let conditions = match Filters::from_raw(&raw) {
                Ok(conditions) => conditions,
                Err(message) => return Ok(Err(input_error(message))),
            };
            let response = |candidates: Vec<Value>, notes: Vec<String>, unavailable: usize, total: usize| {
                json!({"filters": conditions.describe(), "reference": null, "candidates": candidates, "notes": notes, "unavailable_originals": unavailable, "total_matches": total})
            };
            if conditions.is_empty() {
                return Ok(Ok(response(Vec::new(), vec!["検索条件か「〜のような曲」を1つ以上指定してください".into()], 0, 0)));
            }
            let rows = metadata(&conn)?;
            let excluded = excluded_with_duplicates(&rows, None, &exclude);
            let mut matching: Vec<&Meta> = rows.iter().filter(|row| !excluded.contains(&row.facts.id) && conditions.matches(&row.facts, row.album.as_deref())).collect();
            if matching.is_empty() {
                return Ok(Ok(response(Vec::new(), vec!["指定した条件に合う曲がライブラリにありません".into()], 0, 0)));
            }
            let registered = match Rekordbox::open().registered() {
                Ok(registered) => registered,
                Err(error) => return Ok(Ok(response(Vec::new(), vec![format!("{error}。登録済みの原本を確認できないため、検索を停止しました")], 0, 0))),
            };
            let by_tempo = conditions.bpm_min.is_some() || conditions.bpm_max.is_some();
            matching.sort_by(|a, b| {
                let tempo = |m: &Meta| if by_tempo { m.facts.bpm.filter(|b| *b != 0.0).unwrap_or(0.0) } else { 0.0 };
                tempo(a)
                    .partial_cmp(&tempo(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| normalize_text(a.facts.artist.as_deref()).cmp(&normalize_text(b.facts.artist.as_deref())))
                    .then_with(|| normalize_text(a.facts.title.as_deref()).cmp(&normalize_text(b.facts.title.as_deref())))
                    .then_with(|| a.facts.id.cmp(&b.facts.id))
            });
            let (mut chosen, mut seen, mut unavailable) = (Vec::new(), BTreeSet::new(), 0);
            for row in &matching {
                if !available(&row.filepath, &registered) {
                    unavailable += 1;
                    continue;
                }
                let id = identity(row.facts.title.as_deref(), row.facts.artist.as_deref());
                if id.is_some() && seen.contains(&id) {
                    continue;
                }
                seen.insert(id);
                chosen.push(row.facts.id);
                if chosen.len() >= limit.min(MAX_SUGGESTIONS) {
                    break;
                }
            }
            let tracks = plumdeck_db::repos::tracks::tracks_by_ids(&conn, &chosen)?;
            let mut notes = Vec::new();
            if unavailable > 0 {
                notes.push(format!("rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"));
            }
            let mut candidates = Vec::new();
            for id in &chosen {
                if let Some(track) = tracks.iter().find(|t| t.id == *id) {
                    let payload = Payload::of_track(track, lyrics_row_exists(&conn, track.id)?);
                    candidates.push(search_payload(&payload));
                }
            }
            Ok(Ok(response(candidates, notes, unavailable, matching.len())))
        })
        .await;
    match result {
        Ok(Ok(value)) => answer(value),
        Ok(Err(response)) | Err(response) => response,
    }
}

/// `_search_payload_from`.
fn search_payload(payload: &Payload) -> Value {
    let facts = &payload.facts;
    let parts = [
        facts.bpm.filter(|b| *b != 0.0).map_or("BPM 不明".to_owned(), |b| format!("{} BPM", g(b))),
        facts.key.clone().filter(|k| !k.is_empty()).unwrap_or_else(|| "キー不明".into()),
        facts.genre.clone().filter(|k| !k.is_empty()).unwrap_or_else(|| "ジャンル未登録".into()),
        facts.year.filter(|y| *y != 0.0).map_or("年不明".to_owned(), |y| format!("{}", y as i64)),
    ];
    let mut value = payload.dump.clone();
    value.insert("score".into(), Value::Null);
    value.insert("components".into(), json!({}));
    value.insert("like".into(), Value::Null);
    value.insert("summary".into(), json!("条件に一致（つなぎの相性は基準デッキがあると判定できます）"));
    value.insert("strengths".into(), json!([]));
    value.insert("reasons".into(), json!([{"kind": "match", "tone": "neutral", "text": parts.join(" · ")}]));
    value.insert("wordplay".into(), Value::Null);
    Value::Object(value)
}

// ------------------------------------------------------------------ agent picks

/// `describe_tracks`: check tracks an agent picked and explain them against
/// the source deck.
pub fn describe_tracks(conn: &Connection, ids: &[i64], source_id: Option<i64>, intent: &str, now: chrono::NaiveDateTime) -> Result<Value, DbError> {
    let intent = assist::resolve_intent(intent).unwrap_or("keep");
    let mut wanted: Vec<i64> = Vec::new();
    for id in ids {
        if !wanted.contains(id) {
            wanted.push(*id);
        }
    }
    let library = Rekordbox::open();
    let registered = match library.registered() {
        Ok(registered) => registered,
        Err(error) => {
            return Ok(json!({"tracks": [], "rejected": wanted.iter().map(|id| json!({"track_id": id, "reason": error})).collect::<Vec<_>>()}));
        }
    };
    let source = match source_id {
        Some(id) => get_track(conn, BigInt(id.into()))?,
        None => None,
    };
    let context = match &source {
        Some(source) => {
            let mut payload = Payload::of_track(source, lyrics_row_exists(conn, source.id)?);
            if let Some((_, end, _)) = assist::parse_transition(source.title.as_deref()) {
                payload = payload.with_bpm(end);
            }
            let stored = embedding(conn, source.id)?;
            let (history, _) = history_from(&library, conn, now)?;
            Some(Context {
                source: payload.facts,
                model: stored.as_ref().and_then(|(m, _)| m.clone()),
                vector: stored.as_ref().and_then(|(_, j)| parse_embedding(j.as_deref())),
                pairs: approved_pairs(conn, source.id)?,
                transition: false,
                history,
                history_source: identity(source.title.as_deref(), source.artist.as_deref()),
                tolerance: None,
            })
        }
        None => None,
    };
    let pool = if wanted.is_empty() {
        Vec::new()
    } else {
        candidates(
            candidate_pool(conn, &PoolQuery { targets: Targets::default(), genres: None, subgenres: None, limit: wanted.len() as i64, exclude_ids: &[], candidate_ids: Some(&wanted) })?,
            now,
        )
    };
    let (mut offered, mut rejected) = (Vec::new(), Vec::new());
    for id in &wanted {
        let Some(candidate) = pool.iter().find(|c| c.row.id == *id) else {
            rejected.push(json!({"track_id": id, "reason": "ライブラリに存在しない曲 ID です"}));
            continue;
        };
        if source.as_ref().is_some_and(|s| s.id == *id) {
            rejected.push(json!({"track_id": id, "reason": "基準デッキの曲そのものです"}));
            continue;
        }
        if !available(&candidate.row.filepath, &registered) {
            rejected.push(json!({"track_id": id, "reason": "rekordbox 未登録、またはファイルが見つからないためデッキに載せられません"}));
            continue;
        }
        let track = get_track(conn, BigInt((*id).into()))?;
        let payload = match &track {
            Some(track) => Payload::of_track(track, candidate.row.has_lyrics),
            None => candidate.payload.clone(),
        };
        let Some(context) = &context else {
            offered.push(search_payload(&payload));
            continue;
        };
        let scored = Candidate { row: candidate.row.clone(), payload: payload.clone() };
        let result = evaluate_candidate(context, &scored, intent, None, true);
        let evidence = context.history.as_ref().and_then(|history| {
            history.evidence(context.history_source.as_ref(), identity(payload.facts.title.as_deref(), payload.facts.artist.as_deref()).as_ref())
        });
        let fits = evidence.is_some() && history_fits(&context.source, &payload.facts, false);
        offered.push(entry_payload(&Entry { payload, result, history: evidence, history_fits: fits }, &context.pairs));
    }
    Ok(json!({"tracks": offered, "rejected": rejected}))
}

/// The recommendation, route and search services for MCP (same rules as the routes).
pub fn mcp_recommend(conn: &Connection, body: &Value, now: chrono::NaiveDateTime) -> Result<Value, String> {
    let text = body.to_string();
    let parsed = plumdeck_core::compat::pyjson_loads::loads(&text).map_err(|_| "invalid request".to_owned())?;
    let PyValue::Dict(pairs) = parsed else { return Err("invalid request".into()) };
    let b = Body(&pairs);
    let request = (|| -> Result<RecommendRequest, ()> {
        Ok(RecommendRequest {
            source: b.int("source_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            intent: match get(&pairs, "intent") { Some(PyValue::Str(s)) => s.clone(), _ => "keep".into() },
            transition: b.boolean("transition")?,
            limit: b.int("limit", Some(12), 1, 50)?.unwrap_or(12) as usize,
            exclude: b.ints("exclude_track_ids", 10_000)?,
            scope: b.choice("genre_scope", "any", &SCOPES)?,
            genres: None,
            filters: b.filters()?,
            tolerance: b.float("tempo_tolerance", 0.0, 12.0)?,
        })
    })()
    .map_err(|_| "invalid assist arguments".to_owned())?;
    recommend(conn, request, now).map_err(|_| "recommendation failed".to_owned())
}

pub fn mcp_route(conn: &Connection, body: &Value, now: chrono::NaiveDateTime) -> Result<Value, String> {
    let parsed = plumdeck_core::compat::pyjson_loads::loads(&body.to_string()).map_err(|_| "invalid request".to_owned())?;
    let PyValue::Dict(pairs) = parsed else { return Err("invalid request".into()) };
    let b = Body(&pairs);
    let request = (|| -> Result<RouteRequest, ()> {
        Ok(RouteRequest {
            source: b.int("source_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            target: b.int("target_track_id", None, i64::MIN.into(), i64::MAX.into())?.ok_or(())? as i64,
            intent: match get(&pairs, "intent") { Some(PyValue::Str(s)) => s.clone(), _ => "keep".into() },
            transition: b.boolean("transition")?,
            max_intermediate: b.int("max_intermediate", Some(2), 0, 4)?.unwrap_or(2) as i64,
            limit: b.int("limit", Some(3), 1, 5)?.unwrap_or(3) as usize,
            exclude: b.ints("exclude_track_ids", 10_000)?,
            scope: b.choice("genre_scope", "any", &SCOPES)?,
            filters: b.filters()?,
            tolerance: b.float("tempo_tolerance", 0.0, 12.0)?,
        })
    })()
    .map_err(|_| "invalid assist arguments".to_owned())?;
    route(conn, request, now).map_err(|_| "route failed".to_owned())
}

/// Validate and normalize filters as `AssistFilters.from_dict(...).to_dict()`.
pub fn normalized_filters(filters: &Value) -> Result<Value, String> {
    let parsed = plumdeck_core::compat::pyjson_loads::loads(&json!({"filters": filters}).to_string()).map_err(|_| "invalid filters".to_owned())?;
    let PyValue::Dict(pairs) = parsed else { return Err("invalid filters".into()) };
    let raw = Body(&pairs).filters().map_err(|_| "invalid filters".to_owned())?;
    let f = Filters::from_raw(&raw)?;
    Ok(json!({
        "bpm_min": f.bpm_min, "bpm_max": f.bpm_max, "genres": f.genres, "subgenres": f.subgenres, "artists": f.artists,
        "keys": f.keys, "year_min": f.year_min, "year_max": f.year_max, "query": f.query,
    }))
}
