//! `json.dumps` with Python's exact output: separators, `indent`,
//! `ensure_ascii` (with surrogate pairs), `allow_nan`, `default=str`, float
//! `repr` and dict insertion order.
//!
//! Stored strings depend on these bytes (USB snapshot hashes, wordplay_json,
//! grid fingerprints), so serde_json's own formatting is not a substitute.

use std::fmt;

use chrono::{NaiveDate, NaiveDateTime};

use super::pyfmt::float_repr;
use crate::time::{py_date_isoformat, py_str_datetime};

/// A Python value as `json.dumps` sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum PyValue {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<PyValue>),
    /// Insertion-ordered; keys may be str, int, float, bool or None.
    Dict(Vec<(PyValue, PyValue)>),
    /// Not JSON serializable; `default=str` turns it into `str(datetime)`.
    DateTime(NaiveDateTime),
    /// Not JSON serializable; `default=str` turns it into `date.isoformat()`.
    Date(NaiveDate),
}

impl From<&serde_json::Value> for PyValue {
    fn from(value: &serde_json::Value) -> Self {
        use serde_json::Value;
        match value {
            Value::Null => Self::None,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Self::Int(i.into())
                } else if let Some(u) = n.as_u64() {
                    Self::Int(u.into())
                } else {
                    Self::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => Self::Str(s.clone()),
            Value::Array(items) => Self::List(items.iter().map(Self::from).collect()),
            Value::Object(map) => Self::Dict(
                map.iter().map(|(k, v)| (Self::Str(k.clone()), Self::from(v))).collect(),
            ),
        }
    }
}

/// Keyword arguments of `json.dumps` that the backend uses.
#[derive(Clone, Debug, PartialEq)]
pub struct DumpOptions {
    pub ensure_ascii: bool,
    /// `(item_separator, key_separator)`; `None` is Python's default, which
    /// depends on `indent`.
    pub separators: Option<(String, String)>,
    pub indent: Option<usize>,
    pub allow_nan: bool,
    /// `default=str`.
    pub default_str: bool,
}

impl Default for DumpOptions {
    /// Plain `json.dumps(value)`.
    fn default() -> Self {
        Self { ensure_ascii: true, separators: None, indent: None, allow_nan: true, default_str: false }
    }
}

impl DumpOptions {
    /// `json.dumps(value, ensure_ascii=False)`.
    pub fn unicode() -> Self {
        Self { ensure_ascii: false, ..Self::default() }
    }

    /// What Starlette's `JSONResponse` renders with: `json.dumps(value,
    /// ensure_ascii=False, allow_nan=False, separators=(",", ":"))`.
    pub fn starlette() -> Self {
        Self {
            ensure_ascii: false,
            separators: Some((",".into(), ":".into())),
            allow_nan: false,
            ..Self::default()
        }
    }

    /// `json.dumps(value, ensure_ascii=False, separators=(",", ":"), default=str)`.
    pub fn compact_unicode_default_str() -> Self {
        Self {
            ensure_ascii: false,
            separators: Some((",".into(), ":".into())),
            default_str: true,
            ..Self::default()
        }
    }

    fn item_separator(&self) -> &str {
        match &self.separators {
            Some((item, _)) => item,
            None if self.indent.is_some() => ",",
            None => ", ",
        }
    }

    fn key_separator(&self) -> &str {
        match &self.separators {
            Some((_, key)) => key,
            None => ": ",
        }
    }
}

/// The exception `json.dumps` would raise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DumpError {
    /// `ValueError`: NaN/Infinity with `allow_nan=False`.
    ValueError(String),
    /// `TypeError`: an unserializable value or dict key.
    TypeError(String),
}

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueError(msg) | Self::TypeError(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for DumpError {}

/// `json.dumps(value, **opts)`.
pub fn dumps(value: &PyValue, opts: &DumpOptions) -> Result<String, DumpError> {
    let mut out = String::new();
    Encoder { opts }.value(&mut out, value, 0)?;
    Ok(out)
}

/// `json.dumps` of a serde_json value (numbers keep their int/float kind).
pub fn dumps_json(value: &serde_json::Value, opts: &DumpOptions) -> Result<String, DumpError> {
    dumps(&PyValue::from(value), opts)
}

struct Encoder<'a> {
    opts: &'a DumpOptions,
}

impl Encoder<'_> {
    fn float(&self, x: f64) -> Result<String, DumpError> {
        if x.is_finite() {
            return Ok(float_repr(x));
        }
        if !self.opts.allow_nan {
            return Err(DumpError::ValueError(format!(
                "Out of range float values are not JSON compliant: {}",
                float_repr(x)
            )));
        }
        Ok(if x.is_nan() { "NaN" } else if x > 0.0 { "Infinity" } else { "-Infinity" }.into())
    }

    fn newline(&self, out: &mut String, level: usize) {
        if let Some(indent) = self.opts.indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', indent * level));
        }
    }

    fn value(&self, out: &mut String, value: &PyValue, level: usize) -> Result<(), DumpError> {
        match value {
            PyValue::None => out.push_str("null"),
            PyValue::Bool(true) => out.push_str("true"),
            PyValue::Bool(false) => out.push_str("false"),
            PyValue::Int(i) => out.push_str(&i.to_string()),
            PyValue::Float(x) => out.push_str(&self.float(*x)?),
            PyValue::Str(s) => encode_str(out, s, self.opts.ensure_ascii),
            PyValue::List(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return Ok(());
                }
                out.push('[');
                self.newline(out, level + 1);
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.opts.item_separator());
                        self.newline(out, level + 1);
                    }
                    self.value(out, item, level + 1)?;
                }
                self.newline(out, level);
                out.push(']');
            }
            PyValue::Dict(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return Ok(());
                }
                out.push('{');
                self.newline(out, level + 1);
                for (index, (key, item)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.opts.item_separator());
                        self.newline(out, level + 1);
                    }
                    let key = self.key(key)?;
                    encode_str(out, &key, self.opts.ensure_ascii);
                    out.push_str(self.opts.key_separator());
                    self.value(out, item, level + 1)?;
                }
                self.newline(out, level);
                out.push('}');
            }
            PyValue::DateTime(stamp) => self.fallback(out, "datetime", || py_str_datetime(stamp))?,
            PyValue::Date(day) => self.fallback(out, "date", || py_date_isoformat(day))?,
        }
        Ok(())
    }

    fn fallback(
        &self,
        out: &mut String,
        type_name: &str,
        to_str: impl FnOnce() -> String,
    ) -> Result<(), DumpError> {
        if !self.opts.default_str {
            return Err(DumpError::TypeError(format!(
                "Object of type {type_name} is not JSON serializable"
            )));
        }
        encode_str(out, &to_str(), self.opts.ensure_ascii);
        Ok(())
    }

    fn key(&self, key: &PyValue) -> Result<String, DumpError> {
        Ok(match key {
            PyValue::Str(s) => s.clone(),
            PyValue::Float(x) => self.float(*x)?,
            PyValue::Bool(true) => "true".into(),
            PyValue::Bool(false) => "false".into(),
            PyValue::None => "null".into(),
            PyValue::Int(i) => i.to_string(),
            PyValue::List(_) => return Err(key_type_error("list")),
            PyValue::Dict(_) => return Err(key_type_error("dict")),
            PyValue::DateTime(_) => return Err(key_type_error("datetime")),
            PyValue::Date(_) => return Err(key_type_error("date")),
        })
    }
}

fn key_type_error(type_name: &str) -> DumpError {
    DumpError::TypeError(format!("keys must be str, int, float, bool or None, not {type_name}"))
}

fn push_u_escape(out: &mut String, unit: u32) {
    out.push_str(&format!("\\u{unit:04x}"));
}

/// Python's `encode_basestring` / `encode_basestring_ascii`.
pub fn encode_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => push_u_escape(out, c as u32),
            c if ensure_ascii && !(' '..='~').contains(&c) => {
                let code = c as u32;
                if code >= 0x10000 {
                    let v = code - 0x10000;
                    push_u_escape(out, 0xd800 | (v >> 10));
                    push_u_escape(out, 0xdc00 | (v & 0x3ff));
                } else {
                    push_u_escape(out, code);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> PyValue {
        PyValue::Str(text.into())
    }

    #[test]
    fn default_and_compact_layouts() {
        let value = PyValue::Dict(vec![
            (s("b"), PyValue::Int(1)),
            (s("a"), PyValue::List(vec![PyValue::Float(1e-5), PyValue::Float(1e16)])),
        ]);
        assert_eq!(dumps(&value, &DumpOptions::default()).unwrap(), r#"{"b": 1, "a": [1e-05, 1e+16]}"#);
        assert_eq!(
            dumps(&value, &DumpOptions::compact_unicode_default_str()).unwrap(),
            r#"{"b":1,"a":[1e-05,1e+16]}"#
        );
    }

    #[test]
    fn ascii_escapes_use_surrogate_pairs() {
        let out = dumps(&s("\u{65e5}\u{1f600}\u{7f}"), &DumpOptions::default()).unwrap();
        assert_eq!(out, "\"\\u65e5\\ud83d\\ude00\\u007f\"");
        let out = dumps(&s("\u{65e5}\u{7f}"), &DumpOptions::unicode()).unwrap();
        assert_eq!(out, "\"\u{65e5}\u{7f}\"");
    }
}
