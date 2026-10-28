//! Assist-mode scoring (backend/domain/services/assist_recommendation.py),
//! compound filters (assist_filters.py) and the DJ's play history as
//! evidence (play_history.py); S6.
//!
//! Every user-facing text is Python's, formatted with Python's `:g`,
//! `:+.1f` and `:+.2f`.

use std::collections::{BTreeMap, HashMap};

use plumdeck_core::compat::pyfmt::format_float;
use plumdeck_core::compat::text::{casefold, nfkc, py_split, py_strip};
use plumdeck_key::normalize_key;
use regex::Regex;

use crate::mixability::bpm_distance;

fn g(x: f64) -> String {
    format_float(x, "g").unwrap_or_else(|_| x.to_string())
}

fn signed(x: f64, places: usize) -> String {
    format_float(x, &format!("+.{places}f")).unwrap_or_else(|_| x.to_string())
}

// ------------------------------------------------------------------ presets

/// A preset's feature direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Hold,
    NotDown,
    NotUp,
}

/// One preset (`Preset`).
#[derive(Clone, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    pub tempo_window: (f64, f64),
    pub tempo_center: f64,
    pub directions: Vec<(&'static str, Direction)>,
    pub weights: Vec<(&'static str, f64)>,
}

impl Preset {
    pub fn weight(&self, name: &str) -> Option<f64> {
        self.weights.iter().find(|(n, _)| *n == name).map(|(_, w)| *w)
    }
}

use Direction::*;

/// `PRESETS`, in Python's dict order.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "keep", label: "キープ", detail: "今の流れを保つ", tempo_window: (-6.0, 6.0), tempo_center: 0.0,
            directions: vec![("energy", Hold), ("danceability", Hold), ("noisiness", Hold)],
            weights: vec![("bpm", 0.30), ("vector", 0.22), ("continuity", 0.22), ("era_proximity", 0.10), ("energy", 0.06), ("danceability", 0.05), ("noisiness", 0.05)],
        },
        Preset {
            name: "hype", label: "盛り上げる", detail: "熱量を上げる", tempo_window: (-2.0, 8.0), tempo_center: 0.0,
            directions: vec![("energy", Up), ("noisiness", NotDown)],
            weights: vec![("bpm", 0.37), ("vector", 0.06), ("continuity", 0.04), ("era_proximity", 0.05), ("energy", 0.35), ("noisiness", 0.13)],
        },
        Preset {
            name: "dance", label: "踊らせる", detail: "体が動くグルーヴへ", tempo_window: (-6.0, 6.0), tempo_center: 0.0,
            directions: vec![("danceability", Up), ("energy", NotDown)],
            weights: vec![("bpm", 0.25), ("vector", 0.12), ("continuity", 0.09), ("era_proximity", 0.05), ("danceability", 0.35), ("energy", 0.14)],
        },
        Preset {
            name: "calm", label: "落ち着かせる", detail: "クールダウン", tempo_window: (-8.0, 2.0), tempo_center: 0.0,
            directions: vec![("energy", Down), ("noisiness", Down)],
            weights: vec![("bpm", 0.33), ("vector", 0.07), ("continuity", 0.08), ("era_proximity", 0.05), ("energy", 0.32), ("noisiness", 0.15)],
        },
        Preset {
            name: "emotional", label: "エモく", detail: "切なく・深く", tempo_window: (-6.0, 6.0), tempo_center: 0.0,
            directions: vec![("brightness", Down), ("energy", Hold)],
            weights: vec![("bpm", 0.17), ("key", 0.22), ("vector", 0.08), ("continuity", 0.08), ("era_proximity", 0.05), ("brightness", 0.21), ("energy", 0.19)],
        },
        Preset {
            name: "bright", label: "明るく", detail: "開放感・多幸感へ", tempo_window: (-6.0, 6.0), tempo_center: 0.0,
            directions: vec![("brightness", Up), ("energy", Hold)],
            weights: vec![("bpm", 0.17), ("key", 0.22), ("vector", 0.08), ("continuity", 0.08), ("era_proximity", 0.05), ("brightness", 0.21), ("energy", 0.19)],
        },
        Preset {
            name: "throwback", label: "時代を戻す", detail: "懐かしい曲で沸かせる", tempo_window: (-6.0, 6.0), tempo_center: 0.0,
            directions: vec![("energy", Hold), ("danceability", Hold)],
            weights: vec![("bpm", 0.27), ("vector", 0.12), ("continuity", 0.12), ("era", 0.30), ("energy", 0.10), ("danceability", 0.09)],
        },
        Preset {
            name: "wordplay", label: "ワードプレイ", detail: "言葉でつなぐ", tempo_window: (-100.0, 100.0), tempo_center: 0.0,
            directions: vec![("energy", Hold)],
            weights: vec![("wordplay", 0.40), ("bpm", 0.24), ("key", 0.18), ("vector", 0.08), ("energy", 0.10)],
        },
    ]
}

/// The preset of a (resolved) intent.
pub fn preset(intent: &str) -> Preset {
    presets().into_iter().find(|p| p.name == intent).expect("resolved intent")
}

/// `MOOD_PRESETS`.
pub const MOOD_PRESETS: [&str; 7] = ["keep", "hype", "dance", "calm", "emotional", "bright", "throwback"];

/// `resolve_intent`: `groove` is `keep`; `Err` names an unknown intent.
pub fn resolve_intent(intent: &str) -> Result<&'static str, String> {
    let intent = if intent == "groove" { "keep" } else { intent };
    presets().iter().map(|p| p.name).find(|name| *name == intent).ok_or_else(|| format!("unknown intent: {intent}"))
}

pub fn feature_step(feature: &str) -> f64 {
    match feature {
        "energy" => 0.15,
        "danceability" | "brightness" => 0.06,
        _ => 0.10,
    }
}

pub fn feature_label(feature: &str) -> &'static str {
    match feature {
        "energy" => "エネルギー",
        "danceability" => "ダンサビリティ",
        "brightness" => "明るさ",
        _ => "ノイズ感",
    }
}

pub fn component_label(name: &str) -> &'static str {
    match name {
        "bpm" => "テンポ",
        "key" => "キー",
        "vector" => "音色",
        "era" => "年代",
        "era_proximity" => "年代の近さ",
        "continuity" => "系統の連続性",
        "wordplay" => "ワードプレイ",
        other => feature_label(other),
    }
}

pub const THROWBACK_MIN_YEARS: f64 = 5.0;
pub const TEMPO_SIGMA_PERCENT: f64 = 3.0;
pub const HALF_DOUBLE_TEMPO_FACTOR: f64 = 0.72;
pub fn transition_tolerance() -> f64 {
    1.04f64.log2()
}

pub const TRANSITION_CAVEATS: [&str; 3] = [
    "トランジション曲はタイトルの「100-128」などの表記から入りと出口の BPM を読み取っています。表記の誤りや、表記のない曲は判定できません",
    "テンポが変わる曲の解析 BPM・キー・エネルギーは曲の一部分の値で、入りや出口と一致しない場合があります。rekordbox のビートグリッドも途中でずれることがあるため、試聴して確認してください",
    "倍テン・ハーフテンは BPM の比率だけで判定しています。実際に半分・倍のノリに聞こえるかは試聴で確認してください",
];

pub const TIMBRE_FLOOR: f64 = 0.55;
pub const TIMBRE_CEILING: f64 = 0.95;
pub const DEFAULT_HISTORY_WEIGHT: f64 = 0.25;
pub const HISTORY_TEMPO_GATE_PERCENT: f64 = 3.0;
pub const HISTORY_ENERGY_GATE: f64 = 0.2;
pub const STRENGTH_THRESHOLD: f64 = 0.7;
pub const WEAKNESS_THRESHOLD: f64 = 0.4;
pub const REFERENCE_BLEND: f64 = 0.55;
pub const LIKE_THRESHOLD: f64 = 0.7;
pub const ERA_SPAN_YEARS: f64 = 10.0;
const FEEL_FEATURES: [&str; 3] = ["energy", "danceability", "noisiness"];

pub fn likeness_weight(name: &str) -> f64 {
    match name {
        "timbre" => 0.50,
        "feel" => 0.25,
        "genre" => 0.15,
        _ => 0.10,
    }
}

pub fn likeness_label(name: &str) -> &'static str {
    match name {
        "timbre" => "音色",
        "feel" => "ノリ",
        "genre" => "ジャンル",
        _ => "年代",
    }
}

pub fn history_weight(intent: &str) -> f64 {
    match intent {
        "keep" => 0.45,
        "wordplay" => 0.0,
        _ => DEFAULT_HISTORY_WEIGHT,
    }
}

// ------------------------------------------------------------------ track facts

/// The track fields scoring reads (`_track_payload`'s values).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub year: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
}

impl Facts {
    pub fn feature(&self, name: &str) -> Option<f64> {
        let value = match name {
            "energy" => self.energy,
            "danceability" => self.danceability,
            "brightness" => self.brightness,
            "noisiness" => self.noisiness,
            _ => None,
        };
        value.filter(|v| v.is_finite())
    }
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

// ------------------------------------------------------------------ tempo

/// `matched_tempo`: (effective BPM, signed percent, octave shift).
pub fn matched_tempo(source: Option<f64>, candidate: Option<f64>) -> Option<(f64, f64, i32)> {
    let (source, candidate) = (positive(source)?, positive(candidate)?);
    let ratio = candidate.log2() - source.log2();
    let mut shift = 0;
    let mut best = ratio.abs();
    for value in [-1i32, 1] {
        let distance = (ratio + f64::from(value)).abs();
        if distance < best {
            best = distance;
            shift = value;
        }
    }
    let effective = candidate * 2f64.powi(shift);
    Some((effective, (effective / source - 1.0) * 100.0, shift))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoRule {
    pub low: f64,
    pub high: f64,
    pub center: f64,
    pub sigma: f64,
}

/// `tempo_rule`.
pub fn tempo_rule(intent: &str, tolerance: Option<f64>) -> TempoRule {
    let preset = preset(intent);
    let (low, high) = preset.tempo_window;
    match positive(tolerance) {
        Some(limit) if intent != "wordplay" => {
            let (low, high) = (low.max(-limit), high.min(limit));
            TempoRule { low, high, center: high.min(low.max(preset.tempo_center)), sigma: TEMPO_SIGMA_PERCENT.min(limit / 2.0) }
        }
        _ => TempoRule { low, high, center: preset.tempo_center, sigma: TEMPO_SIGMA_PERCENT },
    }
}

/// `tempo_score`.
pub fn tempo_score(source: Option<f64>, candidate: Option<f64>, center: f64, sigma: f64) -> Option<f64> {
    let (_, percent, shift) = matched_tempo(source, candidate)?;
    let score = (-0.5 * ((percent - center) / sigma).powi(2)).exp();
    Some(if shift != 0 { score * HALF_DOUBLE_TEMPO_FACTOR } else { score })
}

/// `parse_transition`: opening and closing BPM declared in a title.
pub fn parse_transition(title: Option<&str>) -> Option<(f64, f64, bool)> {
    let title = title?;
    let explicit = casefold(title).contains("transition");
    let pattern = Regex::new(r"(?i)(\d{2,3}(?:\.\d+)?)\s*(?:-|–|—|~|〜|→|->|>|to)\s*(\d{2,3}(?:\.\d+)?)").expect("pattern");
    let bytes = title.as_bytes();
    for captures in pattern.captures_iter(title) {
        let (first, second) = (captures.get(1).expect("group"), captures.get(2).expect("group"));
        // (?<![\d.]) and (?![\d.]) by hand: the regex crate has no lookaround.
        if first.start() > 0 && matches!(bytes[first.start() - 1], b'0'..=b'9' | b'.') {
            continue;
        }
        if second.end() < bytes.len() && matches!(bytes[second.end()], b'0'..=b'9' | b'.') {
            continue;
        }
        let (start, end): (f64, f64) = (first.as_str().parse().ok()?, second.as_str().parse().ok()?);
        if (55.0..=200.0).contains(&start) && (55.0..=200.0).contains(&end) && (start - end).abs() >= 5.0 {
            return Some((start, end, explicit));
        }
    }
    None
}

/// `TransitionFit`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionFit {
    pub kind: &'static str,
    pub score: f64,
    pub start_bpm: f64,
    pub end_bpm: f64,
    pub explicit: bool,
}

/// `transition_fit`.
pub fn transition_fit(source_bpm: Option<f64>, candidate: &Facts) -> Option<TransitionFit> {
    let source = positive(source_bpm)?;
    let tolerance = transition_tolerance();
    if let Some((start, end, explicit)) = parse_transition(candidate.title.as_deref()) {
        let distance = bpm_distance(Some(source), Some(start));
        if distance <= tolerance {
            let score = (-0.5 * (distance / 1.02f64.log2()).powi(2)).exp();
            return Some(TransitionFit { kind: "edit", score, start_bpm: start, end_bpm: end, explicit });
        }
        return None;
    }
    let candidate_bpm = positive(candidate.bpm)?;
    let ratio = candidate_bpm.log2() - source.log2();
    for (octave, kind) in [(1.0, "double"), (-1.0, "half")] {
        let distance = (ratio - octave).abs();
        if distance <= tolerance {
            let score = (-0.5 * (distance / 1.02f64.log2()).powi(2)).exp();
            return Some(TransitionFit { kind, score, start_bpm: source, end_bpm: candidate_bpm, explicit: false });
        }
    }
    None
}

// ------------------------------------------------------------------ key

fn camelot(key: Option<&str>) -> Option<(i64, char)> {
    let code = normalize_key(key)?;
    let (number, letter) = code.split_at(code.len() - 1);
    Some((number.parse().ok()?, letter.chars().next()?))
}

/// `key_move`.
pub fn key_move(source: Option<&str>, candidate: Option<&str>) -> Option<&'static str> {
    let (source, candidate) = (camelot(source)?, camelot(candidate)?);
    let mut step = (candidate.0 - source.0).rem_euclid(12);
    if step > 6 {
        step -= 12;
    }
    if source.1 == candidate.1 {
        return Some(match step {
            0 => "same",
            1 => "+1",
            -1 => "-1",
            2 => "+2",
            -5 => "+7",
            5 => "-7",
            _ => "other",
        });
    }
    if step == 0 {
        return Some(if candidate.1 == 'B' { "to_major" } else { "to_minor" });
    }
    Some(if step.abs() == 1 { "diagonal" } else { "other" })
}

fn compatible(movement: &str) -> f64 {
    match movement {
        "same" => 1.0,
        "+1" | "-1" => 0.9,
        "to_major" | "to_minor" => 0.85,
        "diagonal" => 0.55,
        "+7" => 0.5,
        "-7" | "+2" => 0.4,
        _ => 0.15,
    }
}

fn key_table(preset: &str, movement: &str) -> f64 {
    match preset {
        "hype" => match movement {
            "+1" => 1.0,
            "+7" => 0.95,
            "same" => 0.8,
            "+2" => 0.75,
            "to_major" => 0.7,
            "-1" | "to_minor" => 0.55,
            "diagonal" => 0.5,
            "-7" => 0.2,
            _ => 0.15,
        },
        "calm" => match movement {
            "-1" => 1.0,
            "to_minor" => 0.9,
            "same" => 0.8,
            "-7" => 0.6,
            "+1" => 0.55,
            "to_major" => 0.5,
            "diagonal" => 0.45,
            "+7" | "+2" => 0.2,
            _ => 0.15,
        },
        _ => compatible(movement),
    }
}

/// `key_score`.
pub fn key_score(source: Option<&str>, candidate: Option<&str>, preset: &str) -> Option<f64> {
    let movement = key_move(source, candidate)?;
    let target = match preset {
        "emotional" => Some(('A', "to_minor")),
        "bright" => Some(('B', "to_major")),
        _ => None,
    };
    if let Some((letter, arrival)) = target {
        if movement == arrival {
            return Some(1.0);
        }
        let is_compatible = compatible(movement) >= 0.85;
        if camelot(candidate).is_some_and(|c| c.1 == letter) {
            return Some(if is_compatible { 0.85 } else if movement == "diagonal" { 0.55 } else { 0.15 });
        }
        return Some(if is_compatible { 0.45 } else { 0.15 });
    }
    Some(key_table(preset, movement))
}

fn key_move_text(movement: &str) -> &'static str {
    match movement {
        "same" => "同じキー",
        "+1" => "+1",
        "-1" => "-1",
        "+2" => "+2（全音上げ）",
        "+7" => "+7（半音上げ）",
        "-7" => "-7（半音下げ）",
        "to_major" => "長調へ",
        "to_minor" => "短調へ",
        "diagonal" => "斜め移動",
        _ => "離れたキー",
    }
}

// ------------------------------------------------------------------ features

/// `direction_score`.
pub fn direction_score(delta: f64, direction: Direction, step: f64) -> f64 {
    if direction == Hold {
        return (1.0 - (delta.abs() / (step * 2.0)).min(1.0)).max(0.0);
    }
    let signed = if matches!(direction, Up | NotDown) { delta } else { -delta };
    if matches!(direction, NotDown | NotUp) {
        if signed >= 0.0 {
            return if signed <= step * 2.0 { 1.0 } else { 0.5f64.max(1.0 - (signed - step * 2.0) / (step * 4.0)) };
        }
        return (1.0 + signed / step).max(0.0);
    }
    if signed <= step {
        return (0.5 + signed / (step * 2.0)).max(0.0).min(1.0);
    }
    0.6f64.max(1.0 - (signed - step) / (step * 4.0))
}

pub fn feature_score(source: &Facts, candidate: &Facts, feature: &str, direction: Direction) -> Option<f64> {
    let (left, right) = (source.feature(feature)?, candidate.feature(feature)?);
    Some(direction_score(right - left, direction, feature_step(feature)))
}

pub fn era_score(source: &Facts, candidate: &Facts) -> Option<f64> {
    let (left, right) = (positive(source.year)?, positive(candidate.year)?);
    let older = left - right;
    if older < THROWBACK_MIN_YEARS {
        return Some(0.0);
    }
    Some(1.0f64.min(0.6 + 0.4 * (older - THROWBACK_MIN_YEARS) / 5.0))
}

pub fn timbre_score(similarity: Option<f64>) -> Option<f64> {
    let value = similarity.filter(|v| v.is_finite())?;
    Some(((value - TIMBRE_FLOOR) / (TIMBRE_CEILING - TIMBRE_FLOOR)).clamp(0.0, 1.0))
}

/// An approved wordplay edge as scoring reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PairFacts {
    pub keyword: String,
    pub source_phrase: String,
    pub target_phrase: String,
    pub tested: bool,
}

pub fn wordplay_score(pair: Option<&PairFacts>) -> Option<f64> {
    pair.map(|pair| if pair.tested { 1.0 } else { 0.8 })
}

fn fold(value: &Option<String>) -> String {
    casefold(py_strip(value.as_deref().unwrap_or("")))
}

fn genre_likeness(anchor: &Facts, candidate: &Facts) -> Option<f64> {
    let (left, right) = (fold(&anchor.genre), fold(&candidate.genre));
    if left.is_empty() || right.is_empty() {
        return None;
    }
    if left != right {
        return Some(0.0);
    }
    let (left_sub, right_sub) = (fold(&anchor.subgenre), fold(&candidate.subgenre));
    Some(if !left_sub.is_empty() && left_sub == right_sub { 1.0 } else { 0.8 })
}

/// `genre_continuity_score`.
pub fn genre_continuity_score(source: &Facts, candidate: &Facts) -> Option<f64> {
    genre_likeness(source, candidate)
}

fn era_likeness(anchor: &Facts, candidate: &Facts) -> Option<f64> {
    let (left, right) = (positive(anchor.year)?, positive(candidate.year)?);
    Some((1.0 - (left - right).abs() / ERA_SPAN_YEARS).max(0.0))
}

fn feature_closeness(source: &Facts, candidate: &Facts) -> Option<f64> {
    let deltas: Vec<f64> = FEEL_FEATURES
        .iter()
        .filter_map(|feature| Some(candidate.feature(feature)? - source.feature(feature)?))
        .collect();
    if deltas.is_empty() {
        return None;
    }
    let mean = deltas.iter().map(|d| d.abs()).sum::<f64>() / deltas.len() as f64;
    Some((1.0 - (mean / 0.35).min(1.0)).max(0.0))
}

/// `history_fits`.
pub fn history_fits(source: &Facts, candidate: &Facts, transition: bool) -> bool {
    if !transition {
        match matched_tempo(source.bpm, candidate.bpm) {
            Some((_, percent, 0)) if percent.abs() <= HISTORY_TEMPO_GATE_PERCENT => {}
            _ => return false,
        }
    }
    match (source.feature("energy"), candidate.feature("energy")) {
        (Some(left), Some(right)) => (right - left).abs() <= HISTORY_ENERGY_GATE,
        _ => true,
    }
}

// ------------------------------------------------------------------ filters

pub fn passes_intent_filter(intent: &str, source: &Facts, candidate: &Facts, tolerance: Option<f64>) -> bool {
    if intent == "wordplay" {
        return true;
    }
    let Some((_, percent, shift)) = matched_tempo(source.bpm, candidate.bpm) else { return false };
    if shift != 0 {
        return false;
    }
    let rule = tempo_rule(intent, tolerance);
    if !(rule.low <= percent && percent <= rule.high) {
        return false;
    }
    if intent == "throwback" {
        return era_score(source, candidate).is_some_and(|s| s > 0.0);
    }
    true
}

pub fn passes_transition_filter(intent: &str, source: &Facts, candidate: &Facts) -> bool {
    if intent == "throwback" {
        return era_score(source, candidate).is_some_and(|s| s > 0.0);
    }
    true
}

// ------------------------------------------------------------------ scoring

/// `Reason`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reason {
    pub kind: String,
    pub tone: &'static str,
    pub text: String,
}

fn reason(kind: &str, tone: &'static str, text: impl Into<String>) -> Reason {
    Reason { kind: kind.into(), tone, text: text.into() }
}

/// `Scored`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scored {
    pub score: f64,
    /// In Python's insertion order.
    pub components: Vec<(String, f64)>,
    pub reasons: Vec<Reason>,
    pub summary: String,
    pub strengths: Vec<String>,
}

/// `evaluate`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    source: &Facts,
    candidate: &Facts,
    intent: &str,
    vector_similarity: Option<f64>,
    pair: Option<&PairFacts>,
    has_analysis: bool,
    transition: Option<&TransitionFit>,
    tolerance: Option<f64>,
) -> Scored {
    let preset = preset(intent);
    let tempo = tempo_rule(intent, tolerance);
    let mut values: Vec<(&str, Option<f64>)> = vec![
        ("bpm", match transition {
            Some(fit) => Some(fit.score),
            None => tempo_score(source.bpm, candidate.bpm, tempo.center, tempo.sigma),
        }),
        ("key", key_score(source.key.as_deref(), candidate.key.as_deref(), intent)),
        ("vector", timbre_score(vector_similarity)),
        ("continuity", genre_continuity_score(source, candidate)),
        ("era", era_score(source, candidate)),
        ("era_proximity", era_likeness(source, candidate)),
        ("wordplay", wordplay_score(pair)),
    ];
    for (feature, direction) in &preset.directions {
        values.push((feature, feature_score(source, candidate, feature, *direction)));
    }
    let components: Vec<(String, f64)> = values
        .iter()
        .filter(|(name, value)| value.is_some() && preset.weight(name).is_some())
        .map(|(name, value)| ((*name).to_owned(), value.expect("some").clamp(0.0, 1.0)))
        .collect();
    let available: f64 = components.iter().map(|(name, _)| preset.weight(name).expect("weighted")).sum();
    let score = if available > 0.0 {
        components.iter().map(|(name, value)| preset.weight(name).expect("weighted") * value).sum::<f64>() / available
    } else {
        0.0
    };
    let value = |name: &str| values.iter().find(|(n, _)| *n == name).and_then(|(_, v)| *v);
    let reasons = build_reasons(source, candidate, intent, value("key"), value("continuity"), value("vector"), pair, has_analysis, transition, &tempo);
    let (strengths, summary) = summarize(intent, source, candidate, &components, transition);
    Scored { score, components, reasons, summary, strengths }
}

/// `summarize`: strengths and the one-line summary.
pub fn summarize(intent: &str, source: &Facts, candidate: &Facts, components: &[(String, f64)], transition: Option<&TransitionFit>) -> (Vec<String>, String) {
    let preset = preset(intent);
    let weight = |name: &str| preset.weight(name).unwrap_or(0.0);
    let mut contribution: Vec<&(String, f64)> = components.iter().collect();
    contribution.sort_by(|a, b| {
        let (ka, kb) = (-weight(&a.0) * a.1, -weight(&b.0) * b.1);
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
    });
    let strengths: Vec<String> = contribution.iter().filter(|(_, v)| *v >= STRENGTH_THRESHOLD).map(|(n, _)| n.clone()).collect();
    let weaknesses: Vec<String> = contribution.iter().filter(|(_, v)| *v < WEAKNESS_THRESHOLD).map(|(n, _)| n.clone()).collect();
    let unknown: Vec<&str> = preset.weights.iter().map(|(n, _)| *n).filter(|n| *n != "wordplay" && !components.iter().any(|(c, _)| c == n)).collect();
    let mut facts = Vec::new();
    for (feature, _) in &preset.directions {
        if let (Some(left), Some(right)) = (source.feature(feature), candidate.feature(feature)) {
            if preset.weight(feature).is_some() {
                facts.push(format!("{} {}", feature_label(feature), signed(right - left, 2)));
            }
        }
    }
    if intent == "throwback" {
        if let (Some(left), Some(right)) = (positive(source.year), positive(candidate.year)) {
            facts.push(format!("{}年（{}年前）", right.trunc() as i64, (left - right).trunc() as i64));
        }
    }
    if let Some(movement) = key_move(source.key.as_deref(), candidate.key.as_deref()) {
        facts.push(format!(
            "キー {}→{}（{}）",
            normalize_key(source.key.as_deref()).unwrap_or_default(),
            normalize_key(candidate.key.as_deref()).unwrap_or_default(),
            key_move_text(movement)
        ));
    }
    match transition {
        Some(fit) => facts.push(transition_text(fit)),
        None => {
            if let Some((_, percent, shift)) = matched_tempo(source.bpm, candidate.bpm) {
                let mut fact = format!("BPM {}%", signed(percent, 1));
                if shift != 0 {
                    fact.push_str("（倍テン／ハーフテン扱い）");
                }
                facts.push(fact);
            }
        }
    }
    let total: f64 = components.iter().map(|(n, v)| weight(n) * v).sum();
    let available: f64 = components.iter().map(|(n, _)| weight(n)).sum();
    let fit = if available > 0.0 { total / available } else { 0.0 };
    let head = if fit >= 0.65 { format!("{}向き", preset.label) } else { format!("{}の効果は控えめ", preset.label) };
    let mut parts = vec![if facts.is_empty() { head } else { format!("{head}：{}", facts.join("・")) }];
    let label = |names: &[String]| names.iter().map(|n| component_label(n)).collect::<Vec<_>>().join("・");
    if !weaknesses.is_empty() {
        parts.push(format!("{}は弱め", label(&weaknesses)));
    }
    if !unknown.is_empty() {
        parts.push(format!("{}は未判定", unknown.iter().map(|n| component_label(n)).collect::<Vec<_>>().join("・")));
    }
    (strengths, parts.join("／"))
}

fn transition_text(fit: &TransitionFit) -> String {
    if fit.kind == "edit" {
        return format!("トランジション {}→{} BPM", g(fit.start_bpm), g(fit.end_bpm));
    }
    format!("{} {}→{} BPM", if fit.kind == "double" { "倍テン" } else { "ハーフテン" }, g(fit.start_bpm), g(fit.end_bpm))
}

/// `describe_weights`.
pub fn describe_weights(intent: &str) -> String {
    let mut weights = preset(intent).weights;
    weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    weights.iter().map(|(name, weight)| format!("{} {}%", component_label(name), (weight * 100.0).round_ties_even() as i64)).collect::<Vec<_>>().join("・")
}

/// `Likeness`.
#[derive(Clone, Debug, PartialEq)]
pub struct Likeness {
    pub score: f64,
    pub aspects: Vec<&'static str>,
}

/// `likeness`.
pub fn likeness(anchor: &Facts, candidate: &Facts, vector_similarity: Option<f64>) -> Option<Likeness> {
    let values = [
        ("timbre", vector_similarity.filter(|v| v.is_finite())),
        ("feel", feature_closeness(anchor, candidate)),
        ("genre", genre_likeness(anchor, candidate)),
        ("era", era_likeness(anchor, candidate)),
    ];
    if values[0].1.is_none() && values[1].1.is_none() {
        return None;
    }
    let measured: Vec<(&'static str, f64)> = values.iter().filter_map(|(n, v)| v.map(|v| (*n, v.clamp(0.0, 1.0)))).collect();
    let total: f64 = measured.iter().map(|(n, _)| likeness_weight(n)).sum();
    let score = measured.iter().map(|(n, v)| likeness_weight(n) * v).sum::<f64>() / total;
    let mut aspects: Vec<(&'static str, f64)> = measured.iter().filter(|(_, v)| *v >= STRENGTH_THRESHOLD).copied().collect();
    aspects.sort_by(|a, b| (-likeness_weight(a.0) * a.1).partial_cmp(&(-likeness_weight(b.0) * b.1)).unwrap_or(std::cmp::Ordering::Equal));
    Some(Likeness { score, aspects: aspects.into_iter().map(|(n, _)| n).collect() })
}

/// `blend`.
pub fn blend(mix_score: f64, like: Option<&Likeness>) -> f64 {
    REFERENCE_BLEND * like.map_or(0.0, |l| l.score) + (1.0 - REFERENCE_BLEND) * mix_score
}

/// `describe_like`.
pub fn describe_like(label: &str, like: &Likeness) -> String {
    let aspects: Vec<&str> = like.aspects.iter().take(3).map(|n| likeness_label(n)).collect();
    let aspects = if aspects.is_empty() { "雰囲気".to_owned() } else { aspects.join("・") };
    format!("{label}のような{aspects}")
}

#[allow(clippy::too_many_arguments)]
fn build_reasons(
    source: &Facts,
    candidate: &Facts,
    intent: &str,
    key: Option<f64>,
    continuity: Option<f64>,
    similarity: Option<f64>,
    pair: Option<&PairFacts>,
    has_analysis: bool,
    transition: Option<&TransitionFit>,
    tempo: &TempoRule,
) -> Vec<Reason> {
    let mut reasons = Vec::new();
    if let Some(pair) = pair {
        let caveat = if pair.tested { "音出し検証済み" } else { "音出しは未検証です（承認は音を確認したという意味ではありません）" };
        reasons.push(reason(
            "wordplay",
            if pair.tested { "good" } else { "neutral" },
            format!("承認済みワードプレイ「{}」：「{}」→「{}」／{caveat}", pair.keyword, py_strip(&pair.source_phrase), py_strip(&pair.target_phrase)),
        ));
    }
    match transition {
        Some(fit) => reasons.push(transition_reason(source, fit)),
        None => {
            reasons.push(tempo_reason(source, candidate, tempo));
            if let Some((start, end, _)) = parse_transition(candidate.title.as_deref()) {
                reasons.push(reason("tempo_change", "caution", format!("タイトル上テンポが変わる曲（{}→{}）：解析 BPM は参考値です", g(start), g(end))));
            }
        }
    }
    reasons.push(key_reason(source, candidate, key));
    reasons.push(match continuity {
        None => reason("continuity", "caution", "ジャンル情報がないため系統の連続性は判定できません"),
        Some(score) if score >= 0.95 => reason("continuity", "good", "同じサブジャンルで流れを保ちやすい"),
        Some(score) if score >= 0.75 => reason("continuity", "neutral", "同じジャンルだがサブジャンルは異なります"),
        Some(_) => reason("continuity", "caution", "ジャンルが変わるため流れの連続性は弱めです"),
    });
    for (feature, direction) in preset(intent).directions {
        let label = feature_label(feature);
        reasons.push(match (source.feature(feature), candidate.feature(feature)) {
            (Some(left), Some(right)) => {
                let delta = right - left;
                let score = direction_score(delta, direction, feature_step(feature));
                let tone = if score >= 0.65 { "good" } else if score >= 0.4 { "neutral" } else { "caution" };
                let movement = if delta.abs() < feature_step(feature) / 3.0 { "を維持" } else if delta > 0.0 { "を上げる" } else { "を下げる" };
                reason(feature, tone, format!("{label}{movement}"))
            }
            _ => reason(feature, "caution", format!("{label}は未解析です")),
        });
    }
    if intent == "throwback" {
        reasons.push(match (positive(source.year), positive(candidate.year)) {
            (Some(left), Some(right)) => reason("era", "good", format!("{}年リリース（基準の{}年前）", right.trunc() as i64, (left - right).trunc() as i64)),
            _ => reason("era", "caution", "リリース年が未登録のため判定できません"),
        });
    }
    reasons.push(match similarity {
        Some(value) => reason("similarity", "neutral", if value >= 0.7 { "音色の傾向が近い" } else { "音色の傾向に違いがある" }),
        None if has_analysis => reason("similarity", "caution", "音色を比較できません"),
        None => reason("similarity", "caution", "音色の比較は計算待ち"),
    });
    reasons
}

fn tempo_reason(source: &Facts, candidate: &Facts, tempo: &TempoRule) -> Reason {
    let Some((effective, percent, shift)) = matched_tempo(source.bpm, candidate.bpm) else {
        return reason("tempo", "caution", "BPM が未取得のためテンポの相性は判定できません");
    };
    let (source_bpm, candidate_bpm) = (source.bpm.unwrap_or_default(), candidate.bpm.unwrap_or_default());
    let note = if shift != 0 { format!("、原曲{} BPMの{}換算", g(candidate_bpm), if shift == -1 { "1/2" } else { "2倍" }) } else { String::new() };
    let off = (percent - tempo.center).abs();
    let tone = if off <= tempo.sigma { "good" } else if off <= tempo.sigma * 2.0 { "neutral" } else { "caution" };
    reason("tempo", tone, format!("BPM {} → {}（{}%{note}）", g(source_bpm), g(effective), signed(percent, 1)))
}

fn transition_reason(source: &Facts, fit: &TransitionFit) -> Reason {
    if fit.kind == "edit" {
        let label = if fit.explicit { "トランジション曲" } else { "タイトルからテンポ変化曲と判断" };
        return reason(
            "transition",
            if fit.explicit { "good" } else { "neutral" },
            format!("{label}：入り {} → 出口 {} BPM（基準 {} BPM から入れる）", g(fit.start_bpm), g(fit.end_bpm), g(source.bpm.unwrap_or_default())),
        );
    }
    let kind = if fit.kind == "double" { "倍テン" } else { "ハーフテン" };
    reason("transition", "neutral", format!("{kind}：{} → {} BPM", g(fit.start_bpm), g(fit.end_bpm)))
}

fn key_reason(source: &Facts, candidate: &Facts, score: Option<f64>) -> Reason {
    let left = normalize_key(source.key.as_deref());
    let right = normalize_key(candidate.key.as_deref());
    let Some(score) = score else {
        return reason("key", "caution", format!("{}のキー情報がないため判定できません", if left.is_none() { "元曲" } else { "候補" }));
    };
    let movement = key_move(left.as_deref(), right.as_deref()).unwrap_or("other");
    let tone = if score >= 0.85 { "good" } else if score >= 0.5 { "neutral" } else { "caution" };
    reason("key", tone, format!("キー {} → {}：{}", left.unwrap_or_default(), right.unwrap_or_default(), key_move_text(movement)))
}

// ------------------------------------------------------------------ filters

/// `normalize_text`: NFKC, casefold, whitespace collapsed.
pub fn normalize_text(value: Option<&str>) -> String {
    match value {
        None | Some("") => String::new(),
        Some(text) => py_split(&casefold(&nfkc(text))).join(" "),
    }
}

/// `AssistFilters`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub bpm_min: Option<f64>,
    pub bpm_max: Option<f64>,
    pub genres: Vec<String>,
    pub subgenres: Vec<String>,
    pub artists: Vec<String>,
    pub keys: Vec<String>,
    pub year_min: Option<i64>,
    pub year_max: Option<i64>,
    pub query: String,
}

fn texts(values: &[String]) -> Vec<String> {
    let mut seen: Vec<(String, String)> = Vec::new();
    for value in values {
        let text = py_split(value).join(" ");
        if !text.is_empty() {
            let key = normalize_text(Some(&text));
            if !seen.iter().any(|(k, _)| *k == key) {
                seen.push((key, text));
            }
        }
    }
    seen.into_iter().map(|(_, t)| t).take(32).collect()
}

/// The raw filter fields of a request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawFilters {
    pub bpm_min: Option<f64>,
    pub bpm_max: Option<f64>,
    pub genres: Vec<String>,
    pub subgenres: Vec<String>,
    pub artists: Vec<String>,
    pub keys: Vec<String>,
    pub year_min: Option<i64>,
    pub year_max: Option<i64>,
    pub query: String,
}

impl Filters {
    /// `from_dict`; `Err` is the message of an unreadable key.
    pub fn from_raw(raw: &RawFilters) -> Result<Self, String> {
        let mut keys: Vec<String> = Vec::new();
        for value in &raw.keys {
            let key = normalize_key(Some(value)).ok_or_else(|| format!("キー「{value}」を解釈できません（例: 8A, A minor）"))?;
            if !keys.iter().any(|k| k == key) {
                keys.push(key.to_owned());
            }
        }
        keys.truncate(32);
        let finite = |v: Option<f64>| v.filter(|x| x.is_finite());
        let (mut bpm_min, mut bpm_max) = (finite(raw.bpm_min), finite(raw.bpm_max));
        if let (Some(low), Some(high)) = (bpm_min, bpm_max) {
            if low > high {
                (bpm_min, bpm_max) = (Some(high), Some(low));
            }
        }
        let (mut year_min, mut year_max) = (raw.year_min, raw.year_max);
        if let (Some(low), Some(high)) = (year_min, year_max) {
            if low > high {
                (year_min, year_max) = (Some(high), Some(low));
            }
        }
        Ok(Self {
            bpm_min,
            bpm_max,
            genres: texts(&raw.genres),
            subgenres: texts(&raw.subgenres),
            artists: texts(&raw.artists),
            keys,
            year_min,
            year_max,
            query: py_split(&raw.query).join(" ").chars().take(200).collect(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.bpm_min.is_none()
            && self.bpm_max.is_none()
            && self.genres.is_empty()
            && self.subgenres.is_empty()
            && self.artists.is_empty()
            && self.keys.is_empty()
            && self.year_min.is_none()
            && self.year_max.is_none()
            && self.query.is_empty()
    }

    /// `matches`.
    pub fn matches(&self, track: &Facts, album: Option<&str>) -> bool {
        if self.bpm_min.is_some() || self.bpm_max.is_some() {
            let Some(bpm) = positive(track.bpm) else { return false };
            if self.bpm_min.is_some_and(|low| bpm < low) || self.bpm_max.is_some_and(|high| bpm > high) {
                return false;
            }
        }
        let folded = |values: &[String]| values.iter().map(|v| normalize_text(Some(v))).collect::<Vec<_>>();
        if !self.genres.is_empty() && !folded(&self.genres).contains(&normalize_text(track.genre.as_deref())) {
            return false;
        }
        if !self.subgenres.is_empty() && !folded(&self.subgenres).contains(&normalize_text(track.subgenre.as_deref())) {
            return false;
        }
        if !self.artists.is_empty() {
            let artist = normalize_text(track.artist.as_deref());
            if artist.is_empty() || !folded(&self.artists).iter().any(|wanted| artist.contains(wanted.as_str())) {
                return false;
            }
        }
        if !self.keys.is_empty() && !normalize_key(track.key.as_deref()).is_some_and(|key| self.keys.iter().any(|k| k == key)) {
            return false;
        }
        if self.year_min.is_some() || self.year_max.is_some() {
            let Some(year) = positive(track.year) else { return false };
            if self.year_min.is_some_and(|low| year < low as f64) || self.year_max.is_some_and(|high| year > high as f64) {
                return false;
            }
        }
        if !self.query.is_empty() {
            let haystack = [normalize_text(track.title.as_deref()), normalize_text(track.artist.as_deref()), normalize_text(album)].join(" ");
            if !normalize_text(Some(&self.query)).split(' ').all(|word| haystack.contains(word)) {
                return false;
            }
        }
        true
    }

    /// `describe`.
    pub fn describe(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.bpm_min.is_some() || self.bpm_max.is_some() {
            parts.push(format!("BPM {}〜{}", self.bpm_min.map(g).unwrap_or_default(), self.bpm_max.map(g).unwrap_or_default()));
        }
        if !self.genres.is_empty() {
            parts.push(format!("ジャンル {}", self.genres.join(" / ")));
        }
        if !self.subgenres.is_empty() {
            parts.push(format!("サブジャンル {}", self.subgenres.join(" / ")));
        }
        if !self.artists.is_empty() {
            parts.push(format!("アーティスト {}", self.artists.join(" / ")));
        }
        if !self.keys.is_empty() {
            parts.push(format!("キー {}", self.keys.join(" / ")));
        }
        if self.year_min.is_some() || self.year_max.is_some() {
            parts.push(format!(
                "{}〜{}年",
                self.year_min.map(|y| y.to_string()).unwrap_or_default(),
                self.year_max.map(|y| y.to_string()).unwrap_or_default()
            ));
        }
        if !self.query.is_empty() {
            parts.push(format!("「{}」を含む", self.query));
        }
        parts
    }
}

// ------------------------------------------------------------------ play history

/// A recording identity: normalized (title, artist).
pub type Identity = (String, String);

pub const NEAR_WINDOW: usize = 3;
pub const MIN_SESSION_TRACKS: usize = 8;
pub const ACTIVE_SESSION_HOURS: i64 = 12;
const FOLLOWED_WEIGHT: f64 = 2.8;
const NEARBY_WEIGHT: f64 = 0.7;
const AFFINITY_SCALE: f64 = 3.0;

/// `Evidence`.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub followed: i64,
    pub nearby: i64,
    pub plays: i64,
    pub days_since: Option<f64>,
    pub affinity: f64,
}

impl Evidence {
    pub fn related(&self) -> bool {
        self.followed > 0 || self.nearby > 0
    }
}

/// `PlayHistory`.
#[derive(Clone, Debug, Default)]
pub struct PlayHistory {
    now: Option<chrono::NaiveDateTime>,
    followed: HashMap<Identity, HashMap<Identity, i64>>,
    nearby: HashMap<Identity, HashMap<Identity, i64>>,
    plays: HashMap<Identity, i64>,
    last: HashMap<Identity, chrono::NaiveDateTime>,
}

impl PlayHistory {
    pub fn new(sessions: impl IntoIterator<Item = (Option<chrono::NaiveDateTime>, Vec<Option<Identity>>)>, now: chrono::NaiveDateTime) -> Self {
        let mut history = Self { now: Some(now), ..Self::default() };
        for (started, tracks) in sessions {
            if tracks.len() < MIN_SESSION_TRACKS {
                continue;
            }
            if started.is_some_and(|at| now - at < chrono::Duration::hours(ACTIVE_SESSION_HOURS)) {
                continue;
            }
            history.add(started, &tracks);
        }
        history
    }

    fn add(&mut self, started: Option<chrono::NaiveDateTime>, tracks: &[Option<Identity>]) {
        for pair in tracks.windows(2) {
            if let (Some(previous), Some(current)) = (&pair[0], &pair[1]) {
                if previous != current {
                    *self.followed.entry(previous.clone()).or_default().entry(current.clone()).or_default() += 1;
                }
            }
        }
        for (position, current) in tracks.iter().enumerate() {
            let Some(current) = current else { continue };
            for other in tracks[position.saturating_sub(NEAR_WINDOW)..position].iter().flatten() {
                if other != current {
                    *self.nearby.entry(current.clone()).or_default().entry(other.clone()).or_default() += 1;
                    *self.nearby.entry(other.clone()).or_default().entry(current.clone()).or_default() += 1;
                }
            }
        }
        let unique: std::collections::HashSet<&Identity> = tracks.iter().flatten().collect();
        for identity in unique {
            *self.plays.entry(identity.clone()).or_default() += 1;
            if let Some(at) = started {
                if self.last.get(identity).is_none_or(|last| at > *last) {
                    self.last.insert(identity.clone(), at);
                }
            }
        }
    }

    /// `related`: identities played next to `source`, most often first.
    pub fn related(&self, source: &Identity) -> Vec<Identity> {
        let mut counts: BTreeMap<Identity, i64> = BTreeMap::new();
        for map in [self.nearby.get(source), self.followed.get(source)].into_iter().flatten() {
            for (identity, count) in map {
                *counts.entry(identity.clone()).or_default() += count;
            }
        }
        let mut ordered: Vec<(Identity, i64)> = counts.into_iter().collect();
        // Python sorts by str(identity), the tuple's repr.
        ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| identity_repr(&a.0).cmp(&identity_repr(&b.0))));
        ordered.into_iter().map(|(identity, _)| identity).collect()
    }

    /// `evidence`.
    pub fn evidence(&self, source: Option<&Identity>, candidate: Option<&Identity>) -> Option<Evidence> {
        let candidate = candidate?;
        let count = |map: &HashMap<Identity, HashMap<Identity, i64>>| source.and_then(|s| map.get(s)).and_then(|m| m.get(candidate)).copied().unwrap_or(0);
        let (followed, nearby) = (count(&self.followed), count(&self.nearby));
        let plays = self.plays.get(candidate).copied().unwrap_or(0);
        if followed == 0 && nearby == 0 && plays == 0 {
            return None;
        }
        let days_since = match (self.last.get(candidate), self.now) {
            (Some(last), Some(now)) => Some(((now - *last).num_microseconds().unwrap_or(0) as f64 / 1e6 / 86400.0).max(0.0)),
            _ => None,
        };
        let strength = FOLLOWED_WEIGHT * (followed as f64).ln_1p() + NEARBY_WEIGHT * (nearby as f64).ln_1p();
        Some(Evidence { followed, nearby, plays, days_since, affinity: 1.0 - (-strength / AFFINITY_SCALE).exp() })
    }
}

fn identity_repr(identity: &Identity) -> String {
    let repr = plumdeck_core::compat::pyobj::str_repr;
    format!("({}, {})", repr(&identity.0), repr(&identity.1))
}

fn ago(days: f64) -> String {
    if days < 1.0 {
        "今日".into()
    } else if days < 60.0 {
        format!("{}日前", days.trunc() as i64)
    } else if days < 730.0 {
        format!("{}か月前", (days / 30.0).floor() as i64)
    } else {
        format!("{}年前", (days / 365.0).floor() as i64)
    }
}

/// `describe` (history).
pub fn describe_history(evidence: &Evidence) -> String {
    let mut head = if evidence.followed > 0 {
        format!("過去のセットでこの曲の直後に {} 回掛けています", evidence.followed)
    } else if evidence.nearby > 0 {
        format!("過去のセットでこの曲の近くで {} 回掛けています", evidence.nearby)
    } else {
        format!("過去 {} 回のセットで使った曲です", evidence.plays)
    };
    if let Some(days) = evidence.days_since {
        head.push_str(&format!("（最後は{}）", ago(days)));
    }
    head
}

/// `headline`.
pub fn history_headline(evidence: &Evidence) -> String {
    if evidence.followed > 0 {
        format!("実績：直後に{}回", evidence.followed)
    } else {
        format!("実績：近くで{}回", evidence.nearby)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(bpm: f64, key: &str, energy: f64) -> Facts {
        Facts { id: 1, bpm: Some(bpm), key: Some(key.into()), energy: Some(energy), danceability: Some(0.5), noisiness: Some(0.2), ..Facts::default() }
    }

    #[test]
    fn tempo_key_and_transition_rules() {
        assert_eq!(matched_tempo(Some(120.0), Some(62.0)).map(|m| m.2), Some(1));
        assert_eq!(parse_transition(Some("Tall Boys 100-128 Transition (Dirty)")), Some((100.0, 128.0, true)));
        assert_eq!(parse_transition(Some("Vol. 1-2")), None);
        assert_eq!(parse_transition(Some("Track 2019-2020")), None);
        assert_eq!(key_move(Some("8A"), Some("9A")), Some("+1"));
        assert_eq!(key_move(Some("8A"), Some("8B")), Some("to_major"));
        assert_eq!(key_score(Some("8A"), Some("8B"), "bright"), Some(1.0));
        assert_eq!(resolve_intent("groove"), Ok("keep"));
        assert!(resolve_intent("nope").is_err());
    }

    #[test]
    fn evaluation_explains_itself() {
        let scored = evaluate(&track(124.0, "8A", 0.5), &track(125.0, "9A", 0.7), "hype", None, None, false, None, None);
        assert!(scored.score > 0.0 && scored.score <= 1.0);
        assert!(scored.summary.starts_with("盛り上げる"), "{}", scored.summary);
        assert!(scored.summary.contains("エネルギー +0.20"));
        assert!(scored.reasons.iter().any(|r| r.text == "音色の比較は計算待ち"));
        assert_eq!(describe_weights("keep"), "テンポ 30%・音色 22%・系統の連続性 22%・年代の近さ 10%・エネルギー 6%・ダンサビリティ 5%・ノイズ感 5%");
    }

    #[test]
    fn filters_and_history() {
        let raw = RawFilters { bpm_min: Some(130.0), bpm_max: Some(120.0), keys: vec!["A minor".into()], ..RawFilters::default() };
        let filters = Filters::from_raw(&raw).unwrap();
        assert_eq!((filters.bpm_min, filters.bpm_max), (Some(120.0), Some(130.0)));
        assert_eq!(filters.keys, vec!["8A"]);
        assert!(Filters::from_raw(&RawFilters { keys: vec!["H".into()], ..RawFilters::default() }).is_err());
        let id = |n: &str| Some((n.to_owned(), "a".to_owned()));
        let set: Vec<Option<Identity>> = ["a", "b", "c", "d", "e", "f", "g", "h"].iter().map(|n| id(n)).collect();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let history = PlayHistory::new([(Some(now - chrono::Duration::days(3)), set)], now);
        let evidence = history.evidence(id("a").as_ref(), id("b").as_ref()).unwrap();
        assert_eq!((evidence.followed, evidence.nearby, evidence.plays), (1, 1, 1));
        assert_eq!(describe_history(&evidence), "過去のセットでこの曲の直後に 1 回掛けています（最後は3日前）");
        assert_eq!(history.related(&id("a").unwrap())[0], id("b").unwrap());
    }
}
