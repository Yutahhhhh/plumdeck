//! Transition scoring of the legacy recommenders and set builders
//! (backend/utils/audio_math.py `bpm_distance`, `calculate_mixability_score`
//! and backend/utils/embedding.py `cosine_similarity`).
//!
//! The key component carries no weight in either weight set (owner
//! decision), so only tempo and the embedding similarity contribute; a
//! missing similarity is left out and the remaining weights renormalized.
//!
//! `cosine_similarity` sums left to right. numpy's `dot` goes through BLAS,
//! whose summation order differs, so on arbitrary embeddings the last bit of
//! a similarity can differ from Python's; on vectors whose partial sums are
//! exact (the parity fixtures) both agree bit for bit.

/// Tempo and embedding weights (`DEFAULT_MIXABILITY_WEIGHTS`, `SET_TRANSITION_WEIGHTS`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    pub bpm: f64,
    pub vector: f64,
}

/// `DEFAULT_MIXABILITY_WEIGHTS`: the next-track recommender.
pub const DEFAULT_WEIGHTS: Weights = Weights { bpm: 0.35, vector: 0.4 };
/// `SET_TRANSITION_WEIGHTS`: Setlist Auto and the path builder.
pub const SET_TRANSITION_WEIGHTS: Weights = Weights { bpm: 0.4, vector: 0.3 };

/// `bpm_distance`: octaves between two tempos, allowing one half/double-time
/// reading; infinite when either is unknown, non-finite or not positive.
pub fn bpm_distance(target: Option<f64>, candidate: Option<f64>) -> f64 {
    let (Some(target), Some(candidate)) = (target, candidate) else { return f64::INFINITY };
    if !target.is_finite() || !candidate.is_finite() || target.min(candidate) <= 0.0 {
        return f64::INFINITY;
    }
    let ratio = candidate.log2() - target.log2();
    [-1.0, 0.0, 1.0].iter().map(|shift| (ratio + shift).abs()).fold(f64::INFINITY, f64::min)
}

/// `calculate_mixability_score` (0..=1).
pub fn mixability_score(target_bpm: Option<f64>, candidate_bpm: Option<f64>, vector_similarity: Option<f64>, weights: Weights) -> f64 {
    let distance = bpm_distance(target_bpm, candidate_bpm);
    let bpm_score = (-0.5 * (distance / 1.08f64.log2()).powi(2)).exp();
    // The key component is always present with weight 0.0.
    let mut total = bpm_score * weights.bpm + 0.0;
    let mut available = weights.bpm + 0.0;
    if let Some(similarity) = vector_similarity.filter(|value| value.is_finite()) {
        total += similarity.clamp(0.0, 1.0) * weights.vector;
        available += weights.vector;
    }
    if available > 0.0 {
        total / available
    } else {
        0.0
    }
}

/// `LEGACY_MODELS`: names that all mean the original MusiCNN space.
pub const LEGACY_MODELS: [&str; 3] = ["musicnn", "msd-musicnn-1", ""];

/// `embedding_space`: `None` and the legacy names are one space.
pub fn embedding_space(model: Option<&str>) -> &str {
    match model {
        None => "musicnn",
        Some(name) if LEGACY_MODELS.contains(&name) => "musicnn",
        Some(name) => name,
    }
}

/// `cosine_similarity`: `None` when there is no comparable evidence
/// (different spaces, a missing, empty, mismatched or non-finite vector, or
/// a zero norm), else the clipped cosine.
pub fn cosine_similarity(left: Option<&[f64]>, right: Option<&[f64]>, left_model: Option<&str>, right_model: Option<&str>) -> Option<f64> {
    if embedding_space(left_model) != embedding_space(right_model) {
        return None;
    }
    let (left, right) = (left?, right?);
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    if !left.iter().chain(right).all(|value| value.is_finite()) {
        return None;
    }
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).fold(0.0, |sum, (x, y)| sum + x * y);
    let denominator = dot(left, left).sqrt() * dot(right, right).sqrt();
    if !denominator.is_finite() || denominator <= 0.0 {
        return None;
    }
    let cosine = dot(left, right) / denominator;
    // np.clip keeps NaN.
    Some(if cosine.is_nan() { cosine } else { cosine.clamp(-1.0, 1.0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_distance_reads_half_and_double_time() {
        assert_eq!(bpm_distance(Some(120.0), Some(120.0)), 0.0);
        assert_eq!(bpm_distance(Some(87.0), Some(174.0)), 0.0);
        assert_eq!(bpm_distance(Some(120.0), None), f64::INFINITY);
        assert_eq!(bpm_distance(Some(0.0), Some(120.0)), f64::INFINITY);
        assert!((bpm_distance(Some(120.0), Some(126.0)) - (126f64.log2() - 120f64.log2())).abs() < 1e-15);
    }

    #[test]
    fn missing_similarity_renormalizes() {
        assert_eq!(mixability_score(Some(124.0), Some(124.0), None, DEFAULT_WEIGHTS), 1.0);
        assert_eq!(mixability_score(Some(124.0), None, None, DEFAULT_WEIGHTS), 0.0);
        let with = mixability_score(Some(124.0), Some(124.0), Some(0.5), DEFAULT_WEIGHTS);
        assert_eq!(with, 0.7333333333333334);
        assert_eq!(mixability_score(Some(124.0), Some(124.0), Some(2.0), SET_TRANSITION_WEIGHTS), 1.0);
    }

    #[test]
    fn similarity_needs_comparable_vectors() {
        let a = [1.0, 0.0, 1.0];
        let b = [1.0, 1.0, 0.0];
        assert_eq!(cosine_similarity(Some(&a), Some(&b), None, Some("musicnn")), Some(0.4999999999999999));
        assert_eq!(cosine_similarity(Some(&a), Some(&b), None, Some("other")), None);
        assert_eq!(cosine_similarity(Some(&a), Some(&[0.0, 0.0, 0.0]), None, None), None);
        assert_eq!(cosine_similarity(Some(&a), Some(&[1.0]), None, None), None);
        assert_eq!(cosine_similarity(Some(&[]), Some(&[]), None, None), None);
        assert_eq!(embedding_space(Some("")), "musicnn");
    }
}

/// `_parse_embedding`: the stored JSON as a vector, or `None` unless it is
/// a non-empty one-dimensional array of finite numbers (`np.array(...,
/// dtype=float)` also converts booleans and numeric strings).
pub fn parse_embedding(text: Option<&str>) -> Option<Vec<f64>> {
    use plumdeck_core::compat::pyfmt::py_float_from_str;
    use plumdeck_core::compat::pyjson::PyValue;
    use plumdeck_core::compat::pyjson_loads::loads;

    let text = text.filter(|text| !text.is_empty())?;
    let PyValue::List(items) = loads(text).ok()? else { return None };
    let vector = items
        .iter()
        .map(|item| match item {
            PyValue::Bool(flag) => Some(f64::from(u8::from(*flag))),
            PyValue::Int(number) => Some(*number as f64),
            PyValue::Float(number) => Some(*number),
            PyValue::Str(text) => py_float_from_str(text),
            _ => None,
        })
        .collect::<Option<Vec<f64>>>()?;
    (!vector.is_empty() && vector.iter().all(|value| value.is_finite())).then_some(vector)
}

#[cfg(test)]
mod parse_tests {
    use super::parse_embedding;

    #[test]
    fn parses_like_numpy() {
        assert_eq!(parse_embedding(Some("[1, 0.5, true, \" 2 \"]")), Some(vec![1.0, 0.5, 1.0, 2.0]));
        assert_eq!(parse_embedding(Some("[]")), None);
        assert_eq!(parse_embedding(Some("[[1]]")), None);
        assert_eq!(parse_embedding(Some("[NaN]")), None);
        assert_eq!(parse_embedding(Some("[null]")), None);
        assert_eq!(parse_embedding(Some("not json")), None);
        assert_eq!(parse_embedding(Some("5")), None);
        assert_eq!(parse_embedding(None), None);
    }
}
