//! Planned set timing (backend/domain/services/set_duration.py), the check
//! `POST /api/setlists/{id}/tracks` runs before it stores entries.
//!
//! Entry values arrive untyped (the route stores whatever JSON the client
//! sent), so each is read the way Python's `float()` reads it: numbers and
//! booleans, and strings in `float()` syntax.

use plumdeck_core::compat::pyfmt::py_float_from_str;
use plumdeck_core::compat::pyjson::PyValue;

/// Why a set cannot be timed. `Value` carries the `SetTimingError` message
/// (a `ValueError`); `Overflow` is `float()` of an integer beyond f64, which
/// Python does not catch (a 500).
#[derive(Clone, Debug, PartialEq)]
pub enum SetTimingError {
    Value(String),
    Overflow,
}

/// One entry: its fields as stored, and the track's duration in seconds
/// (the DB column).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub duration: Option<f64>,
    pub in_ms: PyValue,
    pub out_ms: PyValue,
    pub playback_rate: PyValue,
    pub extra_duration_ms: PyValue,
    pub overlap_next_ms: PyValue,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            duration: None,
            in_ms: PyValue::Float(0.0),
            out_ms: PyValue::None,
            playback_rate: PyValue::Float(1.0),
            extra_duration_ms: PyValue::Float(0.0),
            overlap_next_ms: PyValue::Float(0.0),
        }
    }
}

/// `float(value)`: `None` is the TypeError/ValueError.
fn py_float(value: &PyValue) -> Result<Option<f64>, SetTimingError> {
    Ok(match value {
        PyValue::Bool(flag) => Some(f64::from(u8::from(*flag))),
        PyValue::Int(number) => {
            let converted = *number as f64;
            if converted.is_infinite() {
                return Err(SetTimingError::Overflow);
            }
            Some(converted)
        }
        PyValue::Float(number) => Some(*number),
        PyValue::Str(text) => py_float_from_str(text),
        _ => None,
    })
}

/// `_finite(value, name)`.
fn finite(value: &PyValue, name: &str) -> Result<f64, SetTimingError> {
    match py_float(value)? {
        None => Err(SetTimingError::Value(format!("{name} は数値で指定してください"))),
        Some(number) if !number.is_finite() => Err(SetTimingError::Value(format!("{name} は有限の数値で指定してください"))),
        Some(number) => Ok(number),
    }
}

fn invalid() -> SetTimingError {
    SetTimingError::Value("IN/OUT・再生速度・追加時間の値が不正です".into())
}

/// `entry_duration_ms`: the planned real duration of one entry.
pub fn entry_duration_ms(entry: &Entry) -> Result<f64, SetTimingError> {
    let in_ms = finite(&entry.in_ms, "IN")?;
    let rate = finite(&entry.playback_rate, "再生速度")?;
    let out_ms = if entry.out_ms == PyValue::None {
        let end = match entry.duration {
            None => f64::INFINITY,
            Some(seconds) => finite(&PyValue::Float(seconds), "曲の長さ")? * 1000.0,
        };
        end.min(in_ms + 120_000.0 * rate)
    } else {
        finite(&entry.out_ms, "OUT")?
    };
    let extra = finite(&entry.extra_duration_ms, "追加時間")?;
    if in_ms < 0.0 || out_ms <= in_ms || rate <= 0.0 || extra < 0.0 {
        return Err(invalid());
    }
    if let Some(seconds) = entry.duration {
        if out_ms > seconds * 1000.0 + 0.5 {
            return Err(SetTimingError::Value("OUT が曲の長さを超えています".into()));
        }
    }
    Ok((out_ms - in_ms) / rate + extra)
}

/// `calculate_set_duration`: the planned total, or the first error.
pub fn set_duration_ms(entries: &[Entry]) -> Result<f64, SetTimingError> {
    let durations = entries.iter().map(entry_duration_ms).collect::<Result<Vec<_>, _>>()?;
    let mut total: f64 = durations.iter().sum();
    for (index, entry) in entries.iter().enumerate().take(entries.len().saturating_sub(1)) {
        let overlap = finite(&entry.overlap_next_ms, "重なり")?;
        if overlap < 0.0 {
            return Err(SetTimingError::Value("重なりは0以上で指定してください".into()));
        }
        if overlap != 0.0 && overlap > durations[index].min(durations[index + 1]) + 0.5 {
            return Err(SetTimingError::Value("重なりが隣接する曲の予定時間を超えています".into()));
        }
        total -= overlap;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(duration: f64) -> Entry {
        Entry { duration: Some(duration), ..Entry::default() }
    }

    #[test]
    fn plans_two_minutes_by_default_and_checks_bounds() {
        assert_eq!(entry_duration_ms(&entry(300.0)), Ok(120_000.0));
        assert_eq!(entry_duration_ms(&entry(60.0)), Ok(60_000.0));
        let fast = Entry { playback_rate: PyValue::Str(" 2 ".into()), ..entry(600.0) };
        assert_eq!(entry_duration_ms(&fast), Ok(120_000.0));
        let late = Entry { out_ms: PyValue::Int(70_000), ..entry(60.0) };
        assert_eq!(entry_duration_ms(&late), Err(SetTimingError::Value("OUT が曲の長さを超えています".into())));
        let text = Entry { in_ms: PyValue::Str("x".into()), ..entry(60.0) };
        assert_eq!(entry_duration_ms(&text), Err(SetTimingError::Value("IN は数値で指定してください".into())));
        let nan = Entry { in_ms: PyValue::Str("nan".into()), ..entry(60.0) };
        assert_eq!(entry_duration_ms(&nan), Err(SetTimingError::Value("IN は有限の数値で指定してください".into())));
        let unknown = Entry { duration: None, ..Entry::default() };
        assert_eq!(entry_duration_ms(&unknown), Ok(120_000.0));
        assert_eq!(entry_duration_ms(&Entry { playback_rate: PyValue::Int(0), ..entry(60.0) }), Err(invalid()));
    }

    #[test]
    fn overlaps_are_bounded_by_both_neighbours() {
        let a = Entry { overlap_next_ms: PyValue::Float(10_000.0), ..entry(60.0) };
        assert_eq!(set_duration_ms(&[a.clone(), entry(30.0)]), Ok(80_000.0));
        let too_long = Entry { overlap_next_ms: PyValue::Float(40_000.0), ..entry(60.0) };
        assert!(set_duration_ms(&[too_long, entry(30.0)]).is_err());
        // The last entry's overlap is never read.
        let last = Entry { overlap_next_ms: PyValue::Str("x".into()), ..entry(60.0) };
        assert_eq!(set_duration_ms(&[last]), Ok(60_000.0));
        assert_eq!(set_duration_ms(&[]), Ok(0.0));
    }
}
