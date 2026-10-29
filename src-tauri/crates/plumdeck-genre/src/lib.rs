//! On-device genre classifier (plan §2.4).
//!
//! Trained on the user's own verified genres only (`is_genre_verified`),
//! from audio-derived features only: the 200-d MusiCNN embedding, the four
//! timbre features (brightness, loudness, contrast, noisiness) and BPM.
//! File-tag and rekordbox genres are never an input or a label.
//!
//! The model is a small multilayer perceptron (one ReLU hidden layer) with
//! two softmax heads that share the hidden layer: the genre, and the
//! (genre, subgenre) pair. It is trained with Adam on mini-batches, in pure
//! Rust, on the user's machine. A stratified held-out split measures the
//! accuracy and fits a temperature per head (temperature scaling), so the
//! reported confidence is calibrated; the shipped model is then refit on
//! every label with the same settings and temperatures.
//!
//! Why this model: on the owner's library (25k verified tracks, 20 genres,
//! ~900 genre/subgenre pairs) a linear softmax and a k-NN vote reach
//! ~64 % / ~60 % held-out genre accuracy; one hidden layer adds a few
//! points at a cost of seconds, stays tiny (≈ 100k weights, no shipped
//! weights) and gives probabilities that temperature scaling calibrates
//! well, which the "low confidence → AI / human" routing depends on.

use serde::{Deserialize, Serialize};

mod net;
mod rng;

pub use net::TrainConfig;
use net::Net;
use rng::Rng;

/// Length of the MusiCNN embedding the classifier reads.
pub const EMBEDDING_DIM: usize = 200;
/// Embedding, four timbre features, BPM / 100 and a "has BPM" flag.
pub const FEATURE_DIM: usize = EMBEDDING_DIM + 4 + 2;
/// Model file format.
pub const MODEL_VERSION: u32 = 1;
/// Genres with fewer verified tracks are not learned.
pub const MIN_GENRE_SAMPLES: usize = 5;
/// Genre/subgenre pairs with fewer verified tracks are not learned.
pub const MIN_SUBGENRE_SAMPLES: usize = 3;
/// Fewer usable labels than this and there is nothing worth training.
pub const MIN_TRAINING_SAMPLES: usize = 20;
/// Calibrated confidence below this is "low": candidates for AI / review.
pub const LOW_CONFIDENCE: f32 = 0.5;
/// Calibrated confidence at or above this is "high".
pub const HIGH_CONFIDENCE: f32 = 0.8;

/// The audio features of one track.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFeatures {
    pub embedding: Vec<f32>,
    pub brightness: Option<f32>,
    pub loudness: Option<f32>,
    pub contrast: Option<f32>,
    pub noisiness: Option<f32>,
    pub bpm: Option<f32>,
}

impl AudioFeatures {
    /// The raw feature vector, or `None` without a usable embedding.
    pub fn vector(&self) -> Option<Vec<f32>> {
        if self.embedding.len() != EMBEDDING_DIM || !self.embedding.iter().all(|v| v.is_finite()) {
            return None;
        }
        let finite = |value: Option<f32>, fallback: f32| value.filter(|v| v.is_finite()).unwrap_or(fallback);
        let mut out = Vec::with_capacity(FEATURE_DIM);
        out.extend_from_slice(&self.embedding);
        out.push(finite(self.brightness, 0.0));
        out.push(finite(self.loudness, -60.0));
        out.push(finite(self.contrast, 0.0));
        out.push(finite(self.noisiness, 0.0));
        match self.bpm.filter(|bpm| bpm.is_finite() && *bpm > 0.0) {
            Some(bpm) => {
                out.push(bpm / 100.0);
                out.push(1.0);
            }
            None => {
                out.push(0.0);
                out.push(0.0);
            }
        }
        Some(out)
    }
}

/// A verified track: its features and the labels a person gave it.
#[derive(Clone, Debug)]
pub struct LabeledTrack {
    pub features: AudioFeatures,
    pub genre: String,
    pub subgenre: String,
}

/// Held-out measurements of a trained model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Labels the model was fit on (all usable verified tracks).
    pub samples: usize,
    pub train_samples: usize,
    pub holdout_samples: usize,
    pub genre_accuracy: f32,
    pub genre_top3_accuracy: f32,
    /// Over held-out tracks that have a subgenre.
    pub subgenre_accuracy: f32,
    /// Share of held-out tracks predicted with high confidence, and the
    /// genre accuracy on that share.
    pub high_confidence_coverage: f32,
    pub high_confidence_accuracy: f32,
    /// Share of held-out tracks below [`LOW_CONFIDENCE`].
    pub low_confidence_share: f32,
    /// Expected calibration error of the genre head (10 bins).
    pub calibration_error: f32,
    pub per_genre: Vec<GenreMetric>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GenreMetric {
    pub genre: String,
    pub support: usize,
    pub recall: f32,
}

/// The stored classifier.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub version: u32,
    /// Seconds since the Unix epoch.
    pub trained_at: i64,
    pub genres: Vec<String>,
    /// (genre index, subgenre) of the pair head.
    pub pairs: Vec<(usize, String)>,
    pub mean: Vec<f32>,
    pub scale: Vec<f32>,
    pub genre_temperature: f32,
    pub pair_temperature: f32,
    pub metrics: Metrics,
    /// What the model was trained on, as the caller fingerprints it (for
    /// example `(track id, label hash)`), so it can tell how many labels
    /// are new since. Filled by the caller; empty after [`train`].
    #[serde(default)]
    pub labels: Vec<(i64, u32)>,
    net: Net,
}

/// One class and its calibrated probability.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Candidate {
    pub genre: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subgenre: Option<String>,
    pub probability: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Prediction {
    pub genre: String,
    pub confidence: f32,
    /// The most likely subgenre within the predicted genre, if any is learned.
    pub subgenre: Option<String>,
    /// P(subgenre | genre).
    pub subgenre_confidence: f32,
    /// Top genres.
    pub candidates: Vec<Candidate>,
    /// Top genre/subgenre pairs.
    pub pair_candidates: Vec<Candidate>,
}

impl Prediction {
    pub fn level(&self) -> &'static str {
        confidence_level(self.confidence)
    }
}

/// "high", "medium" or "low".
pub fn confidence_level(confidence: f32) -> &'static str {
    if confidence >= HIGH_CONFIDENCE {
        "high"
    } else if confidence >= LOW_CONFIDENCE {
        "medium"
    } else {
        "low"
    }
}

#[derive(Debug, PartialEq)]
pub enum TrainError {
    /// Not enough usable verified tracks (count given).
    NotEnoughLabels(usize),
    /// Fewer than two genres to tell apart.
    NotEnoughGenres,
}

impl std::fmt::Display for TrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEnoughLabels(count) => write!(f, "not enough verified tracks with audio features ({count})"),
            Self::NotEnoughGenres => write!(f, "at least two verified genres are needed"),
        }
    }
}

impl std::error::Error for TrainError {}

struct Dataset {
    x: Vec<Vec<f32>>,
    genre: Vec<usize>,
    pair: Vec<Option<usize>>,
}

fn softmax(logits: &[f32], temperature: f32) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut out: Vec<f32> = logits.iter().map(|v| ((v - max) / temperature).exp()).collect();
    let sum: f32 = out.iter().sum();
    out.iter_mut().for_each(|v| *v /= sum);
    out
}

fn top(probabilities: &[f32], k: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..probabilities.len()).collect();
    order.sort_by(|a, b| probabilities[*b].total_cmp(&probabilities[*a]).then(a.cmp(b)));
    order.truncate(k);
    order
}

/// Temperature minimizing the negative log-likelihood (golden section).
fn fit_temperature(logits: &[Vec<f32>], labels: &[usize]) -> f32 {
    if logits.is_empty() {
        return 1.0;
    }
    let nll = |t: f32| -> f64 {
        logits.iter().zip(labels).map(|(row, label)| -(softmax(row, t)[*label].max(1e-12) as f64).ln()).sum()
    };
    let (mut lo, mut hi) = (0.25_f32, 8.0_f32);
    let ratio = 0.618_034_f32;
    for _ in 0..40 {
        let a = hi - ratio * (hi - lo);
        let b = lo + ratio * (hi - lo);
        if nll(a) < nll(b) {
            hi = b;
        } else {
            lo = a;
        }
    }
    (lo + hi) / 2.0
}

fn normalizer(rows: &[Vec<f32>]) -> (Vec<f32>, Vec<f32>) {
    let n = rows.len().max(1) as f64;
    let mut mean = vec![0.0f64; FEATURE_DIM];
    for row in rows {
        for (m, v) in mean.iter_mut().zip(row) {
            *m += *v as f64;
        }
    }
    mean.iter_mut().for_each(|m| *m /= n);
    let mut var = vec![0.0f64; FEATURE_DIM];
    for row in rows {
        for ((s, v), m) in var.iter_mut().zip(row).zip(&mean) {
            *s += (*v as f64 - m).powi(2);
        }
    }
    let scale = var.iter().map(|s| ((s / n).sqrt() as f32).max(1e-6)).collect();
    (mean.into_iter().map(|m| m as f32).collect(), scale)
}

fn normalize(row: &[f32], mean: &[f32], scale: &[f32]) -> Vec<f32> {
    row.iter().zip(mean).zip(scale).map(|((v, m), s)| (v - m) / s).collect()
}

/// Seconds since the Unix epoch.
fn unix_seconds() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Train on the verified tracks. `progress` receives 0..=1.
pub fn train(tracks: &[LabeledTrack], config: &TrainConfig, progress: &(dyn Fn(f32) + Sync)) -> Result<Model, TrainError> {
    // Usable rows and the label sets.
    let rows: Vec<(Vec<f32>, &str, &str)> = tracks
        .iter()
        .filter_map(|t| {
            let genre = t.genre.trim();
            if genre.is_empty() || genre.eq_ignore_ascii_case("unknown") {
                return None;
            }
            Some((t.features.vector()?, genre, t.subgenre.trim()))
        })
        .collect();
    let mut genre_counts: Vec<(&str, usize)> = Vec::new();
    for (_, genre, _) in &rows {
        match genre_counts.iter_mut().find(|(name, _)| name == genre) {
            Some(entry) => entry.1 += 1,
            None => genre_counts.push((genre, 1)),
        }
    }
    genre_counts.retain(|(_, count)| *count >= MIN_GENRE_SAMPLES);
    genre_counts.sort_by(|a, b| a.0.cmp(b.0));
    let genres: Vec<String> = genre_counts.iter().map(|(g, _)| (*g).to_owned()).collect();
    if genres.len() < 2 {
        return Err(TrainError::NotEnoughGenres);
    }
    let rows: Vec<_> = rows.into_iter().filter(|(_, g, _)| genres.iter().any(|name| name == g)).collect();
    if rows.len() < MIN_TRAINING_SAMPLES {
        return Err(TrainError::NotEnoughLabels(rows.len()));
    }
    let genre_index = |g: &str| genres.iter().position(|name| name == g).expect("kept genre");
    let mut pair_counts: Vec<((usize, &str), usize)> = Vec::new();
    for (_, genre, sub) in &rows {
        if sub.is_empty() {
            continue;
        }
        let key = (genre_index(genre), *sub);
        match pair_counts.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 += 1,
            None => pair_counts.push((key, 1)),
        }
    }
    pair_counts.retain(|(_, count)| *count >= MIN_SUBGENRE_SAMPLES);
    pair_counts.sort_by(|a, b| a.0.cmp(&b.0));
    let pairs: Vec<(usize, String)> = pair_counts.iter().map(|((g, s), _)| (*g, (*s).to_owned())).collect();
    let data = Dataset {
        x: rows.iter().map(|(x, _, _)| x.clone()).collect(),
        genre: rows.iter().map(|(_, g, _)| genre_index(g)).collect(),
        pair: rows
            .iter()
            .map(|(_, g, s)| {
                let g = genre_index(g);
                pairs.iter().position(|(pg, ps)| *pg == g && ps == s)
            })
            .collect(),
    };

    // Stratified split: 20 % of each genre (at least one when it has 5+).
    let mut rng = Rng::new(config.seed);
    let mut holdout = vec![false; data.x.len()];
    if config.holdout > 0.0 {
        for g in 0..genres.len() {
            let mut members: Vec<usize> = (0..data.x.len()).filter(|i| data.genre[*i] == g).collect();
            rng.shuffle(&mut members);
            let take = ((members.len() as f32 * config.holdout).round() as usize).max(1).min(members.len() - 1);
            for i in &members[..take] {
                holdout[*i] = true;
            }
        }
    }
    let train_idx: Vec<usize> = (0..data.x.len()).filter(|i| !holdout[*i]).collect();
    let test_idx: Vec<usize> = (0..data.x.len()).filter(|i| holdout[*i]).collect();
    let refit = config.refit && !test_idx.is_empty();
    let phases = if refit { 2.0 } else { 1.0 };

    // Phase 1: fit on the training split, measure on the held-out split.
    let fit = |idx: &[usize], rng: &mut Rng, phase: f32| -> (Net, Vec<f32>, Vec<f32>) {
        let subset: Vec<Vec<f32>> = idx.iter().map(|i| data.x[*i].clone()).collect();
        let (mean, scale) = normalizer(&subset);
        let x: Vec<Vec<f32>> = subset.iter().map(|row| normalize(row, &mean, &scale)).collect();
        let g: Vec<usize> = idx.iter().map(|i| data.genre[*i]).collect();
        let p: Vec<Option<usize>> = idx.iter().map(|i| data.pair[*i]).collect();
        let net = Net::train(&x, &g, &p, genres.len(), pairs.len(), config, rng, &|f| progress((phase + f) / phases));
        (net, mean, scale)
    };
    let (net, mean, scale) = fit(&train_idx, &mut rng, 0.0);
    let mut metrics = Metrics { samples: data.x.len(), train_samples: train_idx.len(), holdout_samples: test_idx.len(), ..Metrics::default() };
    let (mut genre_t, mut pair_t) = (1.0, 1.0);
    if !test_idx.is_empty() {
        let outputs: Vec<(Vec<f32>, Vec<f32>)> = test_idx.iter().map(|i| net.forward(&normalize(&data.x[*i], &mean, &scale))).collect();
        let g_logits: Vec<Vec<f32>> = outputs.iter().map(|(g, _)| g.clone()).collect();
        let g_labels: Vec<usize> = test_idx.iter().map(|i| data.genre[*i]).collect();
        genre_t = fit_temperature(&g_logits, &g_labels);
        let (p_logits, p_labels): (Vec<Vec<f32>>, Vec<usize>) = outputs
            .iter()
            .zip(&test_idx)
            .filter_map(|((_, p), i)| Some((p.clone(), data.pair[*i]?)))
            .unzip();
        if !pairs.is_empty() {
            pair_t = fit_temperature(&p_logits, &p_labels);
        }
        metrics = evaluate(&net, &genres, &pairs, &data, &test_idx, &mean, &scale, genre_t, pair_t, metrics);
    }

    // Phase 2: refit on every label for the shipped model.
    let (net, mean, scale) = if refit {
        let all: Vec<usize> = (0..data.x.len()).collect();
        fit(&all, &mut rng, 1.0)
    } else {
        (net, mean, scale)
    };
    progress(1.0);
    Ok(Model { version: MODEL_VERSION, trained_at: unix_seconds(), genres, pairs, mean, scale, genre_temperature: genre_t, pair_temperature: pair_t, metrics, labels: Vec::new(), net })
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    net: &Net,
    genres: &[String],
    pairs: &[(usize, String)],
    data: &Dataset,
    idx: &[usize],
    mean: &[f32],
    scale: &[f32],
    genre_t: f32,
    pair_t: f32,
    mut metrics: Metrics,
) -> Metrics {
    let n = idx.len() as f32;
    let (mut correct, mut top3, mut high, mut high_correct, mut low) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut sub_total, mut sub_correct) = (0usize, 0usize);
    let mut per = vec![(0usize, 0usize); genres.len()];
    let mut bins = vec![(0usize, 0.0f32, 0usize); 10];
    for i in idx {
        let (g_logits, p_logits) = net.forward(&normalize(&data.x[*i], mean, scale));
        let probs = softmax(&g_logits, genre_t);
        let order = top(&probs, 3);
        let truth = data.genre[*i];
        let best = order[0];
        let confidence = probs[best];
        let hit = best == truth;
        correct += usize::from(hit);
        top3 += usize::from(order.contains(&truth));
        per[truth].0 += 1;
        per[truth].1 += usize::from(hit);
        if confidence >= HIGH_CONFIDENCE {
            high += 1;
            high_correct += usize::from(hit);
        }
        if confidence < LOW_CONFIDENCE {
            low += 1;
        }
        let bin = ((confidence * 10.0) as usize).min(9);
        bins[bin].0 += 1;
        bins[bin].1 += confidence;
        bins[bin].2 += usize::from(hit);
        if let Some(pair) = data.pair[*i] {
            sub_total += 1;
            let predicted = best_pair(&p_logits, pair_t, pairs, best);
            sub_correct += usize::from(predicted.map(|(p, _)| p) == Some(pair));
        }
    }
    let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f32 / b as f32 };
    metrics.genre_accuracy = ratio(correct, idx.len());
    metrics.genre_top3_accuracy = ratio(top3, idx.len());
    metrics.subgenre_accuracy = ratio(sub_correct, sub_total);
    metrics.high_confidence_coverage = ratio(high, idx.len());
    metrics.high_confidence_accuracy = ratio(high_correct, high);
    metrics.low_confidence_share = ratio(low, idx.len());
    metrics.calibration_error = bins
        .iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, conf, hits)| (*count as f32 / n) * (conf / *count as f32 - *hits as f32 / *count as f32).abs())
        .sum();
    metrics.per_genre = genres
        .iter()
        .zip(&per)
        .map(|(genre, (support, hits))| GenreMetric { genre: genre.clone(), support: *support, recall: ratio(*hits, *support) })
        .collect();
    metrics
}

/// The most likely pair within `genre` and P(pair | genre).
fn best_pair(pair_logits: &[f32], temperature: f32, pairs: &[(usize, String)], genre: usize) -> Option<(usize, f32)> {
    if pairs.is_empty() {
        return None;
    }
    let probs = softmax(pair_logits, temperature);
    let within: Vec<usize> = (0..pairs.len()).filter(|p| pairs[*p].0 == genre).collect();
    let total: f32 = within.iter().map(|p| probs[*p]).sum();
    let best = within.into_iter().max_by(|a, b| probs[*a].total_cmp(&probs[*b]).then(b.cmp(a)))?;
    Some((best, if total > 0.0 { probs[best] / total } else { 0.0 }))
}

impl Model {
    /// Calibrated prediction, or `None` without a usable embedding.
    pub fn predict(&self, features: &AudioFeatures) -> Option<Prediction> {
        let x = normalize(&features.vector()?, &self.mean, &self.scale);
        let (g_logits, p_logits) = self.net.forward(&x);
        let probs = softmax(&g_logits, self.genre_temperature);
        let order = top(&probs, 5);
        let best = order[0];
        let pair = best_pair(&p_logits, self.pair_temperature, &self.pairs, best);
        let pair_probs = if self.pairs.is_empty() { Vec::new() } else { softmax(&p_logits, self.pair_temperature) };
        Some(Prediction {
            genre: self.genres[best].clone(),
            confidence: probs[best],
            subgenre: pair.map(|(p, _)| self.pairs[p].1.clone()),
            subgenre_confidence: pair.map_or(0.0, |(_, c)| c),
            candidates: order
                .iter()
                .map(|g| Candidate { genre: self.genres[*g].clone(), subgenre: None, probability: probs[*g] })
                .collect(),
            pair_candidates: top(&pair_probs, 5)
                .into_iter()
                .map(|p| Candidate {
                    genre: self.genres[self.pairs[p].0].clone(),
                    subgenre: Some(self.pairs[p].1.clone()),
                    probability: pair_probs[p],
                })
                .collect(),
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("model serializes")
    }

    /// `None` for an unreadable or older-format file.
    pub fn from_json(text: &str) -> Option<Self> {
        let model: Self = serde_json::from_str(text).ok()?;
        (model.version == MODEL_VERSION && model.mean.len() == FEATURE_DIM && model.net.valid(FEATURE_DIM, model.genres.len(), model.pairs.len()))
            .then_some(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three separable "genres" in embedding space, BPM as a hint.
    fn synthetic(n: usize, seed: u64) -> Vec<LabeledTrack> {
        let mut rng = Rng::new(seed);
        let genres = [("House", "Deep House", 124.0), ("Hip Hop", "Trap", 90.0), ("Drum & Bass", "Liquid", 174.0)];
        (0..n)
            .map(|i| {
                let (genre, sub, bpm) = genres[i % 3];
                let embedding = (0..EMBEDDING_DIM)
                    .map(|d| if d % 3 == i % 3 { 1.0 } else { 0.0 } + rng.normal() * 0.6)
                    .collect();
                LabeledTrack {
                    features: AudioFeatures { embedding, brightness: Some(0.2), loudness: Some(-9.0), contrast: Some(0.3), noisiness: Some(0.4), bpm: Some(bpm + rng.normal() * 2.0) },
                    genre: genre.into(),
                    subgenre: sub.into(),
                }
            })
            .collect()
    }

    fn quick() -> TrainConfig {
        TrainConfig { epochs: 30, hidden: 16, batch: 32, ..TrainConfig::default() }
    }

    #[test]
    fn learns_separable_genres_and_round_trips() {
        let tracks = synthetic(300, 7);
        let model = train(&tracks, &quick(), &|_| {}).unwrap();
        assert_eq!(model.genres, ["Drum & Bass", "Hip Hop", "House"]);
        assert!(model.metrics.genre_accuracy > 0.9, "{:?}", model.metrics);
        assert!(model.metrics.holdout_samples >= 59 && model.metrics.holdout_samples <= 61);
        let prediction = model.predict(&tracks[0].features).unwrap();
        assert_eq!(prediction.genre, "House");
        assert_eq!(prediction.subgenre.as_deref(), Some("Deep House"));
        assert!(prediction.confidence > 0.5 && prediction.confidence <= 1.0);
        assert_eq!(prediction.candidates.len(), 3);
        let restored = Model::from_json(&model.to_json()).unwrap();
        assert_eq!(restored.predict(&tracks[0].features), Some(prediction));
    }

    #[test]
    fn refuses_too_little_data_and_bad_features() {
        let tracks = synthetic(15, 1);
        assert_eq!(train(&tracks, &quick(), &|_| {}).unwrap_err(), TrainError::NotEnoughLabels(15));
        let mut one = synthetic(60, 2);
        one.iter_mut().for_each(|t| t.genre = "House".into());
        assert_eq!(train(&one, &quick(), &|_| {}).unwrap_err(), TrainError::NotEnoughGenres);
        let features = AudioFeatures { embedding: vec![0.0; 3], brightness: None, loudness: None, contrast: None, noisiness: None, bpm: None };
        assert_eq!(features.vector(), None);
    }

    #[test]
    fn unknown_labels_are_not_learned() {
        let mut tracks = synthetic(90, 3);
        tracks.iter_mut().take(30).for_each(|t| t.genre = "Unknown".into());
        let model = train(&tracks, &quick(), &|_| {}).unwrap();
        assert!(!model.genres.iter().any(|g| g == "Unknown"));
        assert_eq!(model.metrics.samples, 60);
    }

    #[test]
    fn confidence_levels() {
        assert_eq!(confidence_level(0.9), "high");
        assert_eq!(confidence_level(0.6), "medium");
        assert_eq!(confidence_level(0.2), "low");
    }
}
