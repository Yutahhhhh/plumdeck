//! Writes into audio files and the artwork lookup (S8):
//! `POST /api/genres/apply-to-files` (backend/app/services/genre_app_service.py),
//! `PATCH /api/metadata/update` and `POST /api/metadata/fetch-artwork-info`
//! (backend/app/services/filesystem_app_service.py).

use std::path::Path as FsPath;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::lyrics::get_lyrics;
use plumdeck_db::repos::tracks::{get_track, BigInt};
use plumdeck_media::tag_write::{write_lyrics_and_art, write_tags, BasicTags};
use serde_json::Value;

use crate::body::{parse_body, parse_embedded, BodyModel, Fields, ValidationError};
use crate::response::{dict, int, text, PyJson};
use crate::{AppState, HttpError};

struct ApplyBody {
    track_ids: Vec<i128>,
}

impl BodyModel for ApplyBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let track_ids = fields.int_list("track_ids");
        fields.finish(|| Some(Self { track_ids: track_ids? }))
    }
}

/// `POST /api/genres/apply-to-files`: write title, artist, album, year,
/// genre and lyrics of each track into its file (all tracks, up to 10000,
/// when the list is empty).
pub async fn apply_to_files(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_body::<ApplyBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let loaded = state
        .blocking(move || {
            let conn = db.lease()?;
            let tracks = if request.track_ids.is_empty() {
                plumdeck_db::repos::tracks::first_tracks(&conn, 10_000)?
            } else {
                let mut tracks = Vec::new();
                for id in &request.track_ids {
                    if let Some(track) = get_track(&conn, BigInt(*id))? {
                        tracks.push(track);
                    }
                }
                tracks
            };
            let mut items = Vec::with_capacity(tracks.len());
            for track in tracks {
                let lyrics = get_lyrics(&conn, BigInt(track.id.into()))?.and_then(|row| row.content);
                items.push((track, lyrics));
            }
            Ok(items)
        })
        .await;
    let items = match loaded {
        Ok(items) => items,
        Err(response) => return response,
    };
    let outcome = state
        .blocking_result(move || {
            let (mut success, mut failed) = (0i64, 0i64);
            for (track, lyrics) in items {
                if track.filepath.is_empty() {
                    failed += 1;
                    continue;
                }
                let basic = BasicTags { title: track.title, artist: track.artist, album: track.album, year: track.year, genre: track.genre };
                if write_tags(FsPath::new(&track.filepath), &basic, lyrics.as_deref()) {
                    success += 1;
                } else {
                    failed += 1;
                }
            }
            (success, failed)
        })
        .await;
    match outcome {
        Ok((success, failed)) => PyJson(dict([("success", int(success)), ("failed", int(failed))])).into_response(),
        Err(response) => response,
    }
}

struct ContentBody {
    track_id: i128,
    lyrics: Option<String>,
    artwork_data: Option<String>,
}

impl BodyModel for ContentBody {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let track_id = fields.required_int("track_id");
        let lyrics = fields.optional_str("lyrics");
        let artwork_data = fields.optional_str("artwork_data");
        fields.finish(|| Some(Self { track_id: track_id?, lyrics, artwork_data }))
    }
}

/// `PATCH /api/metadata/update`: lyrics and/or artwork into the file.
pub async fn update_content(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_body::<ContentBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let id = BigInt(request.track_id);
    let track = match state.blocking(move || db.lease().and_then(|conn| get_track(&conn, id))).await {
        Ok(Some(track)) if !track.filepath.is_empty() => track,
        Ok(_) => return HttpError(ApiError::new(404, "Track not found")).into_response(),
        Err(response) => return response,
    };
    if request.lyrics.is_some() || request.artwork_data.is_some() {
        let artwork = match request.artwork_data.as_deref().filter(|data| !data.is_empty()).map(|data| STANDARD.decode(data)) {
            None => None,
            Some(Ok(bytes)) => Some(bytes),
            Some(Err(_)) => return HttpError(ApiError::new(500, "Failed to write metadata to file")).into_response(),
        };
        let lyrics = request.lyrics.clone();
        let written = state
            .blocking_result(move || write_lyrics_and_art(FsPath::new(&track.filepath), lyrics.as_deref(), artwork.as_deref()))
            .await;
        match written {
            Ok(true) => {}
            Ok(false) => return HttpError(ApiError::new(500, "Failed to write metadata to file")).into_response(),
            Err(response) => return response,
        }
    }
    PyJson(dict([("status", text("success"))])).into_response()
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

/// `POST /api/metadata/fetch-artwork-info`: an iTunes album cover as a
/// `data:` URL, or `""`.
pub async fn fetch_artwork_info(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request = match parse_embedded::<TrackIdBody>(&headers, &body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let id = BigInt(request.track_id);
    let track = match state.blocking(move || db.lease().and_then(|conn| get_track(&conn, id))).await {
        Ok(Some(track)) => track,
        Ok(None) => return HttpError(ApiError::new(404, "Track not found")).into_response(),
        Err(response) => return response,
    };
    let none = |value: &Option<String>| value.clone().unwrap_or_else(|| "None".into());
    let query = match track.album.as_deref().filter(|album| !album.is_empty()) {
        Some(album) => format!("{} {album}", none(&track.artist)),
        None => format!("{} {}", none(&track.artist), none(&track.title)),
    };
    let url = format!(
        "https://itunes.apple.com/search?term={}&media=music&entity=album&limit=1",
        crate::routes::setlists::quote_path(&query)
    );
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
    let info = async {
        let data: Value = client.get(&url).send().await.ok()?.json().await.ok()?;
        if data["resultCount"].as_i64().unwrap_or(0) <= 0 {
            return Some(String::new());
        }
        let Some(artwork) = data["results"][0]["artworkUrl100"].as_str().filter(|u| !u.is_empty()) else { return Some(String::new()) };
        let image = client.get(artwork.replace("100x100bb", "1000x1000bb")).send().await.ok()?;
        if image.status() != 200 {
            return Some(String::new());
        }
        let bytes = image.bytes().await.ok()?;
        Some(format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)))
    }
    .await;
    // A failed lookup is an empty answer, as Python caught it.
    PyJson(dict([("info", text(info.unwrap_or_default()))])).into_response()
}
