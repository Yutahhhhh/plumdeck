//! `json.loads` with CPython 3.12's acceptance rules, error messages and
//! error positions (the C scanner's): `NaN`/`Infinity`/`-Infinity` are
//! accepted, integers stay integers, duplicate keys keep the first key's
//! place with the last value, and bytes are decoded the way `json.loads(b)`
//! detects their encoding.
//!
//! Request bodies go through this (Starlette's `request.json()`), so a body
//! FastAPI accepts or rejects is accepted or rejected the same way here.
//!
//! Known gaps, all inputs no client sends: a lone surrogate (`"\ud800"`, or
//! one encoded in the bytes, which `surrogatepass` lets through) becomes
//! U+FFFD because a Rust string cannot hold it; an integer outside i128 becomes
//! a float.

use std::fmt;

use super::pyjson::PyValue;

/// `json.JSONDecodeError`: the message and the character index into the
/// decoded text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonDecodeError {
    pub msg: &'static str,
    pub pos: usize,
}

impl fmt::Display for JsonDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (char {})", self.msg, self.pos)
    }
}

impl std::error::Error for JsonDecodeError {}

/// What `json.loads(bytes)` raises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadsError {
    /// `JSONDecodeError`.
    Decode(JsonDecodeError),
    /// `UnicodeDecodeError` (the bytes are not valid in the detected
    /// encoding) or `ValueError` (an integer with more than 4300 digits).
    Other(String),
}

impl fmt::Display for LoadsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => error.fmt(f),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LoadsError {}

/// `json.detect_encoding`.
fn detect_encoding(b: &[u8]) -> &'static str {
    if b.starts_with(&[0, 0, 0xFE, 0xFF]) || b.starts_with(&[0xFF, 0xFE, 0, 0]) {
        return "utf-32";
    }
    if b.starts_with(&[0xFE, 0xFF]) || b.starts_with(&[0xFF, 0xFE]) {
        return "utf-16";
    }
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return "utf-8-sig";
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { "utf-16-be" } else { "utf-32-be" };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 { "utf-16-le" } else { "utf-32-le" };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return "utf-16-be";
        }
        if b[1] == 0 {
            return "utf-16-le";
        }
    }
    "utf-8"
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> Result<String, LoadsError> {
    if bytes.len() % 2 != 0 {
        return Err(LoadsError::Other("'utf-16' codec can't decode: truncated data".into()));
    }
    let units = bytes.chunks_exact(2).map(|pair| {
        if big_endian { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from_le_bytes([pair[0], pair[1]]) }
    });
    Ok(char::decode_utf16(units).map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect())
}

fn decode_utf32(bytes: &[u8], big_endian: bool) -> Result<String, LoadsError> {
    if bytes.len() % 4 != 0 {
        return Err(LoadsError::Other("'utf-32' codec can't decode: truncated data".into()));
    }
    bytes
        .chunks_exact(4)
        .map(|quad| {
            let code = if big_endian {
                u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]])
            } else {
                u32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]])
            };
            match code {
                0xD800..=0xDFFF => Ok(char::REPLACEMENT_CHARACTER),
                _ => char::from_u32(code)
                    .ok_or_else(|| LoadsError::Other("'utf-32' codec can't decode: code point not in range".into())),
            }
        })
        .collect()
}

/// `s.decode(detect_encoding(s), 'surrogatepass')`.
fn decode_bytes(bytes: &[u8]) -> Result<String, LoadsError> {
    let utf8 = |b: &[u8]| {
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|error| LoadsError::Other(format!("'utf-8' codec can't decode: {error}")))
    };
    match detect_encoding(bytes) {
        "utf-8-sig" => utf8(&bytes[3..]),
        "utf-16" => decode_utf16(&bytes[2..], bytes[0] == 0xFE),
        "utf-16-be" => decode_utf16(bytes, true),
        "utf-16-le" => decode_utf16(bytes, false),
        "utf-32" => decode_utf32(&bytes[4..], bytes[0] == 0),
        "utf-32-be" => decode_utf32(bytes, true),
        "utf-32-le" => decode_utf32(bytes, false),
        _ => utf8(bytes),
    }
}

/// `json.loads(bytes)`.
pub fn loads_bytes(bytes: &[u8]) -> Result<PyValue, LoadsError> {
    let text = decode_bytes(bytes)?;
    loads_decoded(&text)
}

/// `json.loads(str)`; a leading BOM is an error, as in Python.
pub fn loads(text: &str) -> Result<PyValue, LoadsError> {
    if text.starts_with('\u{feff}') {
        return Err(LoadsError::Decode(JsonDecodeError {
            msg: "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            pos: 0,
        }));
    }
    loads_decoded(text)
}

fn loads_decoded(text: &str) -> Result<PyValue, LoadsError> {
    let chars: Vec<char> = text.chars().collect();
    let mut parser = Parser { s: &chars };
    let start = parser.skip_ws(0);
    let (value, end) = match parser.scan_once(start) {
        Ok(found) => found,
        Err(Fail::Stop(pos)) => return Err(decode("Expecting value", pos)),
        Err(Fail::Error(error)) => return Err(error),
    };
    let end = parser.skip_ws(end);
    if end != chars.len() {
        return Err(decode("Extra data", end));
    }
    Ok(value)
}

fn decode(msg: &'static str, pos: usize) -> LoadsError {
    LoadsError::Decode(JsonDecodeError { msg, pos })
}

/// The scanner's two ways out: `StopIteration(idx)` (no value starts here)
/// or a raised error.
enum Fail {
    Stop(usize),
    Error(LoadsError),
}

impl From<LoadsError> for Fail {
    fn from(error: LoadsError) -> Self {
        Self::Error(error)
    }
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// CPython's limit on decimal digits converted to an int.
const INT_MAX_STR_DIGITS: usize = 4300;

struct Parser<'a> {
    s: &'a [char],
}

impl Parser<'_> {
    fn at(&self, idx: usize) -> Option<char> {
        self.s.get(idx).copied()
    }

    fn skip_ws(&self, mut idx: usize) -> usize {
        while self.at(idx).is_some_and(is_ws) {
            idx += 1;
        }
        idx
    }

    fn starts_with(&self, idx: usize, word: &str) -> bool {
        let mut i = idx;
        for c in word.chars() {
            if self.at(i) != Some(c) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn scan_once(&mut self, idx: usize) -> Result<(PyValue, usize), Fail> {
        let Some(next) = self.at(idx) else { return Err(Fail::Stop(idx)) };
        match next {
            '"' => {
                let (text, end) = self.scan_string(idx + 1)?;
                return Ok((PyValue::Str(text), end));
            }
            '{' => return self.parse_object(idx + 1).map_err(Fail::Error),
            '[' => return self.parse_array(idx + 1).map_err(Fail::Error),
            'n' if self.starts_with(idx, "null") => return Ok((PyValue::None, idx + 4)),
            't' if self.starts_with(idx, "true") => return Ok((PyValue::Bool(true), idx + 4)),
            'f' if self.starts_with(idx, "false") => return Ok((PyValue::Bool(false), idx + 5)),
            _ => {}
        }
        if let Some(found) = self.match_number(idx)? {
            return Ok(found);
        }
        match next {
            'N' if self.starts_with(idx, "NaN") => Ok((PyValue::Float(f64::NAN), idx + 3)),
            'I' if self.starts_with(idx, "Infinity") => Ok((PyValue::Float(f64::INFINITY), idx + 8)),
            '-' if self.starts_with(idx, "-Infinity") => Ok((PyValue::Float(f64::NEG_INFINITY), idx + 9)),
            _ => Err(Fail::Stop(idx)),
        }
    }

    /// `(-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?`
    fn match_number(&self, idx: usize) -> Result<Option<(PyValue, usize)>, LoadsError> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let mut i = idx;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('0') => i += 1,
            Some('1'..='9') => {
                while digit(i) {
                    i += 1;
                }
            }
            _ => return Ok(None),
        }
        let int_end = i;
        let mut is_float = false;
        if self.at(i) == Some('.') && digit(i + 1) {
            i += 1;
            while digit(i) {
                i += 1;
            }
            is_float = true;
        }
        if matches!(self.at(i), Some('e' | 'E')) {
            let mut j = i + 1;
            if matches!(self.at(j), Some('+' | '-')) {
                j += 1;
            }
            if digit(j) {
                while digit(j) {
                    j += 1;
                }
                i = j;
                is_float = true;
            }
        }
        let text: String = self.s[idx..i].iter().collect();
        if is_float {
            // float() rounds correctly, as Rust's parser does; overflow is inf.
            let value: f64 = text.parse().map_err(|_| LoadsError::Other(format!("could not convert {text:?}")))?;
            return Ok(Some((PyValue::Float(value), i)));
        }
        let digits = int_end - idx - usize::from(self.at(idx) == Some('-'));
        if digits > INT_MAX_STR_DIGITS {
            return Err(LoadsError::Other(format!(
                "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {digits} digits"
            )));
        }
        let value = match text.parse::<i128>() {
            Ok(int) => PyValue::Int(int),
            Err(_) => PyValue::Float(text.parse().unwrap_or(f64::NAN)),
        };
        Ok(Some((value, i)))
    }

    /// The four hex digits after the `u` at `pos`; errors point at the `u`.
    fn hex4(&self, pos: usize) -> Result<u32, LoadsError> {
        let mut value = 0u32;
        for k in 1..=4 {
            match self.at(pos + k).and_then(|c| c.to_digit(16)) {
                Some(d) => value = value * 16 + d,
                None => return Err(decode("Invalid \\uXXXX escape", pos)),
            }
        }
        Ok(value)
    }

    /// `scanstring(s, end)`: `end` is just past the opening quote.
    fn scan_string(&self, end: usize) -> Result<(String, usize), LoadsError> {
        let begin = end - 1;
        let mut out = String::new();
        let mut i = end;
        loop {
            let Some(c) = self.at(i) else { return Err(decode("Unterminated string starting at", begin)) };
            i += 1;
            match c {
                '"' => return Ok((out, i)),
                '\\' => {}
                c if (c as u32) < 0x20 => return Err(decode("Invalid control character at", i - 1)),
                c => {
                    out.push(c);
                    continue;
                }
            }
            let Some(esc) = self.at(i) else { return Err(decode("Unterminated string starting at", begin)) };
            if esc != 'u' {
                let decoded = match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(decode("Invalid \\escape", i - 1)),
                };
                out.push(decoded);
                i += 1;
                continue;
            }
            let mut code = self.hex4(i)?;
            i += 5;
            if (0xD800..=0xDBFF).contains(&code) && self.at(i) == Some('\\') && self.at(i + 1) == Some('u') {
                let low = self.hex4(i + 1)?;
                if (0xDC00..=0xDFFF).contains(&low) {
                    code = 0x10000 + (((code - 0xD800) << 10) | (low - 0xDC00));
                    i += 6;
                }
            }
            out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
    }

    fn parse_object(&mut self, mut end: usize) -> Result<(PyValue, usize), LoadsError> {
        let mut pairs: Vec<(PyValue, PyValue)> = Vec::new();
        let mut next = self.at(end);
        if next != Some('"') {
            if next.is_some_and(is_ws) {
                end = self.skip_ws(end);
                next = self.at(end);
            }
            if next == Some('}') {
                return Ok((PyValue::Dict(pairs), end + 1));
            }
            if next != Some('"') {
                return Err(decode("Expecting property name enclosed in double quotes", end));
            }
        }
        end += 1;
        loop {
            let (key, after_key) = self.scan_string(end)?;
            end = after_key;
            if self.at(end) != Some(':') {
                end = self.skip_ws(end);
                if self.at(end) != Some(':') {
                    return Err(decode("Expecting ':' delimiter", end));
                }
            }
            end = self.skip_ws(end + 1);
            let (value, after_value) = match self.scan_once(end) {
                Ok(found) => found,
                Err(Fail::Stop(pos)) => return Err(decode("Expecting value", pos)),
                Err(Fail::Error(error)) => return Err(error),
            };
            end = after_value;
            // dict(pairs): a repeated key keeps its first position.
            match pairs.iter_mut().find(|(k, _)| matches!(k, PyValue::Str(existing) if *existing == key)) {
                Some(slot) => slot.1 = value,
                None => pairs.push((PyValue::Str(key), value)),
            }
            end = self.skip_ws(end);
            let next = self.at(end);
            end += 1;
            match next {
                Some('}') => break,
                Some(',') => {}
                _ => return Err(decode("Expecting ',' delimiter", end - 1)),
            }
            end = self.skip_ws(end);
            let next = self.at(end);
            end += 1;
            if next != Some('"') {
                return Err(decode("Expecting property name enclosed in double quotes", end - 1));
            }
        }
        Ok((PyValue::Dict(pairs), end))
    }

    fn parse_array(&mut self, mut end: usize) -> Result<(PyValue, usize), LoadsError> {
        let mut values = Vec::new();
        end = self.skip_ws(end);
        if self.at(end) == Some(']') {
            return Ok((PyValue::List(values), end + 1));
        }
        loop {
            let (value, after) = match self.scan_once(end) {
                Ok(found) => found,
                Err(Fail::Stop(pos)) => return Err(decode("Expecting value", pos)),
                Err(Fail::Error(error)) => return Err(error),
            };
            values.push(value);
            end = self.skip_ws(after);
            let next = self.at(end);
            end += 1;
            match next {
                Some(']') => break,
                Some(',') => {}
                _ => return Err(decode("Expecting ',' delimiter", end - 1)),
            }
            end = self.skip_ws(end);
        }
        Ok((PyValue::List(values), end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> (&'static str, usize) {
        match loads_bytes(text.as_bytes()) {
            Err(LoadsError::Decode(JsonDecodeError { msg, pos })) => (msg, pos),
            other => panic!("{text:?}: {other:?}"),
        }
    }

    fn s(text: &str) -> PyValue {
        PyValue::Str(text.into())
    }

    #[test]
    fn accepts_what_python_accepts() {
        let value = loads_bytes(br#" {"a": [1, -0, 1.5e3, NaN, -Infinity], "b": null, "a": true} "#).unwrap();
        let PyValue::Dict(pairs) = value else { panic!() };
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], (s("a"), PyValue::Bool(true)));
        assert_eq!(pairs[1], (s("b"), PyValue::None));
        let PyValue::List(items) = loads(r#"[1, -0, 1.5e3, NaN, 12345678901234567890123]"#).unwrap() else { panic!() };
        assert_eq!(items[..3], [PyValue::Int(1), PyValue::Int(0), PyValue::Float(1500.0)]);
        assert!(matches!(items[3], PyValue::Float(f) if f.is_nan()));
        assert_eq!(items[4], PyValue::Int(12345678901234567890123));
        assert_eq!(loads(r#""🎵 ß\/""#).unwrap(), s("🎵 ß/"));
        assert_eq!(loads_bytes(b"\xef\xbb\xbf\"x\"").unwrap(), s("x"));
        assert_eq!(loads_bytes(&[0x22, 0, 0x78, 0, 0x22, 0]).unwrap(), s("x"));
        assert_eq!(loads_bytes(b"1e400").unwrap(), PyValue::Float(f64::INFINITY));
    }

    #[test]
    fn errors_match_cpython_messages_and_positions() {
        // Expected values from CPython 3.12 json.loads.
        assert_eq!(err(""), ("Expecting value", 0));
        assert_eq!(err(" "), ("Expecting value", 1));
        assert_eq!(err("{bad"), ("Expecting property name enclosed in double quotes", 1));
        assert_eq!(err(r#"{"a" 1}"#), ("Expecting ':' delimiter", 5));
        assert_eq!(err(r#"{"a": 1,}"#), ("Expecting property name enclosed in double quotes", 8));
        assert_eq!(err(r#"{"a": 1 "b"}"#), ("Expecting ',' delimiter", 8));
        assert_eq!(err(r#"{"a": }"#), ("Expecting value", 6));
        assert_eq!(err("[1,]"), ("Expecting value", 3));
        assert_eq!(err("[1 2]"), ("Expecting ',' delimiter", 3));
        assert_eq!(err("01"), ("Extra data", 1));
        assert_eq!(err("-"), ("Expecting value", 0));
        assert_eq!(err(r#""abc"#), ("Unterminated string starting at", 0));
        assert_eq!(err("\"a\nb\""), ("Invalid control character at", 2));
        assert_eq!(err(r#""\x""#), ("Invalid \\escape", 1));
        assert_eq!(err(r#""\u12G4""#), ("Invalid \\uXXXX escape", 2));
        assert_eq!(err(r#""\ud800\u12G4""#), ("Invalid \\uXXXX escape", 8));
        assert_eq!(err(r#"{"a":1 ,  "b"  :2  ,}"#), ("Expecting property name enclosed in double quotes", 20));
        assert_eq!(err("[1.]"), ("Expecting ',' delimiter", 2));
        assert_eq!(err("[-]"), ("Expecting value", 1));
        assert_eq!(err("1.e5"), ("Extra data", 1));
        assert_eq!(err("[\"日本\", x]"), ("Expecting value", 7));
        assert_eq!(loads("\u{feff}1").unwrap_err(), decode("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
        assert!(matches!(loads_bytes(b"\"\xff\""), Err(LoadsError::Other(_))));
        assert!(matches!(loads_bytes("9".repeat(4301).as_bytes()), Err(LoadsError::Other(_))));
    }
}
