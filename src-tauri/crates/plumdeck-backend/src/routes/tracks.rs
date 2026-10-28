//! The library list and track edits (backend/api/routers/tracks.py):
//! `GET /api/tracks`, `/page`, `/count`, `/ids`, `PATCH
//! /api/tracks/{id}/info` and `/genre`.
//!
//! `GET /api/tracks/{id}/similar` ranks by the stored embeddings; `/suggest-genre`
//! and the MCP-only `target_params` live with S4/S9. `/count` runs only the count query; Python also built (and threw
//! away) a one-item page, which cannot fail differently.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::py_strip;
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::tracks::{
    count_tracks, search_track_ids, search_tracks, similar_tracks, update_genre, SimilarError, update_track_info, BigInt, TrackFilters, TrackHit,
    TrackInfo,
};

use crate::body::{parse_body_for, BodyModel, Fields, ValidationError};
use crate::models::{track_dict, track_reads, TrackExtras};
use crate::query::Params;
use crate::response::{boolean, dict, int, opt, pydantic, text, PyJson};
use crate::{server_error, AppState, HttpError};

/// The search filters every list route takes, with FastAPI's defaults.
fn filters(params: &mut Params) -> TrackFilters {
    TrackFilters {
        status: params.str_or("status", "all"),
        q: params.opt_str("q"),
        title: params.opt_str("title"),
        artist: params.opt_str("artist"),
        album: params.opt_str("album"),
        genres: params.list("genres"),
        subgenres: params.list("subgenres"),
        key: params.opt_str("key"),
        bpm: params.opt_float("bpm"),
        bpm_range: params.float_or("bpm_range", 5.0),
        min_duration: params.opt_float("min_duration"),
        max_duration: params.opt_float("max_duration"),
        min_energy: params.opt_float("min_energy"),
        max_energy: params.opt_float("max_energy"),
        min_danceability: params.opt_float("min_danceability"),
        max_danceability: params.opt_float("max_danceability"),
        min_brightness: params.opt_float("min_brightness"),
        max_brightness: params.opt_float("max_brightness"),
        min_year: params.opt_int("min_year").map(BigInt),
        max_year: params.opt_int("max_year").map(BigInt),
        year_status: params.str_or("year_status", "all"),
        lyrics_status: params.str_or("lyrics_status", "all"),
        lyrics: params.opt_str("lyrics"),
    }
}

/// `bool(lyrics_content and lyrics_content.strip())`.
fn has_lyrics(lyrics: Option<&str>) -> bool {
    lyrics.is_some_and(|text| !py_strip(text).is_empty())
}

fn extras(hit: &TrackHit) -> TrackExtras {
    TrackExtras {
        is_analyzed: Some(hit.is_analyzed),
        lyrics: hit.lyrics.clone(),
        has_lyrics: has_lyrics(hit.lyrics.as_deref()),
    }
}

/// `search_tracks`' dict for a page item.
fn hit_dict(hit: &TrackHit) -> PyValue {
    track_dict(
        &hit.track,
        vec![
            ("is_analyzed", boolean(hit.is_analyzed)),
            ("lyrics", opt(hit.lyrics.clone(), text)),
            ("has_lyrics", boolean(has_lyrics(hit.lyrics.as_deref()))),
        ],
    )
}

/// `GET /api/tracks`: `List[TrackRead]`.
pub async fn get_tracks(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let filters = filters(&mut params);
    let limit = params.int_or("limit", 100);
    let offset = params.int_or("offset", 0);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let hits = state
        .blocking(move || db.lease().and_then(|conn| search_tracks(&conn, &filters, None, "asc", BigInt(limit), BigInt(offset))))
        .await;
    match hits {
        Ok(hits) => match track_reads(hits.iter().map(|hit| (&hit.track, extras(hit)))) {
            Some(value) => pydantic(value).into_response(),
            None => server_error(&"response validation failed (TrackRead)"),
        },
        Err(response) => response,
    }
}

/// `GET /api/tracks/page`: one bounded page with the exact total.
pub async fn get_tracks_page(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let filters = filters(&mut params);
    let sort = params.opt_str("sort");
    let order = params.choice("order", "asc", &["asc", "desc"]);
    let limit = params.bounded_int("limit", 100, 1, Some(500));
    let offset = params.bounded_int("offset", 0, 0, None);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let page = state
        .blocking(move || {
            let conn = db.lease()?;
            let hits = search_tracks(&conn, &filters, sort.as_deref(), &order, BigInt(limit), BigInt(offset))?;
            let total = count_tracks(&conn, &filters)?;
            Ok((hits, total))
        })
        .await;
    let (hits, total) = match page {
        Ok(page) => page,
        Err(response) => return response,
    };
    let has_more = offset + (hits.len() as i128) < i128::from(total);
    PyJson(dict([
        ("items", PyValue::List(hits.iter().map(hit_dict).collect())),
        ("total", int(total)),
        ("limit", PyValue::Int(limit)),
        ("offset", PyValue::Int(offset)),
        ("has_more", boolean(has_more)),
    ]))
    .into_response()
}

/// `GET /api/tracks/count`: `{"count": total}`.
pub async fn get_tracks_count(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let filters = filters(&mut params);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| count_tracks(&conn, &filters)))
        .await
        .map(|total| PyJson(dict([("count", int(total))])))
        .into_response()
}

/// `GET /api/tracks/ids`: `List[int]`, newest first.
pub async fn get_track_ids(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let filters = filters(&mut params);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| search_track_ids(&conn, &filters)))
        .await
        .map(|ids| pydantic(PyValue::List(ids.into_iter().map(int).collect())))
        .into_response()
}

/// `GET /api/tracks/{track_id}/similar`: `List[TrackRead]` by embedding.
pub async fn get_similar(State(state): State<AppState>, Path(track_id): Path<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let id = params.path_int("track_id", &track_id);
    let limit = params.int_or("limit", 20);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    let db = state.db.clone();
    let found = state
        .blocking(move || db.lease().map(|conn| similar_tracks(&conn, BigInt(id), BigInt(limit))))
        .await;
    match found {
        Ok(Ok(hits)) => {
            let reads = track_reads(hits.iter().map(|hit| {
                (&hit.track, TrackExtras { is_analyzed: Some(hit.is_analyzed), lyrics: None, has_lyrics: has_lyrics(hit.lyrics.as_deref()) })
            }));
            match reads {
                Some(value) => pydantic(value).into_response(),
                None => server_error(&"response validation failed (TrackRead)"),
            }
        }
        Ok(Err(SimilarError::NoEmbedding)) => HttpError(ApiError::new(404, "Track embedding not found. Please analyze the track first.")).into_response(),
        Ok(Err(SimilarError::Db(error))) => HttpError(ApiError::new(500, format!("Vector search failed: {error}"))).into_response(),
        Err(response) => response,
    }
}

/// `TrackInfoUpdate`.
struct TrackInfoUpdate {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year: Option<i128>,
}

impl BodyModel for TrackInfoUpdate {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let update = Self {
            title: fields.optional_str("title"),
            artist: fields.optional_str("artist"),
            album: fields.optional_str("album"),
            year: fields.optional_int("year"),
        };
        fields.finish(|| Some(update))
    }
}

/// `GenreUpdate`.
struct GenreUpdate {
    genre: String,
}

impl BodyModel for GenreUpdate {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let genre = fields.required_str("genre");
        fields.finish(|| Some(Self { genre: genre? }))
    }
}

/// Path id and body, validated together.
fn path_and_body<T: BodyModel>(raw_id: &str, headers: &HeaderMap, body: &[u8]) -> Result<(i128, T), Response> {
    let parsed = parse_body_for::<T>(headers, body)?;
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = params.path_int("track_id", raw_id);
    match parsed {
        Ok(model) => {
            params.finish()?;
            Ok((id.expect("validated"), model))
        }
        Err(errors) => {
            params.extend(errors);
            Err(params.finish().expect_err("body errors"))
        }
    }
}

fn track_not_found() -> Response {
    HttpError(ApiError::new(404, "Track not found")).into_response()
}

/// `PATCH /api/tracks/{track_id}/info`: set the given metadata fields.
pub async fn update_info(
    State(state): State<AppState>,
    Path(track_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (track_id, update) = match path_and_body::<TrackInfoUpdate>(&track_id, &headers, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    // A year beyond BIGINT cannot be stored (Python failed in the driver).
    let year = match update.year.map(i64::try_from).transpose() {
        Ok(year) => year,
        Err(error) => return server_error(&error),
    };
    let info = TrackInfo { title: update.title, artist: update.artist, album: update.album, year };
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| update_track_info(conn, BigInt(track_id), &info))).await {
        Ok(Some(row)) => PyJson(track_dict(&row, Vec::new())).into_response(),
        Ok(None) => track_not_found(),
        Err(response) => response,
    }
}

/// `PATCH /api/tracks/{track_id}/genre`: set the genre and mark it verified.
pub async fn update_track_genre(
    State(state): State<AppState>,
    Path(track_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (track_id, update) = match path_and_body::<GenreUpdate>(&track_id, &headers, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let db = state.db.clone();
    match state.blocking(move || db.write(|conn| update_genre(conn, BigInt(track_id), &update.genre))).await {
        Ok(Some(row)) => PyJson(track_dict(&row, Vec::new())).into_response(),
        Ok(None) => track_not_found(),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    const ROWS: &str = "INSERT INTO tracks (filepath, title, artist, album, genre, subgenre, key, scale, bpm, duration, created_at) VALUES
        ('/m/a.mp3', 'Alpha', 'DJ A', 'LP', 'House', '', 'F minor', '', 124, 300, '2026-01-01 00:00:00'),
        ('/m/b.mp3', 'Beta', 'DJ B', NULL, 'Techno', 'Peak', '4A', '', 62, 200, '2026-01-02 00:00:00');
        INSERT INTO lyrics (track_id, content) VALUES (1, 'la la');";

    #[tokio::test]
    async fn lists_pages_counts_and_ids() {
        let library = TestLibrary::new("tracks");
        library.execute(ROWS);
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/api/tracks?key=Fm", &[], "").await;
        let body = answer.json();
        assert_eq!(body.as_array().unwrap().len(), 2);
        assert_eq!(body[0]["title"], "Beta");
        assert_eq!(body[1]["lyrics"], "la la");
        assert_eq!(body[1]["has_lyrics"], true);
        assert_eq!(body[1]["is_analyzed"], false);
        let page = send(app(), Method::GET, "/api/tracks/page?sort=title&order=desc&limit=1", &[], "").await.json();
        assert_eq!((page["total"].clone(), page["has_more"].clone(), page["items"][0]["title"].clone()), (json!(2), json!(true), json!("Beta")));
        let count = send(app(), Method::GET, "/api/tracks/count?genres=House&genres=Techno&bpm=124&bpm_range=1", &[], "").await;
        assert_eq!(count.json(), json!({"count": 2}));
        let ids = send(app(), Method::GET, "/api/tracks/ids?q=alpha", &[], "").await;
        assert_eq!(ids.json(), json!([1]));
        for bad in ["/api/tracks/page?limit=0", "/api/tracks/page?order=up", "/api/tracks?bpm=", "/api/tracks/ids?min_year=1.5"] {
            assert_eq!(send(app(), Method::GET, bad, &[], "").await.status.as_u16(), 422, "{bad}");
        }
        // A NULL title fails TrackRead (500) but the dict page still answers.
        library.execute("UPDATE tracks SET title = NULL WHERE id = 2");
        assert_eq!(send(app(), Method::GET, "/api/tracks", &[], "").await.status.as_u16(), 500);
        assert_eq!(send(app(), Method::GET, "/api/tracks/page", &[], "").await.status.as_u16(), 200);
    }

    #[tokio::test]
    async fn edits_info_and_genre() {
        let library = TestLibrary::new("track-edits");
        library.execute(ROWS);
        let app = || router(library.state());
        let json_type = [("content-type", "application/json")];
        let answer = send(app(), Method::PATCH, "/api/tracks/1/info", &json_type, r#"{"title":"New","year":"1999"}"#).await;
        let body = answer.json();
        assert_eq!((body["title"].clone(), body["year"].clone(), body["artist"].clone()), (json!("New"), json!(1999), json!("DJ A")));
        assert!(body.get("lyrics").is_none());
        let answer = send(app(), Method::PATCH, "/api/tracks/9/info", &json_type, "{}").await;
        assert_eq!((answer.status.as_u16(), answer.body.as_str()), (404, r#"{"detail":"Track not found"}"#));
        assert_eq!(send(app(), Method::PATCH, "/api/tracks/x/info", &json_type, "{}").await.status.as_u16(), 422);
        assert_eq!(send(app(), Method::PATCH, "/api/tracks/1/info", &json_type, "").await.status.as_u16(), 422);
        let answer = send(app(), Method::PATCH, "/api/tracks/2/genre", &json_type, r#"{"genre":"Ambient"}"#).await;
        assert_eq!((answer.json()["genre"].clone(), answer.json()["is_genre_verified"].clone()), (json!("Ambient"), json!(true)));
        assert_eq!(send(app(), Method::PATCH, "/api/tracks/2/genre", &json_type, r#"{"genre":1}"#).await.status.as_u16(), 422);
    }
}
