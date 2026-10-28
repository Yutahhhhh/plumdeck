//! Release-date and lyrics lookups (backend/api/routers/metadata.py,
//! app/services/metadata_app_service.py, utils/external_metadata.py; S8):
//! `POST /api/metadata/fetch-lyrics-single`, `/update`, `/cancel`,
//! `/clear-cache` and `WS /ws/metadata`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::lyrics::get_lyrics;
use plumdeck_db::repos::tracks::{get_track, BigInt, TrackRow};
use regex::Regex;
use serde_json::{json, Value};

use crate::body::{parse_embedded, BodyModel, Fields, ValidationError};
use crate::response::{dict, text, PyJson};
use crate::tasks::BackgroundTask;
use crate::{AppState, HttpError};

const ITUNES: &str = "https://itunes.apple.com/search";
const LRCLIB_GET: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";

/// The metadata job and its skip cache (`cache/metadata_skip_cache.json`
/// next to the library).
#[derive(Clone)]
pub struct MetadataJobs {
    pub task: BackgroundTask,
    skip: Arc<Mutex<BTreeMap<String, BTreeSet<i64>>>>,
    cache_file: PathBuf,
}

impl MetadataJobs {
    pub fn new(library_dir: PathBuf) -> Self {
        let mut extra = serde_json::Map::new();
        extra.insert("updated".into(), json!(0));
        extra.insert("current_track".into(), json!(""));
        extra.insert("update_type".into(), Value::Null);
        let cache_file = library_dir.join("cache").join("metadata_skip_cache.json");
        let mut skip = BTreeMap::from([("release_date".to_owned(), BTreeSet::new()), ("lyrics".to_owned(), BTreeSet::new())]);
        if let Ok(Value::Object(data)) = std::fs::read(&cache_file).map_err(|_| ()).and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| ())) {
            for kind in ["release_date", "lyrics"] {
                let ids = data.get(kind).and_then(Value::as_array).map(|ids| ids.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
                skip.insert(kind.to_owned(), ids);
            }
        }
        Self { task: BackgroundTask::new(extra), skip: Arc::new(Mutex::new(skip)), cache_file }
    }

    fn save_skip(&self) {
        let skip = self.skip.lock().expect("skip cache").clone();
        let data = json!({"release_date": skip["release_date"], "lyrics": skip["lyrics"]});
        if let Some(parent) = self.cache_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.cache_file, serde_json::to_string_pretty(&data).unwrap_or_default());
    }

    /// `clear_skip_cache`.
    pub fn clear(&self, kind: Option<&str>) {
        {
            let mut skip = self.skip.lock().expect("skip cache");
            match kind {
                Some(kind) => {
                    if let Some(ids) = skip.get_mut(kind) {
                        ids.clear();
                    }
                }
                None => skip.values_mut().for_each(BTreeSet::clear),
            }
        }
        self.save_skip();
    }
}

fn skip_pattern() -> (Regex, Regex) {
    (
        Regex::new(r"\d{2,3}-\d{2,3}").expect("pattern"),
        Regex::new(r"(?i)\b(transition|intro|outro|clean|dirty|extended|edit|mashup|bootleg)\b").expect("pattern"),
    )
}

/// `should_skip_track`: DJ tools, transitions and edits are not looked up.
pub fn should_skip(title: &str) -> bool {
    let (transition, keywords) = skip_pattern();
    !title.is_empty() && (transition.is_match(title) || keywords.is_match(title))
}

/// `clean_search_term`: bracketed parts removed, whitespace collapsed.
pub fn clean_search_term(text: &str) -> String {
    let brackets = Regex::new(r"[\(\[].*?[\)\]]").expect("pattern");
    plumdeck_core::compat::text::collapse_spaces(&brackets.replace_all(text, ""))
}

fn usable(artist: &Option<String>, title: &Option<String>) -> Option<(String, String)> {
    let (artist, title) = (artist.clone().unwrap_or_default(), title.clone().unwrap_or_default());
    if artist.is_empty() || title.is_empty() || artist == "Unknown" || title == "Unknown" || should_skip(&title) {
        return None;
    }
    Some((artist, title))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default()
}

/// `fetch_itunes_release_date`.
pub async fn itunes_release_date(artist: &Option<String>, title: &Option<String>) -> Option<String> {
    let (artist, title) = usable(artist, title)?;
    let mut queries = vec![format!("{artist} {title}"), format!("{} {}", clean_search_term(&artist), clean_search_term(&title))];
    queries.retain(|query| !query.trim().is_empty());
    queries.dedup();
    let client = client();
    for query in queries {
        let url = format!("{ITUNES}?term={}&entity=song&limit=1", crate::routes::setlists::quote_path(&query));
        let Ok(response) = client.get(&url).send().await else { continue };
        if response.status() != 200 {
            continue;
        }
        let Ok(data) = response.json::<Value>().await else { continue };
        if data["resultCount"].as_i64().unwrap_or(0) > 0 {
            return data["results"][0]["releaseDate"].as_str().map(str::to_owned);
        }
    }
    None
}

/// `fetch_lrclib_lyrics`: `/api/get` first, `/api/search` on a 404.
pub async fn lrclib_lyrics(track: &TrackRow) -> Option<Value> {
    let (artist, title) = usable(&track.artist, &track.title)?;
    let mut params = vec![("artist_name", artist.clone()), ("track_name", title.clone())];
    if let Some(album) = track.album.clone().filter(|album| !album.is_empty() && album != "Unknown") {
        params.push(("album_name", album));
    }
    if let Some(duration) = track.duration.filter(|d| *d != 0.0 && d.is_finite()) {
        params.push(("duration", format!("{}", duration.trunc() as i64)));
    }
    let client = client();
    let response = client.get(LRCLIB_GET).query(&params).send().await.ok()?;
    match response.status().as_u16() {
        200 => response.json().await.ok(),
        404 => {
            let search = client.get(LRCLIB_SEARCH).query(&[("q", format!("{artist} {title}"))]).send().await.ok()?;
            if search.status() != 200 {
                return None;
            }
            let results: Value = search.json().await.ok()?;
            results.as_array().and_then(|items| items.first().cloned())
        }
        _ => None,
    }
}

fn lyrics_text(data: &Value) -> Option<String> {
    [&data["syncedLyrics"], &data["plainLyrics"]].iter().find_map(|value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
}

struct TrackIdBody {
    track_id: i128,
}

impl BodyModel for TrackIdBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let track_id = fields.required_int("track_id");
        fields.finish(|| Some(Self { track_id: track_id? }))
    }
}

fn not_found(detail: &str) -> Response {
    HttpError(ApiError::new(404, detail)).into_response()
}

/// `POST /api/metadata/fetch-lyrics-single`.
pub async fn fetch_lyrics_single(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_embedded::<TrackIdBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let id = BigInt(request.track_id);
    let track = match state.blocking(move || db.lease().and_then(|conn| get_track(&conn, id))).await {
        Ok(Some(track)) => track,
        Ok(None) => return not_found("Track not found"),
        Err(response) => return response,
    };
    let Some(data) = lrclib_lyrics(&track).await else { return not_found("Lyrics not found") };
    let Some(content) = lyrics_text(&data) else { return not_found("Lyrics content empty") };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let stored = content.clone();
    match state.blocking(move || db.write(|conn| plumdeck_db::repos::lyrics::store_fetched_lyrics(conn, track.id, &stored, now))).await {
        Ok(()) => PyJson(dict([("lyrics", text(content))])).into_response(),
        Err(response) => response,
    }
}

struct UpdateBody {
    kind: String,
    overwrite: bool,
    track_ids: Option<Vec<i128>>,
}

impl BodyModel for UpdateBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let kind = fields.required_str("type");
        let overwrite = fields.bool_or("overwrite", false);
        let track_ids = fields.optional_int_list("track_ids");
        fields.finish(|| Some(Self { kind: kind?, overwrite, track_ids }))
    }
}

fn status(kind: &str, message: &str) -> Response {
    PyJson(dict([("status", text(kind)), ("message", text(message))])).into_response()
}

/// `POST /api/metadata/update`: start the release-date or lyrics job.
pub async fn start_update(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match crate::body::parse_body::<UpdateBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let jobs = state.metadata.clone();
    let app = state.clone();
    let started = jobs.task.start(move |task| run_update(app, task, request.kind, request.overwrite, request.track_ids));
    if started {
        status("success", "Update started")
    } else {
        status("error", "Update already running")
    }
}

async fn run_update(state: AppState, task: BackgroundTask, kind: String, overwrite: bool, ids: Option<Vec<i128>>) -> Result<(), String> {
    let jobs = state.metadata.clone();
    task.update(&[("updated", json!(0)), ("current_track", json!("")), ("update_type", json!(kind))]);
    let skip: Vec<i64> = if overwrite { Vec::new() } else { jobs.skip.lock().expect("skip cache").get(&kind).map(|s| s.iter().copied().collect()).unwrap_or_default() };
    let db = state.db.clone();
    let query_kind = kind.clone();
    let tracks = state
        .blocking(move || db.lease().and_then(|conn| plumdeck_db::repos::tracks::metadata_job_tracks(&conn, &query_kind, overwrite, ids.as_deref(), &skip)))
        .await
        .map_err(|_| "database error".to_owned())?;
    task.update(&[("type", json!("start")), ("total", json!(tracks.len())), ("message", json!(format!("Starting {kind} update...")))]);
    task.emit();
    for (index, track) in tracks.iter().enumerate() {
        task.update(&[
            ("current", json!(index + 1)),
            ("current_track", json!(format!("{} - {}", track.artist.as_deref().unwrap_or("None"), track.title.as_deref().unwrap_or("None")))),
            ("type", json!("processing")),
        ]);
        task.emit();
        let outcome = match kind.as_str() {
            "release_date" => update_release_date(&state, track, overwrite).await,
            "lyrics" => update_lyrics(&state, track, overwrite).await,
            _ => Ok((false, None)),
        };
        match outcome {
            Ok((true, _)) => task.add("updated", 1),
            Ok((false, reason)) => {
                task.add("skipped", 1);
                if reason == Some("not_found") {
                    jobs.skip.lock().expect("skip cache").entry(kind.clone()).or_default().insert(track.id);
                }
            }
            Err(()) => task.add("errors", 1),
        }
        task.add("processed", 1);
        tokio::time::sleep(Duration::from_secs_f64(if kind == "release_date" { 3.0 } else { 1.0 })).await;
    }
    jobs.save_skip();
    let cached = jobs.skip.lock().expect("skip cache").get(&kind).map_or(0, BTreeSet::len);
    task.update(&[("type", json!("complete")), ("message", json!(format!("Update complete ({cached} tracks cached as not found)")))]);
    task.emit();
    Ok(())
}

async fn update_release_date(state: &AppState, track: &TrackRow, overwrite: bool) -> Result<(bool, Option<&'static str>), ()> {
    if track.year.is_some_and(|year| year != 0) && !overwrite {
        return Ok((false, Some("already_exists")));
    }
    let Some(date) = itunes_release_date(&track.artist, &track.title).await else { return Ok((false, Some("not_found"))) };
    let Some(year) = date.get(..4).and_then(|y| plumdeck_core::compat::pyobj::int_from_str(y)) else { return Ok((false, Some("not_found"))) };
    if track.year == Some(year as i64) {
        return Ok((false, Some("already_exists")));
    }
    let db = state.db.clone();
    let id = track.id;
    state.blocking(move || db.write(|conn| plumdeck_db::repos::tracks::set_year(conn, id, year as i64))).await.map_err(|_| ())?;
    Ok((true, None))
}

async fn update_lyrics(state: &AppState, track: &TrackRow, overwrite: bool) -> Result<(bool, Option<&'static str>), ()> {
    let db = state.db.clone();
    let id = BigInt(track.id.into());
    let existing = state.blocking(move || db.lease().and_then(|conn| get_lyrics(&conn, id))).await.map_err(|_| ())?;
    if existing.as_ref().is_some_and(|row| row.content.as_deref().is_some_and(|c| !c.is_empty())) && !overwrite {
        return Ok((false, Some("already_exists")));
    }
    let Some(content) = lrclib_lyrics(track).await.as_ref().and_then(lyrics_text) else { return Ok((false, Some("not_found"))) };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let id = track.id;
    state.blocking(move || db.write(|conn| plumdeck_db::repos::lyrics::store_fetched_lyrics(conn, id, &content, now))).await.map_err(|_| ())?;
    Ok((true, None))
}

/// `POST /api/metadata/cancel`.
pub async fn cancel_update(State(state): State<AppState>) -> Response {
    state.metadata.task.cancel().await;
    status("success", "Update cancelled")
}

struct ClearBody {
    update_type: Option<String>,
}

impl BodyModel for ClearBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let update_type = fields.optional_str("update_type");
        fields.finish(|| Some(Self { update_type }))
    }
}

/// `POST /api/metadata/clear-cache`.
pub async fn clear_cache(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_embedded::<ClearBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    state.metadata.clear(request.update_type.as_deref().filter(|kind| !kind.is_empty()));
    status("success", "Cache cleared")
}

/// `WS /ws/metadata`: the state now and on every change.
pub async fn ws_metadata(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    let task = state.metadata.task.clone();
    upgrade.on_upgrade(move |socket| serve_task(socket, task))
}

/// Send `task`'s state on connect and on every change until the client goes.
pub async fn serve_task(mut socket: WebSocket, task: BackgroundTask) {
    let mut updates = task.subscribe();
    if socket.send(Message::Text(task.state().to_string().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(state) => {
                    if socket.send(Message::Text(state.to_string().into())).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}
