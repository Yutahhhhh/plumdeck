//! Python builtins applied to JSON values (`int()`, `str()`, `repr()`),
//! for routes that take untyped JSON and convert it themselves.

use super::pyfmt::float_repr;
use super::pyjson::PyValue;
use super::text::py_strip;

/// What `int(value)` raised: `ValueError` carries its message (routes
/// often catch it), `TypeError` and `OverflowError` are not caught.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntError {
    Value(String),
    Type,
    Overflow,
}

/// `int(text)` for a `str` in base 10: surrounding whitespace, a sign,
/// leading zeros and single underscores between digits are accepted.
pub fn int_from_str(text: &str) -> Option<i128> {
    let trimmed = py_strip(text);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10)?;
        value = value.checked_mul(10)?.checked_add(i128::from(digit))?;
    }
    Some(if negative { -value } else { value })
}

/// `int(value)`.
pub fn py_int(value: &PyValue) -> Result<i128, IntError> {
    match value {
        PyValue::Bool(flag) => Ok(i128::from(*flag)),
        PyValue::Int(number) => Ok(*number),
        PyValue::Float(number) if number.is_nan() => Err(IntError::Value("cannot convert float NaN to integer".into())),
        PyValue::Float(number) if number.is_infinite() => Err(IntError::Overflow),
        // Floats beyond i128 are exact Python ints; they never name a row.
        PyValue::Float(number) => number.trunc().to_string().parse::<i128>().map_err(|_| IntError::Overflow),
        PyValue::Str(text) => int_from_str(text)
            .ok_or_else(|| IntError::Value(format!("invalid literal for int() with base 10: {}", str_repr(text)))),
        _ => Err(IntError::Type),
    }
}

/// `repr(text)`: single quotes unless the text has a single quote and no
/// double quote; backslash, the quote, `\t` `\n` `\r` and other control
/// characters escaped.
pub fn str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa1).contains(&(c as u32)) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str(value)` for a JSON value (f-string interpolation).
pub fn py_str(value: &PyValue) -> String {
    match value {
        PyValue::None => "None".into(),
        PyValue::Bool(true) => "True".into(),
        PyValue::Bool(false) => "False".into(),
        PyValue::Int(number) => number.to_string(),
        PyValue::Float(number) => float_repr(*number),
        PyValue::Str(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `repr(value)` for a JSON value.
pub fn py_repr(value: &PyValue) -> String {
    match value {
        PyValue::Str(text) => str_repr(text),
        PyValue::List(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        PyValue::Dict(pairs) => format!(
            "{{{}}}",
            pairs.iter().map(|(key, item)| format!("{}: {}", py_repr(key), py_repr(item))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(value: &PyValue) -> bool {
    match value {
        PyValue::None => false,
        PyValue::Bool(flag) => *flag,
        PyValue::Int(number) => *number != 0,
        PyValue::Float(number) => *number != 0.0,
        PyValue::Str(text) => !text.is_empty(),
        PyValue::List(items) => !items.is_empty(),
        PyValue::Dict(pairs) => !pairs.is_empty(),
        PyValue::DateTime(_) | PyValue::Date(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_follows_python() {
        assert_eq!(py_int(&PyValue::Str(" 0_07 ".into())), Ok(7));
        assert_eq!(py_int(&PyValue::Str("-3".into())), Ok(-3));
        assert_eq!(
            py_int(&PyValue::Str("5.0".into())),
            Err(IntError::Value("invalid literal for int() with base 10: '5.0'".into()))
        );
        assert_eq!(py_int(&PyValue::Str("it's".into())), Err(IntError::Value("invalid literal for int() with base 10: \"it's\"".into())));
        assert_eq!(py_int(&PyValue::Float(-2.9)), Ok(-2));
        assert_eq!(py_int(&PyValue::Float(f64::NAN)), Err(IntError::Value("cannot convert float NaN to integer".into())));
        assert_eq!(py_int(&PyValue::Bool(true)), Ok(1));
        assert_eq!(py_int(&PyValue::List(vec![])), Err(IntError::Type));
        assert_eq!(py_int(&PyValue::Str("1__0".into())).is_err(), true);
    }

    #[test]
    fn str_and_repr() {
        assert_eq!(py_str(&PyValue::Float(5.0)), "5.0");
        assert_eq!(py_str(&PyValue::Bool(true)), "True");
        assert_eq!(py_repr(&PyValue::List(vec![PyValue::Int(1), PyValue::Str("a\n".into())])), "[1, 'a\\n']");
        assert!(!truthy(&PyValue::Str(String::new())) && truthy(&PyValue::Int(-1)));
    }
}
