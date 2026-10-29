//! The on-device genre classifier (plan §2.4, crate `plumdeck-genre`):
//! training on the user's verified genres, predictions for unverified
//! tracks, accepting them, and `GET /api/tracks/{id}/suggest-genre`.
//!
//! The model lives in the library folder (`genre-classifier.json`); no
//! weights ship with the app. Inputs are audio-derived only (embedding,
//! timbre, BPM); file-tag and rekordbox genres are never read, and the
//! prediction routes do not return a track's unverified genre.
//!
//! Routes:
//! - `GET  /api/genres/classifier` status (and auto-retrain when enough
//!   new verified labels exist)
//! - `POST /api/genres/classifier/train`
//! - `GET  /api/genres/classifier/predictions?level=&offset=&limit=`
//! - `POST /api/genres/classifier/accept` `{items:[{track_id, genre, subgenre?}]}`
//! - `GET  /api/genres/classifier/similar/{track_id}?min_confidence=`
//! - `GET  /api/tracks/{track_id}/suggest-genre`

use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use plumdeck_db::repos::genres::{accept_genres, genre_feature_rows, AcceptedGenre, FeatureScope, GenreFeatureRow};
use plumdeck_db::DbError;
use plumdeck_domain::mixability::parse_embedding;
use plumdeck_genre::{confidence_level, train, AudioFeatures, LabeledTrack, Model, Prediction, TrainConfig, LOW_CONFIDENCE};
use serde_json::{json, Value};

use crate::query::Params;
use crate::{server_error, AppState};

/// File name of the stored model in the library folder.
pub const MODEL_FILE: &str = "genre-classifier.json";
/// Retrain automatically once this many labels (or 5 % of the trained
/// labels, whichever is larger) are new or changed since training.
pub const AUTO_RETRAIN_MIN: usize = 20;
/// Train automatically the first time once this many labels exist.
pub const AUTO_FIRST_TRAIN: usize = 100;

#[derive(Default)]
struct Progress {
    fraction: f32,
    error: Option<String>,
    /// Number of labels the running job trains on.
    labels: usize,
}

struct Inner {
    path: PathBuf,
    /// `None` until first read from disk.
    model: Mutex<Option<Option<Arc<Model>>>>,
    training: AtomicBool,
    progress: Mutex<Progress>,
}

/// Shared classifier state: the loaded model and the training job.
#[derive(Clone)]
pub struct GenreClassifier {
    inner: Arc<Inner>,
}

impl GenreClassifier {
    pub fn new(library_dir: &FsPath) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: library_dir.join(MODEL_FILE),
                model: Mutex::new(None),
                training: AtomicBool::new(false),
                progress: Mutex::new(Progress::default()),
            }),
        }
    }

    /// The current model, loaded from disk on first use.
    pub fn model(&self) -> Option<Arc<Model>> {
        let mut slot = self.inner.model.lock().expect("model lock");
        if slot.is_none() {
            let loaded = std::fs::read_to_string(&self.inner.path).ok().and_then(|text| Model::from_json(&text)).map(Arc::new);
            *slot = Some(loaded);
        }
        slot.clone().flatten()
    }

    fn store(&self, model: Model) -> std::io::Result<()> {
        let tmp = self.inner.path.with_extension("json.tmp");
        std::fs::write(&tmp, model.to_json())?;
        std::fs::rename(&tmp, &self.inner.path)?;
        *self.inner.model.lock().expect("model lock") = Some(Some(Arc::new(model)));
        Ok(())
    }

    pub fn is_training(&self) -> bool {
        self.inner.training.load(Ordering::SeqCst)
    }

    /// Start a background training job unless one runs. Returns whether
    /// a job was started.
    pub fn start_training(&self, state: &AppState) -> bool {
        if self.inner.training.swap(true, Ordering::SeqCst) {
            return false;
        }
        *self.inner.progress.lock().expect("progress lock") = Progress::default();
        let this = self.clone();
        let db = state.db.clone();
        std::thread::Builder::new()
            .name("genre-classifier".into())
            .spawn(move || {
                let outcome = (|| -> Result<(), String> {
                    let rows = db.lease().and_then(|conn| genre_feature_rows(&conn, FeatureScope::Verified)).map_err(|e| e.to_string())?;
                    let (tracks, labels) = labeled(&rows);
                    this.inner.progress.lock().expect("progress lock").labels = tracks.len();
                    let progress = |fraction: f32| {
                        if let Ok(mut p) = this.inner.progress.lock() {
                            p.fraction = fraction;
                        }
                    };
                    let mut model = train(&tracks, &TrainConfig::default(), &progress).map_err(|e| e.to_string())?;
                    model.labels = labels;
                    this.store(model).map_err(|e| format!("could not save the model: {e}"))
                })();
                if let Err(error) = &outcome {
                    tracing::warn!(target: "plumdeck_backend::genre", "genre classifier training failed: {error}");
                } else {
                    tracing::info!(target: "plumdeck_backend::genre", "genre classifier trained");
                }
                this.inner.progress.lock().expect("progress lock").error = outcome.err();
                this.inner.training.store(false, Ordering::SeqCst);
            })
            .map_err(|error| {
                self.inner.training.store(false, Ordering::SeqCst);
                tracing::warn!(target: "plumdeck_backend::genre", "could not start training: {error}");
            })
            .is_ok()
    }
}

/// FNV-1a of `genre \x1f subgenre`.
fn label_hash(genre: &str, subgenre: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in genre.trim().bytes().chain([0x1f]).chain(subgenre.trim().bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

pub fn features(row: &GenreFeatureRow) -> Option<AudioFeatures> {
    let embedding = parse_embedding(row.embedding_json.as_deref())?.into_iter().map(|v| v as f32).collect();
    let features = AudioFeatures {
        embedding,
        brightness: row.brightness,
        loudness: row.loudness,
        contrast: row.contrast,
        noisiness: row.noisiness,
        bpm: row.bpm,
    };
    features.vector().map(|_| features)
}

/// Training tracks and their fingerprints, from verified rows with features.
fn labeled(rows: &[GenreFeatureRow]) -> (Vec<LabeledTrack>, Vec<(i64, u32)>) {
    let mut tracks = Vec::new();
    let mut labels = Vec::new();
    for row in rows {
        let Some(features) = features(row) else { continue };
        let genre = row.genre.clone().unwrap_or_default();
        let subgenre = row.subgenre.clone().unwrap_or_default();
        labels.push((row.id, label_hash(&genre, &subgenre)));
        tracks.push(LabeledTrack { features, genre, subgenre });
    }
    (tracks, labels)
}

/// Verified labels with an embedding that are new or changed since the
/// model was trained, and how many usable labels exist now.
fn label_delta(conn: &plumdeck_db::duckdb::Connection, model: Option<&Model>) -> Result<(usize, usize), DbError> {
    let mut statement = conn.prepare(
        "SELECT tracks.id, tracks.genre, coalesce(tracks.subgenre, '') FROM tracks JOIN track_embeddings ON track_embeddings.track_id = tracks.id \
         WHERE tracks.is_genre_verified = true AND tracks.genre IS NOT NULL AND tracks.genre != '' AND lower(tracks.genre) != 'unknown' \
         AND length(track_embeddings.embedding_json) > 2",
    )?;
    let rows: Vec<(i64, String, String)> = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    let known = model.map(|m| &m.labels);
    let changed = rows
        .iter()
        .filter(|(id, genre, sub)| {
            let hash = label_hash(genre, sub);
            known.is_none_or(|labels| labels.binary_search_by_key(id, |(i, _)| *i).map_or(true, |at| labels[at].1 != hash))
        })
        .count();
    Ok((changed, rows.len()))
}

fn should_auto_train(model: Option<&Model>, changed: usize, usable: usize) -> bool {
    match model {
        Some(model) => changed >= AUTO_RETRAIN_MIN.max(model.metrics.samples / 20),
        None => usable >= AUTO_FIRST_TRAIN,
    }
}

fn iso(seconds: i64) -> Value {
    chrono::DateTime::from_timestamp(seconds, 0).map_or(Value::Null, |t| json!(t.to_rfc3339()))
}

fn status_json(classifier: &GenreClassifier, changed: Option<(usize, usize)>, auto_started: bool) -> Value {
    let model = classifier.model();
    let progress = classifier.inner.progress.lock().expect("progress lock");
    let training = classifier.is_training();
    let state = if training {
        "training"
    } else if model.is_some() {
        "ready"
    } else {
        "untrained"
    };
    json!({
        "state": state,
        "progress": if training { json!(progress.fraction) } else { Value::Null },
        "training_labels": if training { json!(progress.labels) } else { Value::Null },
        "error": progress.error,
        "auto_started": auto_started,
        "trained_at": model.as_ref().map_or(Value::Null, |m| iso(m.trained_at)),
        "genres": model.as_ref().map_or(json!([]), |m| json!(m.genres)),
        "subgenre_count": model.as_ref().map_or(0, |m| m.pairs.len()),
        "metrics": model.as_ref().map_or(Value::Null, |m| serde_json::to_value(&m.metrics).unwrap_or(Value::Null)),
        "new_labels": changed.map(|(c, _)| c),
        "usable_labels": changed.map(|(_, u)| u),
        "min_labels": plumdeck_genre::MIN_TRAINING_SAMPLES,
        "auto_retrain_after": model.as_ref().map_or(AUTO_FIRST_TRAIN, |m| AUTO_RETRAIN_MIN.max(m.metrics.samples / 20)),
        "low_confidence": LOW_CONFIDENCE,
        "high_confidence": plumdeck_genre::HIGH_CONFIDENCE,
    })
}

/// Start training when enough labels are new; returns whether it started.
pub fn maybe_auto_train(state: &AppState, changed: usize, usable: usize) -> bool {
    let classifier = &state.genre_classifier;
    !classifier.is_training() && should_auto_train(classifier.model().as_deref(), changed, usable) && classifier.start_training(state)
}

/// `GET /api/genres/classifier`.
pub async fn status(State(state): State<AppState>) -> Response {
    let classifier = state.genre_classifier.clone();
    let db = state.db.clone();
    let model = classifier.model();
    let delta = state.blocking(move || db.lease().and_then(|conn| label_delta(&conn, model.as_deref()))).await;
    match delta {
        Ok((changed, usable)) => {
            let started = maybe_auto_train(&state, changed, usable);
            Json(status_json(&classifier, Some((changed, usable)), started)).into_response()
        }
        Err(response) => response,
    }
}

/// `POST /api/genres/classifier/train`.
pub async fn start_train(State(state): State<AppState>) -> Response {
    let started = state.genre_classifier.start_training(&state);
    let status = if started { StatusCode::ACCEPTED } else { StatusCode::CONFLICT };
    (status, Json(status_json(&state.genre_classifier, None, false))).into_response()
}

pub fn prediction_json(prediction: &Prediction) -> Value {
    json!({
        "genre": prediction.genre,
        "confidence": prediction.confidence,
        "level": prediction.level(),
        "subgenre": prediction.subgenre,
        "subgenre_confidence": prediction.subgenre_confidence,
        "candidates": prediction.candidates,
        "pair_candidates": prediction.pair_candidates,
    })
}

/// The track fields the prediction views show: never the unverified genre.
fn track_json(row: &GenreFeatureRow) -> Value {
    json!({
        "id": row.id,
        "title": row.title.clone().unwrap_or_default(),
        "artist": row.artist.clone().unwrap_or_default(),
        "bpm": row.bpm.map_or(0.0, f64::from),
        "filepath": row.filepath,
    })
}

/// Predictions for the given rows, most confident first; rows without
/// features are counted apart.
pub fn predict_rows(model: &Model, rows: &[GenreFeatureRow]) -> (Vec<(GenreFeatureRow, Prediction)>, usize) {
    let mut out = Vec::new();
    let mut missing = 0;
    for row in rows {
        match features(row).and_then(|f| model.predict(&f)) {
            Some(prediction) => out.push((row.clone(), prediction)),
            None => missing += 1,
        }
    }
    out.sort_by(|a, b| b.1.confidence.total_cmp(&a.1.confidence).then(a.0.id.cmp(&b.0.id)));
    (out, missing)
}

/// `GET /api/genres/classifier/predictions`.
pub async fn predictions(State(state): State<AppState>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let level = params.choice("level", "all", &["all", "high", "medium", "low"]);
    let offset = params.bounded_int("offset", 0, 0, None) as usize;
    let limit = params.bounded_int("limit", 100, 1, Some(1000)) as usize;
    if let Err(response) = params.finish() {
        return response;
    }
    let Some(model) = state.genre_classifier.model() else {
        return Json(json!({"trained": false, "total": 0, "counts": {"high": 0, "medium": 0, "low": 0}, "without_features": 0, "items": []})).into_response();
    };
    let db = state.db.clone();
    let rows = match state.blocking(move || db.lease().and_then(|conn| genre_feature_rows(&conn, FeatureScope::Unclassified))).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let (predicted, missing) = predict_rows(&model, &rows);
    let mut counts = json!({"high": 0, "medium": 0, "low": 0});
    for (_, prediction) in &predicted {
        let key = prediction.level();
        counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
    }
    let selected: Vec<&(GenreFeatureRow, Prediction)> = predicted.iter().filter(|(_, p)| level == "all" || p.level() == level).collect();
    let items: Vec<Value> = selected
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(row, prediction)| json!({"track": track_json(row), "prediction": prediction_json(prediction)}))
        .collect();
    Json(json!({"trained": true, "total": selected.len(), "counts": counts, "without_features": missing, "items": items})).into_response()
}

/// `POST /api/genres/classifier/accept`: write reviewed suggestions and
/// mark them verified (the person accepted them).
pub async fn accept(State(state): State<AppState>, body: Bytes) -> Response {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": "invalid JSON body"}))).into_response();
    };
    let Some(items) = value["items"].as_array() else {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": "items must be a list"}))).into_response();
    };
    let mut accepted = Vec::new();
    for item in items {
        let (Some(track_id), Some(genre)) = (item["track_id"].as_i64(), item["genre"].as_str().map(str::trim).filter(|g| !g.is_empty())) else {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": "each item needs track_id and genre"}))).into_response();
        };
        let subgenre = item["subgenre"].as_str().map(|s| s.trim().to_owned());
        accepted.push(AcceptedGenre { track_id, genre: genre.to_owned(), subgenre });
    }
    let db = state.db.clone();
    let model = state.genre_classifier.model();
    let result = state
        .blocking(move || {
            let updated = db.write(|conn| accept_genres(conn, &accepted))?;
            let delta = db.lease().and_then(|conn| label_delta(&conn, model.as_deref()))?;
            Ok((updated, delta))
        })
        .await;
    match result {
        Ok((updated, (changed, usable))) => {
            let started = maybe_auto_train(&state, changed, usable);
            Json(json!({"updated": updated, "new_labels": changed, "retraining": started})).into_response()
        }
        Err(response) => response,
    }
}

/// `GET /api/genres/classifier/similar/{track_id}`: unverified tracks the
/// classifier puts in the same genre (and subgenre) as the given track —
/// its verified labels, or its own prediction. Most confident first, 50
/// at most; `similarity` is the calibrated confidence.
pub async fn similar(State(state): State<AppState>, Path(track_id): Path<String>, uri: Uri) -> Response {
    let mut params = Params::from_uri(&uri);
    let track_id = params.path_int("track_id", &track_id);
    let min_confidence = params.float_or("min_confidence", 0.5) as f32;
    if let Err(response) = params.finish() {
        return response;
    }
    let Ok(track_id) = i64::try_from(track_id.expect("validated")) else { return Json(json!([])).into_response() };
    let Some(model) = state.genre_classifier.model() else { return Json(json!([])).into_response() };
    let db = state.db.clone();
    let rows = state
        .blocking(move || {
            let conn = db.lease()?;
            let reference = genre_feature_rows(&conn, FeatureScope::Ids(&[track_id]))?;
            let open = genre_feature_rows(&conn, FeatureScope::Unclassified)?;
            Ok((reference, open))
        })
        .await;
    let (reference, open) = match rows {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let Some(reference) = reference.first() else { return Json(json!([])).into_response() };
    let target = if reference.verified && reference.genre.as_deref().is_some_and(|g| !g.is_empty()) {
        Some((reference.genre.clone().unwrap_or_default(), reference.subgenre.clone().filter(|s| !s.is_empty())))
    } else {
        features(reference).and_then(|f| model.predict(&f)).map(|p| (p.genre, p.subgenre))
    };
    let Some((genre, subgenre)) = target else { return Json(json!([])).into_response() };
    let (predicted, _) = predict_rows(&model, &open);
    let items: Vec<Value> = predicted
        .iter()
        .filter(|(row, p)| row.id != track_id && p.genre == genre && p.confidence >= min_confidence)
        .filter(|(_, p)| subgenre.is_none() || p.subgenre == subgenre || p.subgenre.is_none())
        .take(50)
        .map(|(row, p)| {
            let mut item = track_json(row);
            item["similarity"] = json!(p.confidence);
            item["suggested_genre"] = json!(p.genre);
            item["suggested_subgenre"] = json!(p.subgenre);
            item
        })
        .collect();
    Json(Value::Array(items)).into_response()
}

fn reason(value: &str) -> Value {
    json!({"suggested_genre": null, "suggested_subgenre": null, "confidence": null, "level": null, "candidates": [], "reason": value})
}

/// `GET /api/tracks/{track_id}/suggest-genre`: the classifier's prediction
/// for one track (verified or not).
pub async fn suggest_genre(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let track_id = params.path_int("track_id", &track_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let Ok(track_id) = i64::try_from(track_id.expect("validated")) else { return Json(reason("no_embedding")).into_response() };
    let db = state.db.clone();
    let rows = match state.blocking(move || db.lease().and_then(|conn| genre_feature_rows(&conn, FeatureScope::Ids(&[track_id])))).await {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let Some(features) = rows.first().and_then(features) else { return Json(reason("no_embedding")).into_response() };
    let Some(model) = state.genre_classifier.model() else { return Json(reason("classifier_not_trained")).into_response() };
    match model.predict(&features) {
        Some(p) => Json(json!({
            "suggested_genre": p.genre,
            "suggested_subgenre": p.subgenre,
            "confidence": p.confidence,
            "level": confidence_level(p.confidence),
            "candidates": p.candidates,
            "reason": null,
        }))
        .into_response(),
        None => server_error(&"prediction failed"),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::router;
    use crate::test_support::{send, TestLibrary};

    fn embedding(cluster: usize, i: usize) -> String {
        let values: Vec<String> = (0..plumdeck_genre::EMBEDDING_DIM)
            .map(|d| {
                let noise = (((i * 7919 + d * 104_729) % 1000) as f64 / 1000.0 - 0.5) * 0.8;
                format!("{}", if d % 3 == cluster { 1.0 } else { 0.0 } + noise)
            })
            .collect();
        format!("[{}]", values.join(","))
    }

    /// 60 verified tracks in three genres, plus three unverified ones.
    fn seed(library: &TestLibrary) {
        let genres = [("House", "Deep House", 124), ("Hip Hop", "Trap", 90), ("Drum & Bass", "Liquid", 174)];
        let mut sql = String::new();
        for i in 0..63 {
            let (genre, sub, bpm) = genres[i % 3];
            let verified = i < 60;
            let stored_genre = if verified { genre } else { "Tagged Genre" };
            sql.push_str(&format!(
                "INSERT INTO tracks (id, filepath, title, artist, genre, subgenre, bpm, brightness, loudness, contrast, noisiness, is_genre_verified) \
                 VALUES ({id}, '/{id}', 'T{id}', 'A', '{stored_genre}', '{sub}', {bpm}, 0.2, -9, 0.3, 0.4, {verified});\
                 INSERT INTO track_embeddings (track_id, embedding_json) VALUES ({id}, '{emb}');",
                id = i + 1,
                sub = if verified { sub } else { "" },
                emb = embedding(i % 3, i)
            ));
        }
        library.execute(&sql);
    }

    #[tokio::test]
    async fn trains_predicts_and_accepts() {
        let library = TestLibrary::new("genre-classifier");
        seed(&library);
        let state = library.state();
        let app = || router(state.clone());
        let status = send(app(), Method::GET, "/api/genres/classifier", &[], "").await.json();
        assert_eq!(status["state"], "untrained");
        assert_eq!(status["usable_labels"], 60);
        let none = send(app(), Method::GET, "/api/tracks/61/suggest-genre", &[], "").await.json();
        assert_eq!(none["reason"], "classifier_not_trained");
        let started = send(app(), Method::POST, "/api/genres/classifier/train", &[], "").await;
        assert_eq!(started.status.as_u16(), 202);
        let mut status = json!({});
        for _ in 0..600 {
            status = send(app(), Method::GET, "/api/genres/classifier", &[], "").await.json();
            if status["state"] != "training" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(status["state"], "ready", "{status}");
        assert_eq!(status["new_labels"], 0);
        assert!(status["metrics"]["genre_accuracy"].as_f64().unwrap() > 0.8, "{status}");
        assert!(library.dir().join(MODEL_FILE).exists());

        let page = send(app(), Method::GET, "/api/genres/classifier/predictions", &[], "").await.json();
        assert_eq!(page["total"], 3);
        let first = &page["items"][0];
        assert!(first["track"].get("genre").is_none(), "the unverified tag genre is not returned");
        let suggestion = send(app(), Method::GET, "/api/tracks/61/suggest-genre", &[], "").await.json();
        assert_eq!(suggestion["suggested_genre"], "House");
        assert_eq!(suggestion["suggested_subgenre"], "Deep House");
        let similar = send(app(), Method::GET, "/api/genres/classifier/similar/1?min_confidence=0", &[], "").await.json();
        assert_eq!(similar.as_array().unwrap().iter().map(|t| t["id"].clone()).collect::<Vec<_>>(), [json!(61)]);

        let body = r#"{"items":[{"track_id":61,"genre":"House","subgenre":"Deep House"},{"track_id":62,"genre":"Hip Hop"}]}"#;
        let accepted = send(app(), Method::POST, "/api/genres/classifier/accept", &[("content-type", "application/json")], body).await.json();
        assert_eq!(accepted["updated"], 2);
        assert_eq!(accepted["new_labels"], 2);
        assert_eq!(accepted["retraining"], false);
        let page = send(app(), Method::GET, "/api/genres/classifier/predictions?level=all", &[], "").await.json();
        assert_eq!(page["total"], 1);
        let bad = send(app(), Method::POST, "/api/genres/classifier/accept", &[], r#"{"items":[{"track_id":1}]}"#).await;
        assert_eq!(bad.status.as_u16(), 422);
    }

    #[test]
    fn auto_training_policy() {
        assert!(!should_auto_train(None, 0, AUTO_FIRST_TRAIN - 1));
        assert!(should_auto_train(None, 0, AUTO_FIRST_TRAIN));
        assert_ne!(label_hash("House", "Deep"), label_hash("House", "Tech"));
        assert_eq!(label_hash(" House", "Deep "), label_hash("House", "Deep"));
    }
}
