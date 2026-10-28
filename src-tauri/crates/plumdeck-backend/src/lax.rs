//! pydantic v2's lax-mode coercion of scalars, as FastAPI validates query
//! parameters (strings) and JSON bodies (`json.loads` values). Checked
//! against contracts/compat-vectors/pydantic_lax.jsonl
//! (tools/parity/export_validation_vectors.py).

use plumdeck_core::compat::pyjson::PyValue;

/// jiter's `NumberInt::try_from`: a JSON integer (`-?(0|[1-9][0-9]*)`),
/// the whole input.
fn json_int(text: &str) -> Option<i128> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let valid = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    if !valid {
        return None;
    }
    text.parse().ok()
}

/// `strip_underscores`: no leading, trailing or doubled underscore; `None`
/// also when there is none to strip.
fn strip_underscores(text: &str) -> Option<String> {
    if text.starts_with('_') || text.ends_with('_') || !text.contains('_') || text.contains("__") {
        None
    } else {
        Some(text.replace('_', ""))
    }
}

/// `strip_leading_zeros`: zeros and underscores before the first other
/// digit or `-` go; one zero is kept before a `.` or when nothing else is
/// left.
fn strip_leading_zeros(text: &str) -> Option<&str> {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, '0')) => {}
        Some((_, c)) if c.is_ascii_digit() || c == '-' => return Some(text),
        _ => return None,
    }
    for (at, c) in chars {
        match c {
            '0' | '_' => {}
            '1'..='9' | '-' => return Some(&text[at..]),
            '.' => return Some(&text[at - 1..]),
            _ => return None,
        }
    }
    Some(&text[text.len() - 1..])
}

/// `clean_int_str`.
fn clean_int_str(text: &str) -> Option<String> {
    let len_before = text.len();
    let mut s = text.trim();
    if let Some(suffix) = s.strip_prefix('+') {
        if suffix.starts_with('-') {
            return None;
        }
        s = suffix;
    }
    let mut negative = false;
    if let Some(suffix) = s.strip_prefix('-') {
        if suffix.starts_with('-') || suffix.starts_with('+') {
            return None;
        }
        negative = true;
        s = suffix;
    }
    s = strip_leading_zeros(s)?;
    if let Some(dot) = s.find('.') {
        let decimals = &s[dot + 1..];
        if !decimals.is_empty() && decimals.chars().all(|c| c == '0') {
            s = &s[..dot];
        }
    }
    let sign = if negative { "-" } else { "" };
    if let Some(stripped) = strip_underscores(s) {
        return Some(format!("{sign}{stripped}"));
    }
    if len_before == s.len() {
        return None;
    }
    Some(format!("{sign}{s}"))
}

/// `str_as_int` (pydantic-core src/input/shared.rs), as ported: a JSON
/// integer, else the cleaned string (whitespace, `+`, leading zeros, `.0`,
/// underscores) as one. Integers beyond i128 are refused here (Python
/// accepted them; DuckDB then failed).
pub fn int_from_str(text: &str) -> Option<i128> {
    json_int(text).or_else(|| clean_int_str(text).and_then(|cleaned| json_int(&cleaned)))
}

/// `str_as_float`: Rust's f64 parser on the trimmed string, else on the
/// untrimmed string without its underscores.
pub fn float_from_str(text: &str) -> Option<f64> {
    text.trim().parse().ok().or_else(|| strip_underscores(text).and_then(|stripped| stripped.parse().ok()))
}

const TRUE_WORDS: [&str; 6] = ["1", "on", "t", "true", "y", "yes"];
const FALSE_WORDS: [&str; 6] = ["0", "off", "f", "false", "n", "no"];

/// `int` in lax mode.
pub fn to_int(value: &PyValue) -> Option<i128> {
    match value {
        PyValue::Bool(flag) => Some(i128::from(*flag)),
        PyValue::Int(number) => Some(*number),
        PyValue::Float(number) => {
            let in_range = number.is_finite() && number.fract() == 0.0 && number.abs() <= 9.223_372_036_854_775e18;
            in_range.then_some(*number as i128)
        }
        PyValue::Str(text) => int_from_str(text),
        _ => None,
    }
}

/// `float` in lax mode.
pub fn to_float(value: &PyValue) -> Option<f64> {
    match value {
        PyValue::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        PyValue::Int(number) => Some(*number as f64),
        PyValue::Float(number) => Some(*number),
        PyValue::Str(text) => float_from_str(text),
        _ => None,
    }
}

/// `bool` in lax mode.
pub fn to_bool(value: &PyValue) -> Option<bool> {
    match value {
        PyValue::Bool(flag) => Some(*flag),
        PyValue::Int(0) => Some(false),
        PyValue::Int(1) => Some(true),
        PyValue::Float(number) if *number == 0.0 => Some(false),
        PyValue::Float(number) if *number == 1.0 => Some(true),
        PyValue::Str(text) => {
            let lowered = text.to_ascii_lowercase();
            if TRUE_WORDS.contains(&lowered.as_str()) {
                Some(true)
            } else if FALSE_WORDS.contains(&lowered.as_str()) {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `str` in lax mode (strings only).
pub fn to_str(value: &PyValue) -> Option<&str> {
    match value {
        PyValue::Str(text) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn decode(value: &Value) -> PyValue {
        match value {
            Value::Null => PyValue::None,
            Value::Bool(flag) => PyValue::Bool(*flag),
            Value::Object(map) => {
                let (kind, inner) = map.iter().next().unwrap();
                match kind.as_str() {
                    "int" => PyValue::Int(inner.as_str().unwrap().parse().unwrap()),
                    "float" => {
                        let bits = u64::from_str_radix(inner.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
                        PyValue::Float(f64::from_bits(bits))
                    }
                    "str" => PyValue::Str(inner.as_str().unwrap().into()),
                    "list" => PyValue::List(inner.as_array().unwrap().iter().map(decode).collect()),
                    "dict" => PyValue::Dict(
                        inner.as_array().unwrap().iter().map(|pair| (decode(&pair[0]), decode(&pair[1]))).collect(),
                    ),
                    other => panic!("unknown kind {other}"),
                }
            }
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn matches_pydantic_vectors() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../contracts/compat-vectors/pydantic_lax.jsonl");
        let text = std::fs::read_to_string(path).unwrap();
        let mut checked = 0;
        for line in text.lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            let input = decode(&row["input"]);
            let expected = row["ok"].as_bool().unwrap().then(|| decode(&row["value"]));
            let got = match row["type"].as_str().unwrap() {
                "int" => to_int(&input).map(PyValue::Int),
                "float" => to_float(&input).map(PyValue::Float),
                "bool" => to_bool(&input).map(PyValue::Bool),
                "str" => to_str(&input).map(|text| PyValue::Str(text.into())),
                other => panic!("unknown type {other}"),
            };
            let same = match (&got, &expected) {
                (Some(PyValue::Float(a)), Some(PyValue::Float(b))) => a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()),
                _ => got == expected,
            };
            assert!(same, "{line}: got {got:?}");
            checked += 1;
        }
        assert!(checked > 9000);
    }
}
