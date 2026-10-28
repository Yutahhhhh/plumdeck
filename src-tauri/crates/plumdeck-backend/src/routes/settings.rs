//! `GET/POST /api/settings` (backend/api/routers/settings.py). The CSV
//! routes under `/api/settings/` arrive with S8.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use plumdeck_core::error::ApiError;
use plumdeck_db::repos::settings::{save_setting, visible_settings};
use plumdeck_db::DbError;

use crate::body::{parse_body, BodyModel, Fields, ValidationError};
use crate::response::{dict, text, PyJson};
use crate::{AppState, HttpError};

/// `SettingUpdate`.
#[derive(Debug, PartialEq)]
pub struct SettingUpdate {
    pub key: String,
    pub value: String,
}

impl BodyModel for SettingUpdate {
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
        let key = fields.required_str("key");
        let value = fields.required_str("value");
        fields.finish(|| Some(Self { key: key?, value: value? }))
    }
}

/// Every stored setting except retired LLM keys, as `{key: value}`.
pub async fn get_settings(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| visible_settings(&conn)))
        .await
        .map(|rows| PyJson(dict(rows.into_iter().map(|(key, value)| (key, text(value))))))
        .into_response()
}

/// Insert or replace one setting; a retired LLM key is a 400.
pub async fn update_setting(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let update: SettingUpdate = match parse_body(&headers, &body) {
        Ok(update) => update,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let saved = state.blocking_result(move || db.write(|conn| save_setting(conn, &update.key, &update.value))).await;
    match saved {
        Ok(Ok((key, value))) => PyJson(dict([("key", text(key)), ("value", text(value))])).into_response(),
        Ok(Err(DbError::Invalid(message))) => HttpError(ApiError::bad_request(message)).into_response(),
        Ok(Err(error)) => crate::server_error(&error),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn saves_lists_and_refuses_like_python() {
        let library = TestLibrary::new("settings");
        library.execute("INSERT INTO settings VALUES ('root_path', '/music'), ('llm_provider', 'openai')");
        let app = || router(library.state());
        let json_type = [("content-type", "application/json")];

        let answer = send(app(), Method::GET, "/api/settings", &[], "").await;
        assert_eq!(answer.json(), json!({"root_path": "/music"}));

        let answer = send(app(), Method::POST, "/api/settings", &json_type, r#"{"key":"ui.theme","value":"dark","x":1}"#).await;
        assert_eq!((answer.status.as_u16(), answer.json()), (200, json!({"key": "ui.theme", "value": "dark"})));
        let answer = send(app(), Method::POST, "/api/settings", &json_type, r#"{"key":"root_path","value":""}"#).await;
        assert_eq!(answer.json(), json!({"key": "root_path", "value": ""}));
        let answer = send(app(), Method::GET, "/api/settings", &[], "").await;
        assert_eq!(answer.json(), json!({"root_path": "", "ui.theme": "dark"}));

        let answer = send(app(), Method::POST, "/api/settings", &json_type, r#"{"key":" OpenAI_Model ","value":"x"}"#).await;
        assert_eq!(answer.status.as_u16(), 400);
        assert_eq!(
            answer.body,
            r#"{"detail":"plumdeck no longer stores LLM credentials or model settings; AI reasoning is provided by the connected MCP client."}"#
        );
        let answer = send(app(), Method::POST, "/api/settings", &json_type, r#"{"key":"a"}"#).await;
        assert_eq!(answer.status.as_u16(), 422);
        assert_eq!(answer.json()["detail"][0]["loc"], json!(["body", "value"]));
    }
}
