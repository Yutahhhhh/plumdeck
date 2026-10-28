//! JSON answers rendered the way Starlette's `JSONResponse` renders a
//! route's return value: `json.dumps(ensure_ascii=False, allow_nan=False,
//! separators=(",", ":"))`, so the bytes match Python's (float `repr`,
//! insertion order) and a NaN or Infinity anywhere is a 500, as it is in
//! Python ("Out of range float values are not JSON compliant").
//!
//! Use it for routes whose contract serializer is `json.dumps`
//! (contracts/http/routes.v5.json). Response-model routes go through
//! [`pydantic`]: FastAPI writes them with pydantic-core's `dump_json`, which
//! turns NaN and Infinity into `null` instead of failing.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::time::py_isoformat;

use crate::server_error;

/// A route's return value, after `jsonable_encoder`.
#[derive(Clone, Debug, PartialEq)]
pub struct PyJson(pub PyValue);

impl IntoResponse for PyJson {
    fn into_response(self) -> Response {
        match dumps(&self.0, &DumpOptions::starlette()) {
            Ok(text) => ([(header::CONTENT_TYPE, "application/json")], text).into_response(),
            Err(error) => server_error(&error),
        }
    }
}

/// A `dict` with string keys, in order.
pub fn dict<K: Into<String>>(pairs: impl IntoIterator<Item = (K, PyValue)>) -> PyValue {
    PyValue::Dict(pairs.into_iter().map(|(key, value)| (PyValue::Str(key.into()), value)).collect())
}

pub fn text(value: impl Into<String>) -> PyValue {
    PyValue::Str(value.into())
}

pub fn int(value: i64) -> PyValue {
    PyValue::Int(value.into())
}

/// `None` → `null`.
pub fn opt<T>(value: Option<T>, convert: impl FnOnce(T) -> PyValue) -> PyValue {
    value.map_or(PyValue::None, convert)
}

/// A response-model answer: non-finite floats become `null`
/// (`ser_json_inf_nan="null"`), then the JSON is written like [`PyJson`]
/// (only the float spelling differs from pydantic-core, and the parity
/// runner compares values).
pub fn pydantic(value: PyValue) -> PyJson {
    fn finite(value: PyValue) -> PyValue {
        match value {
            PyValue::Float(number) if !number.is_finite() => PyValue::None,
            PyValue::List(items) => PyValue::List(items.into_iter().map(finite).collect()),
            PyValue::Dict(pairs) => PyValue::Dict(pairs.into_iter().map(|(key, item)| (key, finite(item))).collect()),
            other => other,
        }
    }
    PyJson(finite(value))
}

pub fn float(value: f64) -> PyValue {
    PyValue::Float(value)
}

pub fn boolean(value: bool) -> PyValue {
    PyValue::Bool(value)
}

/// A naive datetime as `jsonable_encoder` writes it (`isoformat()`).
pub fn datetime(value: &NaiveDateTime) -> PyValue {
    PyValue::Str(py_isoformat(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn renders_like_starlette_and_refuses_nan() {
        let value = dict([("a", PyValue::Float(1e-7)), ("b", text("日本")), ("c", opt(None::<i64>, int))]);
        let response = PyJson(value).into_response();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], r#"{"a":1e-07,"b":"日本","c":null}"#.as_bytes());
        let response = PyJson(dict([("x", PyValue::Float(f64::NAN))])).into_response();
        assert_eq!(response.status(), 500);
        assert_eq!(&to_bytes(response.into_body(), 1024).await.unwrap()[..], b"Internal Server Error");
    }
}
