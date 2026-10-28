//! API errors in FastAPI's wire shape: `{"detail": "<text>"}` for business
//! errors, `{"detail": {...}}` for structured ones (e.g. the optimistic-lock
//! 409 with `current_revision`).

use std::fmt;

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::Value;

/// The `detail` member of an error body.
#[derive(Clone, Debug, PartialEq)]
pub enum Detail {
    Text(String),
    Json(Value),
}

impl Serialize for Detail {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(text) => serializer.serialize_str(text),
            Self::Json(value) => value.serialize(serializer),
        }
    }
}

/// An HTTP error response: status plus `{"detail": ...}`.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub detail: Detail,
}

impl ApiError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        Self { status, detail: Detail::Text(detail.into()) }
    }

    pub fn json(status: u16, detail: Value) -> Self {
        Self { status, detail: Detail::Json(detail) }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(400, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(409, detail)
    }

    /// `HTTPException(status_code=404)` without a detail. Unmatched routes
    /// are not this: they get the MCP mount's plain-text 404
    /// (plumdeck_backend::unmatched).
    pub fn not_found() -> Self {
        Self::new(404, "Not Found")
    }

    /// The JSON body, `{"detail": ...}`.
    pub fn body(&self) -> Value {
        serde_json::to_value(self).expect("error body serializes")
    }
}

impl Serialize for ApiError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("detail", &self.detail)?;
        map.end()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Detail::Text(text) => write!(f, "{} {}", self.status, text),
            Detail::Json(value) => write!(f, "{} {}", self.status, value),
        }
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_like_fastapi() {
        assert_eq!(serde_json::to_string(&ApiError::not_found()).unwrap(), r#"{"detail":"Not Found"}"#);
        let conflict = ApiError::json(409, serde_json::json!({"message": "m", "current_revision": 3}));
        assert_eq!(
            serde_json::to_string(&conflict).unwrap(),
            r#"{"detail":{"message":"m","current_revision":3}}"#
        );
    }
}
