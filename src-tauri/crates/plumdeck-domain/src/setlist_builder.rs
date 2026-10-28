//! Setlist Auto and the path builder (backend/domain/services/setlist_builder.py).
//!
//! Both are greedy: each step takes the best-scoring unused candidate. The
//! builders return the chosen nodes' indexes; the caller owns the tracks.

use std::collections::BTreeSet;

use plumdeck_core::compat::text::{py_lower, py_strip};

use crate::mixability::{bpm_distance, cosine_similarity, mixability_score, SET_TRANSITION_WEIGHTS};
use crate::targets::Targets;

/// `SAME_ARTIST_PENALTY`.
pub const SAME_ARTIST_PENALTY: f64 = 0.2;
/// `APPROVED_WORDPLAY_BONUS`.
pub const APPROVED_WORDPLAY_BONUS: f64 = 0.25;

/// One candidate or seed: what the builders read of its track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub id: i64,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
    pub vector: Option<Vec<f64>>,
    pub embedding_model: Option<String>,
    /// `wordplay_only_from_ids`: a target in the pool only because of an
    /// approved edge, eligible only right after one of these sources.
    pub wordplay_only_from: Option<BTreeSet<i64>>,
}

impl Node {
    fn feature(&self, name: &str) -> Option<f64> {
        match name {
            "energy" => self.energy,
            "danceability" => self.danceability,
            "brightness" => self.brightness,
            "noisiness" => self.noisiness,
            _ => None,
        }
    }

    fn eligible_after(&self, current: i64) -> bool {
        self.wordplay_only_from.as_ref().is_none_or(|sources| sources.is_empty() || sources.contains(&current))
    }
}

/// A Python `TypeError` the builder would raise (arithmetic on a missing
/// BPM or energy): the route answers 500.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingValue;

/// Approved wordplay edges `(from, to)`.
pub type Edges = BTreeSet<(i64, i64)>;

/// Where a chosen node lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Seed(usize),
    Pool(usize),
    Start,
    End,
}

fn transition(current: &Node, candidate: &Node) -> f64 {
    let similarity = cosine_similarity(
        current.vector.as_deref(),
        candidate.vector.as_deref(),
        current.embedding_model.as_deref(),
        candidate.embedding_model.as_deref(),
    );
    mixability_score(current.bpm, candidate.bpm, similarity, SET_TRANSITION_WEIGHTS)
}

fn normalized_artist(node: &Node) -> String {
    py_lower(py_strip(node.artist.as_deref().unwrap_or("")))
}

/// `build_chain`: seeds first (in order, without repeats, up to the
/// length), else the pool node closest to the targets; then the best next
/// track until the length is reached or nothing fits.
pub fn build_chain(pool: &[Node], seeds: &[Node], target_length: i64, targets: &Targets, edges: &Edges) -> Vec<Pick> {
    if target_length <= 0 || (pool.is_empty() && seeds.is_empty()) {
        return Vec::new();
    }
    let length = usize::try_from(target_length).unwrap_or(usize::MAX);
    let mut chain: Vec<(Pick, &Node)> = Vec::new();
    let mut used = BTreeSet::new();
    for (index, seed) in seeds.iter().enumerate() {
        if used.contains(&seed.id) {
            continue;
        }
        if chain.len() >= length {
            break;
        }
        chain.push((Pick::Seed(index), seed));
        used.insert(seed.id);
    }
    if chain.is_empty() {
        let start_score = |node: &Node| {
            let mut score = 0.0;
            for (name, target) in targets.features() {
                if let Some(target) = target {
                    score -= node.feature(name).map_or(1.0, |value| (value - target).abs());
                }
            }
            if let Some(bpm) = targets.bpm {
                score -= bpm_distance(Some(bpm), node.bpm);
            }
            score
        };
        // min(starters, key=(-score, id)): the first of the smallest key.
        let mut best: Option<(usize, f64)> = None;
        for (index, node) in pool.iter().enumerate() {
            if node.wordplay_only_from.as_ref().is_some_and(|sources| !sources.is_empty()) {
                continue;
            }
            let key = -start_score(node);
            let better = match best {
                None => true,
                Some((at, best_key)) => key < best_key || (key == best_key && node.id < pool[at].id),
            };
            if better {
                best = Some((index, key));
            }
        }
        let Some((index, _)) = best else { return Vec::new() };
        chain.push((Pick::Pool(index), &pool[index]));
        used.insert(pool[index].id);
    }
    while chain.len() < length {
        let current = chain.last().expect("chain has a start").1;
        let mut best: Option<usize> = None;
        let mut best_score = -999.0;
        for (index, candidate) in pool.iter().enumerate() {
            if used.contains(&candidate.id) || !candidate.eligible_after(current.id) {
                continue;
            }
            let mix = transition(current, candidate);
            let mut vibe = 0.0;
            for (name, target) in targets.features() {
                if let Some(target) = target {
                    let value = candidate.feature(name).filter(|value| *value != 0.0).unwrap_or(0.0);
                    vibe -= (value - target).abs() * 0.1;
                }
            }
            let current_artist = normalized_artist(current);
            let penalty = if !current_artist.is_empty() && current_artist == normalized_artist(candidate) {
                SAME_ARTIST_PENALTY
            } else {
                0.0
            };
            let bonus = if edges.contains(&(current.id, candidate.id)) { APPROVED_WORDPLAY_BONUS } else { 0.0 };
            let total = mix + vibe - penalty + bonus;
            let better = total > best_score || (total == best_score && best.is_some_and(|at| candidate.id < pool[at].id));
            if better {
                best_score = total;
                best = Some(index);
            }
        }
        let Some(index) = best else { break };
        chain.push((Pick::Pool(index), &pool[index]));
        used.insert(pool[index].id);
    }
    chain.into_iter().map(|(pick, _)| pick).collect()
}

/// `build_path`: `steps - 2` intermediate tracks between `start` and `end`,
/// each scored on the transition from the previous one, the similarity to
/// `end` and the distance to the interpolated BPM and energy.
pub fn build_path(pool: &[Node], start: &Node, end: &Node, steps: i64, edges: &Edges) -> Result<Vec<Pick>, MissingValue> {
    let mut chain = vec![Pick::Start];
    let mut used: BTreeSet<i64> = [start.id, end.id].into_iter().collect();
    let mut current = start;
    let intermediate = (steps - 2).max(0);
    for step in 0..intermediate {
        let progress = (step + 1) as f64 / (intermediate + 1) as f64;
        let (start_bpm, end_bpm) = (start.bpm.ok_or(MissingValue)?, end.bpm.ok_or(MissingValue)?);
        let target_bpm = start_bpm + (end_bpm - start_bpm) * progress;
        let (start_energy, end_energy) = (start.energy.ok_or(MissingValue)?, end.energy.ok_or(MissingValue)?);
        let target_energy = start_energy + (end_energy - start_energy) * progress;
        let mut best: Option<usize> = None;
        let mut best_score = -999.0;
        for (index, candidate) in pool.iter().enumerate() {
            if used.contains(&candidate.id) || !candidate.eligible_after(current.id) {
                continue;
            }
            let mix = transition(current, candidate);
            let goal = cosine_similarity(
                candidate.vector.as_deref(),
                end.vector.as_deref(),
                candidate.embedding_model.as_deref(),
                end.embedding_model.as_deref(),
            );
            let bpm = candidate.bpm.ok_or(MissingValue)?;
            let mut param = 0.0;
            if bpm > 0.0 {
                param -= (bpm - target_bpm).abs() * 0.01;
            }
            param -= (candidate.energy.ok_or(MissingValue)? - target_energy).abs();
            let mut total = mix * 1.5 + param * 0.5;
            match goal {
                None => total *= 3.0 / 2.0,
                Some(similarity) => total += similarity,
            }
            if edges.contains(&(current.id, candidate.id)) {
                total += APPROVED_WORDPLAY_BONUS;
            }
            if total > best_score {
                best_score = total;
                best = Some(index);
            }
        }
        let Some(index) = best else { break };
        chain.push(Pick::Pool(index));
        used.insert(pool[index].id);
        current = &pool[index];
    }
    chain.push(Pick::End);
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, bpm: f64, artist: &str) -> Node {
        Node { id, artist: Some(artist.into()), bpm: Some(bpm), energy: Some(0.5), ..Node::default() }
    }

    #[test]
    fn chain_follows_tempo_and_avoids_repeating_an_artist() {
        let pool = vec![node(3, 124.0, "A"), node(1, 124.0, "B"), node(2, 125.0, "A"), node(4, 60.0, "C")];
        let picks = build_chain(&pool, &[], 3, &Targets::default(), &Edges::new());
        // Start: every score is 0, so the smallest id; then the closest
        // tempo, then a half-time track over the same artist (penalty).
        assert_eq!(picks, vec![Pick::Pool(1), Pick::Pool(0), Pick::Pool(3)]);
        let seeds = vec![node(9, 124.0, "Z"), node(9, 124.0, "Z")];
        let picks = build_chain(&pool, &seeds, 2, &Targets::default(), &Edges::new());
        assert_eq!(picks[0], Pick::Seed(0));
        assert_eq!(build_chain(&pool, &[], 0, &Targets::default(), &Edges::new()), vec![]);
    }

    #[test]
    fn wordplay_only_targets_follow_their_source() {
        let mut target = node(5, 60.0, "W");
        target.wordplay_only_from = Some([1].into_iter().collect());
        let pool = vec![node(1, 124.0, "B"), target, node(2, 124.0, "C")];
        let edges: Edges = [(1, 5)].into_iter().collect();
        let picks = build_chain(&pool, &[], 2, &Targets::default(), &edges);
        // The bonus outweighs the half-time reading.
        assert_eq!(picks, vec![Pick::Pool(0), Pick::Pool(1)]);
    }

    #[test]
    fn path_needs_bpm_and_energy() {
        let pool = vec![node(2, 125.0, "B")];
        let start = node(1, 120.0, "A");
        let end = node(3, 130.0, "C");
        assert_eq!(build_path(&pool, &start, &end, 3, &Edges::new()), Ok(vec![Pick::Start, Pick::Pool(0), Pick::End]));
        assert_eq!(build_path(&pool, &start, &end, 2, &Edges::new()), Ok(vec![Pick::Start, Pick::End]));
        let silent = Node { bpm: None, ..node(1, 0.0, "A") };
        assert_eq!(build_path(&pool, &silent, &end, 3, &Edges::new()), Err(MissingValue));
    }
}
