//! FastAPI's query and path parameters.
//!
//! Starlette parses the query string with `parse_qsl(keep_blank_values=True)`
//! (split on `&`, `+` is a space, `%XX` decoded as UTF-8 with replacement);
//! a scalar parameter takes the last value of its name, a list parameter
//! every value. Values are validated in pydantic's lax mode ([`crate::lax`]);
//! an empty string is a value, not a missing parameter. Every error of a
//! request is collected and answered as one 422 (FastAPI validates path and
//! query parameters and the body together).

use axum::http::Uri;
use axum::response::Response;
use plumdeck_core::compat::pyjson::PyValue;

use crate::body::{validation_response, Loc, ValidationError};
use crate::lax::{float_from_str, int_from_str};

/// `unquote_plus(text, errors="replace")` over a Latin-1 decoded string.
fn unquote_plus(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending: Vec<u8> = Vec::new();
    let flush = |pending: &mut Vec<u8>, out: &mut String| {
        if !pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(pending));
            pending.clear();
        }
    };
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '%' && at + 2 < chars.len() {
            let hex: String = chars[at + 1..=at + 2].iter().collect();
            if hex.chars().all(|h| h.is_ascii_hexdigit()) {
                pending.push(u8::from_str_radix(&hex, 16).expect("hex"));
                at += 3;
                continue;
            }
        }
        flush(&mut pending, &mut out);
        out.push(if c == '+' { ' ' } else { c });
        at += 1;
    }
    flush(&mut pending, &mut out);
    out
}

/// The query string's `(name, value)` pairs, in order.
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (name, value) = field.split_once('=').unwrap_or((field, ""));
            (unquote_plus(name), unquote_plus(value))
        })
        .collect()
}

/// The raw query of a request, decoded as Latin-1 (as Starlette reads
/// `scope["query_string"]`).
fn latin1_query(uri: &Uri) -> String {
    uri.query().map(|query| query.bytes().map(char::from).collect()).unwrap_or_default()
}

/// One request's parameters and the validation errors found so far.
pub struct Params {
    pairs: Vec<(String, String)>,
    errors: Vec<ValidationError>,
}

fn invalid(kind: &'static str, location: &str, name: &str, msg: &str, input: &str) -> ValidationError {
    ValidationError::new(kind, vec![Loc::from(location), Loc::from(name)], msg, PyValue::Str(input.into()))
}

impl Params {
    pub fn from_uri(uri: &Uri) -> Self {
        Self { pairs: parse_query(&latin1_query(uri)), errors: Vec::new() }
    }

    fn last(&self, name: &str) -> Option<&str> {
        self.pairs.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// A list parameter: every value, or `None` when the name is absent.
    pub fn list(&self, name: &str) -> Option<Vec<String>> {
        let values: Vec<String> =
            self.pairs.iter().filter(|(key, _)| key == name).map(|(_, value)| value.clone()).collect();
        (!values.is_empty()).then_some(values)
    }

    pub fn opt_str(&self, name: &str) -> Option<String> {
        self.last(name).map(str::to_owned)
    }

    pub fn str_or(&self, name: &str, default: &str) -> String {
        self.last(name).unwrap_or(default).to_owned()
    }

    /// A required `str` parameter.
    pub fn required_str(&mut self, name: &str) -> Option<String> {
        let value = self.opt_str(name);
        if value.is_none() {
            self.missing("query", name);
        }
        value
    }

    fn missing(&mut self, location: &str, name: &str) {
        self.errors.push(ValidationError::new(
            "missing",
            vec![Loc::from(location), Loc::from(name)],
            "Field required",
            PyValue::None,
        ));
    }

    pub fn opt_int(&mut self, name: &str) -> Option<i128> {
        let text = self.last(name)?.to_owned();
        let parsed = int_from_str(&text);
        if parsed.is_none() {
            self.errors.push(invalid(
                "int_parsing",
                "query",
                name,
                "Input should be a valid integer, unable to parse string as an integer",
                &text,
            ));
        }
        parsed
    }

    pub fn int_or(&mut self, name: &str, default: i128) -> i128 {
        if self.last(name).is_none() {
            return default;
        }
        self.opt_int(name).unwrap_or(default)
    }

    pub fn required_int(&mut self, name: &str) -> Option<i128> {
        if self.last(name).is_none() {
            self.missing("query", name);
            return None;
        }
        self.opt_int(name)
    }

    pub fn opt_float(&mut self, name: &str) -> Option<f64> {
        let text = self.last(name)?.to_owned();
        let parsed = float_from_str(&text);
        if parsed.is_none() {
            self.errors.push(invalid(
                "float_parsing",
                "query",
                name,
                "Input should be a valid number, unable to parse string as a number",
                &text,
            ));
        }
        parsed
    }

    pub fn float_or(&mut self, name: &str, default: f64) -> f64 {
        if self.last(name).is_none() {
            return default;
        }
        self.opt_float(name).unwrap_or(default)
    }

    /// `Query(..., ge=, le=)` on an int.
    pub fn bounded_int(&mut self, name: &str, default: i128, min: i128, max: Option<i128>) -> i128 {
        let value = self.int_or(name, default);
        let text = self.last(name).unwrap_or_default().to_owned();
        if value < min {
            self.errors.push(invalid("greater_than_equal", "query", name, "Input should be greater than or equal", &text));
        } else if max.is_some_and(|max| value > max) {
            self.errors.push(invalid("less_than_equal", "query", name, "Input should be less than or equal", &text));
        }
        value
    }

    /// A string restricted to `choices` (an Enum or a full-match pattern).
    pub fn choice(&mut self, name: &str, default: &str, choices: &[&str]) -> String {
        let value = self.str_or(name, default);
        if !choices.contains(&value.as_str()) {
            self.errors.push(invalid("enum", "query", name, "Input should be one of the allowed values", &value));
        }
        value
    }

    /// An int path parameter.
    pub fn path_int(&mut self, name: &str, text: &str) -> Option<i128> {
        let parsed = int_from_str(text);
        if parsed.is_none() {
            self.errors.push(invalid(
                "int_parsing",
                "path",
                name,
                "Input should be a valid integer, unable to parse string as an integer",
                text,
            ));
        }
        parsed
    }

    /// Add body errors (FastAPI reports them after the parameters).
    pub fn extend(&mut self, errors: Vec<ValidationError>) {
        self.errors.extend(errors);
    }

    /// The 422 answer when anything failed.
    pub fn finish(self) -> Result<(), Response> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(validation_response(&self.errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_parse_qsl() {
        assert_eq!(
            parse_query("a=1&&b=&c&d=x+y%20z&e=%E6%97%A5&f=%ff&g=%zz&a=2"),
            [
                ("a", "1"),
                ("b", ""),
                ("c", ""),
                ("d", "x y z"),
                ("e", "日"),
                ("f", "\u{fffd}"),
                ("g", "%zz"),
                ("a", "2")
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
        assert_eq!(parse_query("x=%"), [("x".to_owned(), "%".to_owned())]);
        assert_eq!(parse_query("x=a%4"), [("x".to_owned(), "a%4".to_owned())]);
    }

    #[test]
    fn last_value_wins_and_lists_keep_all() {
        let uri: Uri = "/x?limit=5&limit=7&g=a&g=b&n=".parse().unwrap();
        let mut params = Params::from_uri(&uri);
        assert_eq!(params.int_or("limit", 100), 7);
        assert_eq!(params.list("g"), Some(vec!["a".to_owned(), "b".to_owned()]));
        assert_eq!(params.list("missing"), None);
        assert_eq!(params.opt_float("n"), None);
        assert!(params.finish().is_err());
    }
}
