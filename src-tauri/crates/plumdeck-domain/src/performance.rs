//! Performance metadata models (backend/api/schemas/performance_metadata.py:
//! `CuePoint`, `SavedLoop`, `BeatGrid`, `PerformanceMetadataWrite`), the
//! playback grid fit (backend/domain/services/analysis/beat_grid.py) and the
//! rekordbox timing shift (backend/infra/rekordbox_timing.py `adjust_grid`).
//!
//! Values are validated strictly as pydantic does (ints stay ints, bools are
//! not numbers) and written back in model field order with every default.

use plumdeck_core::compat::pyjson::PyValue;

/// `StrictInt | StrictFloat`, kept as given so JSON writes `1` vs `1.0` alike.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    pub fn value(self) -> f64 {
        match self {
            Num::Int(n) => n as f64,
            Num::Float(x) => x,
        }
    }

    fn of(value: &PyValue) -> Option<Num> {
        match value {
            PyValue::Int(n) => Some(Num::Int(*n)),
            PyValue::Float(x) => Some(Num::Float(*x)),
            _ => None,
        }
    }

    pub fn json(self) -> PyValue {
        match self {
            Num::Int(n) => PyValue::Int(n),
            Num::Float(x) => PyValue::Float(x),
        }
    }
}

/// A validation failure (pydantic `ValidationError`; 422 for a request,
/// 500 for stored data the response model rejects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

fn invalid<T>(message: &str) -> Result<T, Invalid> {
    Err(Invalid(message.to_owned()))
}

fn get<'a>(pairs: &'a [(PyValue, PyValue)], key: &str) -> Option<&'a PyValue> {
    pairs.iter().rev().find(|(name, _)| matches!(name, PyValue::Str(k) if k == key)).map(|(_, value)| value)
}

fn object<'a>(value: &'a PyValue, allowed: &[&str]) -> Result<&'a [(PyValue, PyValue)], Invalid> {
    let PyValue::Dict(pairs) = value else { return invalid("expected an object") };
    for (key, _) in pairs {
        match key {
            PyValue::Str(name) if allowed.contains(&name.as_str()) => {}
            _ => return invalid("extra field"),
        }
    }
    Ok(pairs)
}

fn strict_int(value: Option<&PyValue>) -> Result<Option<i128>, Invalid> {
    match value {
        None => Ok(None),
        Some(PyValue::Int(n)) => Ok(Some(*n)),
        Some(_) => invalid("expected an integer"),
    }
}

/// `FiniteNumber` with the `must be finite` validator: >= 0 and finite.
fn finite_number(value: Option<&PyValue>) -> Result<Num, Invalid> {
    let Some(number) = value.and_then(Num::of) else { return invalid("expected a number") };
    if !(number.value() >= 0.0) || !number.value().is_finite() {
        return invalid("expected a finite, nonnegative number");
    }
    Ok(number)
}

fn text_field(value: Option<&PyValue>, default: &str, max: usize) -> Result<String, Invalid> {
    match value {
        None => Ok(default.to_owned()),
        Some(PyValue::Str(text)) if text.chars().count() <= max => Ok(text.clone()),
        Some(_) => invalid("invalid string"),
    }
}

/// `CuePoint`.
#[derive(Clone, Debug, PartialEq)]
pub struct CuePoint {
    pub slot: i128,
    pub position_ms: Num,
    pub label: String,
    pub color: Option<String>,
}

impl CuePoint {
    pub fn parse(value: &PyValue) -> Result<Self, Invalid> {
        let pairs = object(value, &["slot", "position_ms", "label", "color"])?;
        let slot = strict_int(get(pairs, "slot"))?.ok_or_else(|| Invalid("slot required".into()))?;
        if !(0..=15).contains(&slot) {
            return invalid("slot out of range");
        }
        let position_ms = finite_number(get(pairs, "position_ms"))?;
        let label = text_field(get(pairs, "label"), "", 100)?;
        let color = match get(pairs, "color") {
            None | Some(PyValue::None) => None,
            Some(PyValue::Str(text)) if text.chars().count() <= 32 => Some(text.clone()),
            Some(_) => return invalid("invalid color"),
        };
        Ok(Self { slot, position_ms, label, color })
    }

    pub fn json(&self) -> PyValue {
        dict(vec![
            ("slot", PyValue::Int(self.slot)),
            ("position_ms", self.position_ms.json()),
            ("label", PyValue::Str(self.label.clone())),
            ("color", self.color.clone().map_or(PyValue::None, PyValue::Str)),
        ])
    }
}

/// `SavedLoop`.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedLoop {
    pub id: String,
    pub start_ms: Num,
    pub end_ms: Num,
    pub label: String,
}

impl SavedLoop {
    pub fn parse(value: &PyValue) -> Result<Self, Invalid> {
        let pairs = object(value, &["id", "start_ms", "end_ms", "label"])?;
        let id = match get(pairs, "id") {
            Some(PyValue::Str(id)) if (1..=64).contains(&id.chars().count()) && !plumdeck_core::compat::text::py_strip(id).is_empty() => id.clone(),
            _ => return invalid("invalid loop id"),
        };
        let start_ms = finite_number(get(pairs, "start_ms"))?;
        let end_ms = finite_number(get(pairs, "end_ms"))?;
        let label = text_field(get(pairs, "label"), "", 100)?;
        if end_ms.value() <= start_ms.value() {
            return invalid("end_ms must be greater than start_ms");
        }
        Ok(Self { id, start_ms, end_ms, label })
    }

    pub fn json(&self) -> PyValue {
        dict(vec![
            ("id", PyValue::Str(self.id.clone())),
            ("start_ms", self.start_ms.json()),
            ("end_ms", self.end_ms.json()),
            ("label", PyValue::Str(self.label.clone())),
        ])
    }
}

/// `BeatGrid`.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: Num,
    pub first_beat_ms: Num,
    pub beats_per_bar: i128,
    pub beat_times_ms: Option<Vec<Num>>,
    pub beat_numbers: Option<Vec<i128>>,
    pub source: String,
    pub confidence: Option<Num>,
}

impl BeatGrid {
    pub fn parse(value: &PyValue) -> Result<Self, Invalid> {
        let pairs = object(
            value,
            &["bpm", "first_beat_ms", "beats_per_bar", "beat_times_ms", "beat_numbers", "source", "confidence"],
        )?;
        let bpm = get(pairs, "bpm").and_then(Num::of).ok_or_else(|| Invalid("bpm required".into()))?;
        let first_beat_ms = finite_number(get(pairs, "first_beat_ms"))?;
        let beats_per_bar = strict_int(get(pairs, "beats_per_bar"))?.unwrap_or(4);
        let beat_times_ms = match get(pairs, "beat_times_ms") {
            None | Some(PyValue::None) => None,
            Some(PyValue::List(items)) => {
                if !(2..=100_000).contains(&items.len()) {
                    return invalid("beat_times_ms length");
                }
                Some(items.iter().map(|item| finite_number(Some(item))).collect::<Result<Vec<_>, _>>()?)
            }
            Some(_) => return invalid("beat_times_ms must be a list"),
        };
        let beat_numbers = match get(pairs, "beat_numbers") {
            None | Some(PyValue::None) => None,
            Some(PyValue::List(items)) => Some(
                items
                    .iter()
                    .map(|item| match item {
                        PyValue::Int(n) if (1..=16).contains(n) => Ok(*n),
                        _ => invalid("beat number"),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Some(_) => return invalid("beat_numbers must be a list"),
        };
        let source = match get(pairs, "source") {
            None => "manual".to_owned(),
            Some(PyValue::Str(source)) if ["rekordbox", "analysis", "manual"].contains(&source.as_str()) => source.clone(),
            Some(_) => return invalid("invalid source"),
        };
        let confidence = match get(pairs, "confidence") {
            None | Some(PyValue::None) => None,
            Some(value) => match Num::of(value) {
                Some(number) if number.value() >= 0.0 => Some(number),
                _ => return invalid("invalid confidence"),
            },
        };
        let grid = Self { bpm, first_beat_ms, beats_per_bar, beat_times_ms, beat_numbers, source, confidence };
        grid.check()?;
        Ok(grid)
    }

    /// The field constraints and `validate_beats` / `finite_number`.
    pub fn check(&self) -> Result<(), Invalid> {
        let bpm = self.bpm.value();
        if !(20.0..=300.0).contains(&bpm) {
            return invalid("bpm out of range");
        }
        if !(1..=16).contains(&self.beats_per_bar) {
            return invalid("beats_per_bar out of range");
        }
        if self.confidence.is_some_and(|c| !c.value().is_finite()) {
            return invalid("confidence must be finite");
        }
        if let Some(times) = &self.beat_times_ms {
            if times.windows(2).any(|pair| pair[1].value() <= pair[0].value()) {
                return invalid("beat times must be strictly increasing");
            }
            if (self.first_beat_ms.value() - times[0].value()).abs() > 0.01 {
                return invalid("first_beat_ms must match the first beat timestamp");
            }
        }
        if let Some(numbers) = &self.beat_numbers {
            if self.beat_times_ms.as_ref().is_none_or(|times| times.len() != numbers.len()) {
                return invalid("beat_numbers must match beat_times_ms length");
            }
            if numbers.iter().any(|n| *n > self.beats_per_bar) {
                return invalid("beat numbers must fit beats_per_bar");
            }
        }
        if !bpm.is_finite() || !self.first_beat_ms.value().is_finite() {
            return invalid("must be finite");
        }
        Ok(())
    }

    /// The last beat (or the first when there is no beat list).
    pub fn last_ms(&self) -> f64 {
        self.beat_times_ms.as_ref().and_then(|times| times.last()).map_or(self.first_beat_ms.value(), |t| t.value())
    }

    pub fn json(&self) -> PyValue {
        let list = |items: &Option<Vec<Num>>| items.as_ref().map_or(PyValue::None, |items| PyValue::List(items.iter().map(|n| n.json()).collect()));
        dict(vec![
            ("bpm", self.bpm.json()),
            ("first_beat_ms", self.first_beat_ms.json()),
            ("beats_per_bar", PyValue::Int(self.beats_per_bar)),
            ("beat_times_ms", list(&self.beat_times_ms)),
            ("beat_numbers", self.beat_numbers.as_ref().map_or(PyValue::None, |n| PyValue::List(n.iter().map(|x| PyValue::Int(*x)).collect()))),
            ("source", PyValue::Str(self.source.clone())),
            ("confidence", self.confidence.map_or(PyValue::None, Num::json)),
        ])
    }
}

fn dict(pairs: Vec<(&str, PyValue)>) -> PyValue {
    PyValue::Dict(pairs.into_iter().map(|(key, value)| (PyValue::Str(key.into()), value)).collect())
}

/// `PerformanceMetadataWrite`.
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataWrite {
    pub revision: i128,
    pub cue_points: Vec<CuePoint>,
    pub loops: Vec<SavedLoop>,
    pub beat_grid: Option<BeatGrid>,
}

impl MetadataWrite {
    pub fn parse(value: &PyValue) -> Result<Self, Invalid> {
        let pairs = object(value, &["revision", "cue_points", "loops", "beat_grid"])?;
        let revision = strict_int(get(pairs, "revision"))?.ok_or_else(|| Invalid("revision required".into()))?;
        if revision < 0 {
            return invalid("revision must be >= 0");
        }
        let list = |name: &str, max: usize| -> Result<Vec<PyValue>, Invalid> {
            match get(pairs, name) {
                None => Ok(Vec::new()),
                Some(PyValue::List(items)) if items.len() <= max => Ok(items.clone()),
                Some(_) => invalid("invalid list"),
            }
        };
        let cue_points = list("cue_points", 16)?.iter().map(CuePoint::parse).collect::<Result<Vec<_>, _>>()?;
        let loops = list("loops", 32)?.iter().map(SavedLoop::parse).collect::<Result<Vec<_>, _>>()?;
        let beat_grid = match get(pairs, "beat_grid") {
            None | Some(PyValue::None) => None,
            Some(grid) => Some(BeatGrid::parse(grid)?),
        };
        let mut slots: Vec<i128> = cue_points.iter().map(|cue| cue.slot).collect();
        slots.sort_unstable();
        if slots.windows(2).any(|pair| pair[0] == pair[1]) {
            return invalid("cue point slots must be unique");
        }
        let mut ids: Vec<&str> = loops.iter().map(|l| l.id.as_str()).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return invalid("loop ids must be unique");
        }
        Ok(Self { revision, cue_points, loops, beat_grid })
    }

    /// `_validate_duration`: the service's messages (422).
    pub fn check_duration(&self, duration_s: Option<f64>) -> Result<(), Invalid> {
        let duration_ms = duration_s.filter(|d| *d != 0.0).unwrap_or(0.0) * 1000.0;
        if !(duration_ms > 0.0) {
            return Ok(());
        }
        for cue in &self.cue_points {
            if cue.position_ms.value() >= duration_ms {
                return Err(Invalid(format!("cue point slot {} must be before the track duration", cue.slot)));
            }
        }
        for item in &self.loops {
            if item.end_ms.value() > duration_ms {
                return Err(Invalid(format!("loop {} must end within the track duration", item.id)));
            }
        }
        if let Some(grid) = &self.beat_grid {
            if grid.first_beat_ms.value() >= duration_ms {
                return invalid("beat_grid.first_beat_ms must be before the track duration");
            }
            if grid.beat_times_ms.as_ref().and_then(|t| t.last()).is_some_and(|last| last.value() >= duration_ms) {
                return invalid("Every beat_grid.beat_times_ms entry must be before the track duration");
            }
        }
        Ok(())
    }
}

/// `VERSION` of beat_grid.py (cache key suffix).
pub const GRID_VERSION: i64 = 2;

/// `playback_grid(ticks, bpm, confidence)`: `Ok(None)` without enough beats
/// or a usable tempo; `Err` for invalid timestamps (a ValueError).
pub fn playback_grid(ticks: &[f64], bpm: f64, confidence: Option<Num>) -> Result<Option<BeatGrid>, Invalid> {
    if ticks.len() < 2 || !bpm.is_finite() || !(20.0..=300.0).contains(&bpm) {
        return Ok(None);
    }
    if ticks.iter().any(|t| !t.is_finite() || *t < 0.0) || ticks.windows(2).any(|p| p[1] <= p[0]) {
        return invalid("Invalid beat timestamps");
    }
    let mut grid = BeatGrid {
        bpm: Num::Float(bpm),
        first_beat_ms: Num::Float(ticks[0] * 1000.0),
        beats_per_bar: 4,
        beat_times_ms: Some(ticks.iter().map(|t| Num::Float(t * 1000.0)).collect()),
        beat_numbers: None,
        source: "analysis".into(),
        confidence,
    };
    if ticks.len() < 16 {
        return Ok(Some(grid));
    }
    let n = ticks.len() as f64;
    let center = (n - 1.0) / 2.0;
    let mean = fsum(ticks.iter().copied()) / n;
    let period = fsum(ticks.iter().enumerate().map(|(i, t)| (i as f64 - center) * (t - mean))) / (n * (n * n - 1.0) / 12.0);
    let origin = mean - period * center;
    let mut residuals: Vec<f64> = ticks.iter().enumerate().map(|(i, t)| (t - (origin + i as f64 * period)).abs()).collect();
    residuals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = residuals[((n * 0.95).ceil() as usize).saturating_sub(1)];
    if origin >= 0.0 && (0.2..=3.0).contains(&period) && p95 <= 0.020 && *residuals.last().expect("ticks") <= 0.050 {
        grid.bpm = Num::Float(60.0 / period);
        grid.first_beat_ms = Num::Float(origin * 1000.0);
        grid.beat_times_ms = None;
    }
    Ok(Some(grid))
}

/// `math.fsum`: exactly rounded sum (Shewchuk partials, as CPython).
pub fn fsum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut partials: Vec<f64> = Vec::new();
    for mut x in values {
        let mut kept = 0;
        for i in 0..partials.len() {
            let mut y = partials[i];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[kept] = lo;
                kept += 1;
            }
            x = hi;
        }
        partials.truncate(kept);
        partials.push(x);
    }
    // Round the partials as CPython does (half-even across the last two).
    let mut n = partials.len();
    if n == 0 {
        return 0.0;
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0;
    while n > 0 {
        let x = hi;
        n -= 1;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        let yr = x - hi;
        if y == yr {
            hi = x;
        }
    }
    hi
}

/// `adjust_grid(grid, offset_ms)`: move a rekordbox grid onto the decoder's
/// clock. `Err` carries `RekordboxTimingError`'s message.
pub fn adjust_grid(grid: &BeatGrid, offset_ms: f64) -> Result<BeatGrid, Invalid> {
    if offset_ms == 0.0 {
        return Ok(grid.clone());
    }
    let mut adjusted = grid.clone();
    if let Some(times) = grid.beat_times_ms.as_ref().filter(|t| !t.is_empty()) {
        let kept: Vec<(usize, f64)> =
            times.iter().enumerate().filter(|(_, t)| t.value() >= offset_ms).map(|(i, t)| (i, t.value() - offset_ms)).collect();
        if kept.len() < 2 {
            return invalid("時刻補正後のビートが不足しています");
        }
        adjusted.beat_times_ms = Some(kept.iter().map(|(_, t)| Num::Float(*t)).collect());
        adjusted.first_beat_ms = Num::Float(kept[0].1);
        if let Some(numbers) = grid.beat_numbers.as_ref().filter(|n| !n.is_empty()) {
            adjusted.beat_numbers = Some(kept.iter().map(|(i, _)| numbers[*i]).collect());
        }
    } else {
        let mut first = grid.first_beat_ms.value() - offset_ms;
        let period = 60_000.0 / grid.bpm.value();
        while first < 0.0 {
            first += period;
        }
        adjusted.first_beat_ms = Num::Float(first);
    }
    adjusted.check()?;
    Ok(adjusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumdeck_core::compat::pyjson_loads::loads;

    #[test]
    fn write_payload_follows_the_models() {
        let ok = loads(r#"{"revision": 0, "cue_points": [{"slot": 1, "position_ms": 10}], "loops": [{"id": "a", "start_ms": 1, "end_ms": 2.5}],
            "beat_grid": {"bpm": 120, "first_beat_ms": 0, "beat_times_ms": [0, 500], "beat_numbers": [1, 2]}}"#).unwrap();
        let write = MetadataWrite::parse(&ok).unwrap();
        assert_eq!(write.cue_points[0].position_ms, Num::Int(10));
        assert!(write.check_duration(Some(0.4)).is_err());
        for bad in [
            r#"{"revision": -1}"#,
            r#"{"revision": true}"#,
            r#"{"revision": 0, "extra": 1}"#,
            r#"{"revision": 0, "cue_points": [{"slot": 1, "position_ms": 1}, {"slot": 1, "position_ms": 2}]}"#,
            r#"{"revision": 0, "loops": [{"id": " ", "start_ms": 1, "end_ms": 2}]}"#,
            r#"{"revision": 0, "beat_grid": {"bpm": 10, "first_beat_ms": 0}}"#,
            r#"{"revision": 0, "beat_grid": {"bpm": 120, "first_beat_ms": 1, "beat_times_ms": [0, 500]}}"#,
        ] {
            assert!(MetadataWrite::parse(&loads(bad).unwrap()).is_err(), "{bad}");
        }
    }

    #[test]
    fn fits_a_constant_tempo_and_shifts_grids() {
        let ticks: Vec<f64> = (0..32).map(|i| 0.1 + i as f64 * 0.5).collect();
        let grid = playback_grid(&ticks, 120.0, None).unwrap().unwrap();
        assert!(grid.beat_times_ms.is_none() && (grid.bpm.value() - 120.0).abs() < 1e-9);
        assert_eq!(fsum([1e16, 1.0, -1e16]), 1.0);
        let shifted = adjust_grid(&BeatGrid { beat_times_ms: None, ..grid.clone() }, 200.0).unwrap();
        assert!((shifted.first_beat_ms.value() - 400.0).abs() < 1e-9);
    }
}
