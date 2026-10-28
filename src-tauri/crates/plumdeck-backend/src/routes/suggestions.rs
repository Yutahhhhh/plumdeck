//! Embedding-based genre suggestions (backend/app/services/recommendation_app_service.py,
//! S4): `GET /api/tracks/{id}/suggest-genre`, `GET /api/genres/grouped-suggestions`
//! and `GET /api/genres/grouped-suggestions/{track_id}`.
//!
//! numpy's matrix products and norms are summed left to right here; on
//! embeddings whose partial sums are exact the similarities agree bit for
//! bit, otherwise they can differ in the last place (see
//! plumdeck_domain::mixability). `np.argsort` is replaced by a stable sort,
//! which is what numpy's small-array quicksort amounts to (ties only).

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::recommendations::{candidate_vectors, embedding, parent_vectors, verified_genre_vectors};
use plumdeck_db::repos::tracks::{tracks_by_ids, TrackRow};
use plumdeck_db::DbError;
use plumdeck_domain::mixability::{embedding_space, parse_embedding};

use crate::models::{track_read, TrackExtras};
use crate::query::Params;
use crate::response::{dict, float, int, opt, pydantic, text, PyJson};
use crate::{server_error, AppState};

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |sum, (x, y)| sum + x * y)
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

/// A failed numpy operation (a ragged matrix, mismatched shapes): a 500.
struct ShapeError;

/// `np.array(vectors)` then `matrix @ vector / (norms * vector_norm)` with
/// zero row norms replaced by 1e-10.
fn similarities(rows: &[Vec<f64>], vector: &[f64], vector_norm: f64) -> Result<Vec<f64>, ShapeError> {
    let width = rows.first().map_or(0, Vec::len);
    if rows.iter().any(|row| row.len() != width) {
        return Err(ShapeError);
    }
    if !rows.is_empty() && width != vector.len() {
        return Err(ShapeError);
    }
    Ok(rows
        .iter()
        .map(|row| {
            let mut row_norm = norm(row);
            if row_norm == 0.0 {
                row_norm = 1e-10;
            }
            dot(row, vector) / (row_norm * vector_norm)
        })
        .collect())
}

/// Indexes in ascending order of `values`, ties in index order.
fn argsort(values: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].partial_cmp(&values[*b]).unwrap_or(std::cmp::Ordering::Equal));
    order
}

/// The stored embedding of a track and its space.
fn target(conn: &Connection, track_id: i64) -> Result<(Option<Vec<f64>>, String), DbError> {
    let stored = embedding(conn, track_id)?;
    let model = stored.as_ref().and_then(|(model, _)| model.clone());
    let vector = stored.and_then(|(_, json)| parse_embedding(json.as_deref()));
    Ok((vector, embedding_space(model.as_deref()).to_owned()))
}

fn reason(value: &str) -> PyValue {
    dict([("suggested_genre", PyValue::None), ("reason", text(value))])
}

/// `GET /api/tracks/{track_id}/suggest-genre`.
pub async fn suggest_genre(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let track_id = params.path_int("track_id", &track_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let Ok(track_id) = i64::try_from(track_id.expect("validated")) else { return PyJson(reason("no_embedding")).into_response() };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let (vector, space) = target(&conn, track_id)?;
            let Some(vector) = vector else { return Ok(Ok(reason("no_embedding"))) };
            let candidates = verified_genre_vectors(&conn, track_id, Some(Some(&space)))?;
            if candidates.is_empty() {
                return Ok(Ok(reason("no_verified_tracks")));
            }
            let (genres, rows): (Vec<String>, Vec<Vec<f64>>) =
                candidates.into_iter().filter(|(_, row)| row.len() == vector.len()).unzip();
            if rows.is_empty() {
                return Ok(Ok(reason("no_valid_candidates")));
            }
            let target_norm = norm(&vector);
            if target_norm == 0.0 {
                return Ok(Ok(reason("zero_norm_target")));
            }
            let Ok(scores) = similarities(&rows, &vector, target_norm) else { return Ok(Err(())) };
            let k = scores.len().min(10);
            let order = argsort(&scores);
            let top: Vec<usize> = order[order.len() - k..].iter().rev().copied().collect();
            let mut totals: Vec<(String, f64)> = Vec::new();
            for index in top {
                let genre = &genres[index];
                match totals.iter_mut().find(|(name, _)| name == genre) {
                    Some(entry) => entry.1 += scores[index],
                    None => totals.push((genre.clone(), 0.0 + scores[index])),
                }
            }
            // max(): the first of the largest.
            let mut best: Option<&(String, f64)> = None;
            for entry in &totals {
                if best.is_none_or(|current| entry.1 > current.1) {
                    best = Some(entry);
                }
            }
            let genre = best.map(|(genre, _)| genre.clone()).expect("at least one candidate");
            Ok(Ok(dict([("suggested_genre", text(genre)), ("reason", PyValue::None)])))
        })
        .await;
    match result {
        Ok(Ok(value)) => PyJson(value).into_response(),
        Ok(Err(())) => server_error(&"embedding shapes do not match"),
        Err(response) => response,
    }
}

/// Python `items[start:stop]` bounds for `len` items.
fn slice_bounds(len: usize, start: i128, stop: i128) -> (usize, usize) {
    let len = len as i128;
    let clamp = |at: i128| if at < 0 { (at + len).max(0) } else { at.min(len) };
    let (start, stop) = (clamp(start), clamp(stop));
    (start as usize, stop.max(start) as usize)
}

/// `GET /api/genres/grouped-suggestions`: verified tracks by how many
/// unclassified tracks are at least `threshold` similar (fewest first).
pub async fn grouped_suggestions(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let offset = params.int_or("offset", 0);
    let limit = params.int_or("limit", 10);
    let threshold = params.float_or("threshold", 0.85);
    let mode = params.choice("mode", "genre", &["genre", "subgenre", "both"]);
    if let Err(response) = params.finish() {
        return response;
    }
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let parents = parent_vectors(&conn)?;
            if parents.is_empty() {
                return Ok(Ok(Vec::new()));
            }
            let mut matrices: Vec<(String, Vec<Vec<f64>>)> = Vec::new();
            let mut stats: Vec<(i64, usize)> = Vec::new();
            for (id, vector, space) in &parents {
                if !matrices.iter().any(|(name, _)| name == space) {
                    let rows: Vec<Vec<f64>> = candidate_vectors(&conn, &mode, Some(space))?.into_iter().map(|(_, row)| row).collect();
                    let width = rows.first().map_or(0, Vec::len);
                    if rows.iter().any(|row| row.len() != width) {
                        return Ok(Err(()));
                    }
                    matrices.push((space.clone(), rows));
                }
                let rows = &matrices.iter().find(|(name, _)| name == space).expect("cached").1;
                let parent_norm = norm(vector);
                if parent_norm == 0.0 || rows.is_empty() || rows[0].is_empty() {
                    stats.push((*id, 0));
                    continue;
                }
                let Ok(scores) = similarities(rows, vector, parent_norm) else { return Ok(Err(())) };
                stats.push((*id, scores.iter().filter(|score| **score >= threshold).count()));
            }
            stats.sort_by_key(|(id, count)| (*count, *id));
            let (start, stop) = slice_bounds(stats.len(), offset, offset.saturating_add(limit));
            let sliced = &stats[start..stop];
            if sliced.is_empty() {
                return Ok(Ok(Vec::new()));
            }
            let ids: Vec<i64> = sliced.iter().map(|(id, _)| *id).collect();
            let tracks: Vec<TrackRow> = tracks_by_ids(&conn, &ids)?;
            Ok(Ok(sliced
                .iter()
                .filter_map(|(id, count)| tracks.iter().find(|track| track.id == *id).map(|track| (track.clone(), *count)))
                .collect::<Vec<_>>()))
        })
        .await;
    let found = match result {
        Ok(Ok(found)) => found,
        Ok(Err(())) => return server_error(&"embedding shapes do not match"),
        Err(response) => return response,
    };
    let mut items = Vec::with_capacity(found.len());
    for (track, count) in &found {
        let Some(parent) = track_read(track, &TrackExtras::default()) else {
            return server_error(&"response validation failed (TrackRead)");
        };
        items.push(dict([("parent_track", parent), ("suggestion_count", int(*count as i64))]));
    }
    pydantic(PyValue::List(items)).into_response()
}

/// `GET /api/genres/grouped-suggestions/{track_id}`: unverified tracks at
/// least `threshold` similar to the track, most similar first (50 at most).
pub async fn suggestions_for_track(State(state): State<AppState>, Path(track_id): Path<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let track_id = params.path_int("track_id", &track_id);
    let threshold = params.float_or("threshold", 0.85);
    if let Err(response) = params.finish() {
        return response;
    }
    let Ok(track_id) = i64::try_from(track_id.expect("validated")) else { return pydantic(PyValue::List(Vec::new())).into_response() };
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            let conn = db.lease()?;
            let (vector, space) = target(&conn, track_id)?;
            let Some(vector) = vector else { return Ok(Ok(Vec::new())) };
            let candidates = candidate_vectors(&conn, "genre", Some(&space))?;
            let (ids, rows): (Vec<i64>, Vec<Vec<f64>>) = candidates.into_iter().unzip();
            let width = rows.first().map_or(0, Vec::len);
            if rows.iter().any(|row| row.len() != width) {
                return Ok(Err(()));
            }
            if rows.is_empty() || width == 0 {
                return Ok(Ok(Vec::new()));
            }
            let parent_norm = norm(&vector);
            if parent_norm == 0.0 {
                return Ok(Ok(Vec::new()));
            }
            let Ok(scores) = similarities(&rows, &vector, parent_norm) else { return Ok(Err(())) };
            let matched: Vec<usize> = (0..scores.len()).filter(|index| scores[*index] >= threshold).collect();
            if matched.is_empty() {
                return Ok(Ok(Vec::new()));
            }
            let matched_scores: Vec<f64> = matched.iter().map(|index| scores[*index]).collect();
            let order = argsort(&matched_scores);
            let top: Vec<usize> = order.iter().rev().map(|at| matched[*at]).take(50).collect();
            let top_ids: Vec<i64> = top.iter().map(|index| ids[*index]).collect();
            let tracks = tracks_by_ids(&conn, &top_ids)?;
            Ok(Ok(top
                .iter()
                .filter_map(|index| tracks.iter().find(|track| track.id == ids[*index]).map(|track| (track.clone(), scores[*index])))
                .collect::<Vec<_>>()))
        })
        .await;
    match result {
        Ok(Ok(found)) => {
            let items = found
                .iter()
                .map(|(track, similarity)| {
                    dict([
                        ("id", int(track.id)),
                        ("title", text(track.title.clone().unwrap_or_default())),
                        ("artist", text(track.artist.clone().unwrap_or_default())),
                        ("bpm", float(track.bpm.filter(|bpm| *bpm != 0.0).unwrap_or(0.0))),
                        ("filepath", text(track.filepath.clone())),
                        ("current_genre", opt(track.genre.clone(), text)),
                        ("similarity", float(*similarity)),
                    ])
                })
                .collect();
            pydantic(PyValue::List(items)).into_response()
        }
        Ok(Err(())) => server_error(&"embedding shapes do not match"),
        Err(response) => response,
    }
}
