//! Optional in-app AI (plan §2.4): the user's own Claude, OpenAI or Gemini
//! API key, used only for genre cases the on-device classifier is unsure
//! about. Nothing else depends on it; without a key everything works.
//!
//! Keys live in the OS keychain (service [`KEYCHAIN_SERVICE`], one account
//! per provider) and are never logged, returned or stored in the library.
//! The provider choice and model names are ordinary settings under names
//! distinct from the retired LLM keys deleted at startup.
//!
//! What a provider receives per track: title, artist, BPM, the four timbre
//! features and the classifier's candidates, plus the user's verified
//! genre vocabulary. Never the file-tag or rekordbox genre.
//!
//! Routes:
//! - `GET  /api/ai/settings`, `PUT /api/ai/settings` `{provider?, models?}`
//! - `PUT  /api/ai/keys/{provider}` `{api_key}`, `DELETE /api/ai/keys/{provider}`
//! - `POST /api/ai/genre-suggestions` `{track_ids}`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use plumdeck_db::repos::genres::{genre_feature_rows, FeatureScope, GenreFeatureRow};
use plumdeck_db::repos::settings::{save_setting, setting_value};
use plumdeck_domain::genre_rules::{label_map, normalize_genre_label, normalize_subgenre_label};
use serde_json::{json, Value};

use super::genre_classifier::features;
use crate::AppState;

/// Keychain service name of the provider keys.
pub const KEYCHAIN_SERVICE: &str = "plumdeck-ai-provider-key";
const PROVIDER_SETTING: &str = "ai_genre_provider";
const MODEL_SETTING_PREFIX: &str = "ai_genre_model.";
/// Tracks per provider request.
const BATCH: usize = 20;
/// Tracks per `genre-suggestions` call.
const MAX_TRACKS: usize = 100;

/// `(id, label, default model)`.
pub const PROVIDERS: [(&str, &str, &str); 3] = [
    ("claude", "Claude (Anthropic)", "claude-opus-5-5"),
    ("openai", "OpenAI", "gpt-5-mini"),
    ("gemini", "Gemini (Google)", "gemini-2.5-flash"),
];

fn provider(id: &str) -> Option<(&'static str, &'static str, &'static str)> {
    PROVIDERS.iter().copied().find(|(p, _, _)| *p == id)
}

/// Where API keys are kept.
pub trait SecretStore: Send + Sync {
    fn get(&self, provider: &str) -> Option<String>;
    fn set(&self, provider: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, provider: &str) -> Result<(), String>;
}

/// The OS keychain (macOS Keychain, Windows Credential Manager).
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, provider: &str) -> Option<String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, provider).ok()?.get_password().ok().filter(|k| !k.is_empty())
    }

    fn set(&self, provider: &str, secret: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, provider).map_err(|_| "the keychain is unavailable".to_owned())?;
        entry.set_password(secret).map_err(|_| "could not save the key to the keychain".to_owned())
    }

    fn delete(&self, provider: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, provider).map_err(|_| "the keychain is unavailable".to_owned())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("could not delete the key from the keychain".to_owned()),
        }
    }
}

/// In-memory keys (tests).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, provider: &str) -> Option<String> {
        self.0.lock().ok()?.get(provider).cloned()
    }

    fn set(&self, provider: &str, secret: &str) -> Result<(), String> {
        self.0.lock().map_err(|_| "poisoned".to_owned())?.insert(provider.into(), secret.into());
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<(), String> {
        self.0.lock().map_err(|_| "poisoned".to_owned())?.remove(provider);
        Ok(())
    }
}

/// The keychain, except in unit tests.
pub fn default_secret_store() -> Arc<dyn SecretStore> {
    if cfg!(test) {
        Arc::new(MemoryStore::default())
    } else {
        Arc::new(KeychainStore)
    }
}

fn unprocessable(detail: &str) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": detail}))).into_response()
}

struct AiSettings {
    provider: Option<String>,
    models: HashMap<String, String>,
}

fn read_settings(conn: &plumdeck_db::duckdb::Connection) -> Result<AiSettings, plumdeck_db::DbError> {
    let provider = setting_value(conn, PROVIDER_SETTING)?.filter(|p| provider(p).is_some());
    let mut models = HashMap::new();
    for (id, _, _) in PROVIDERS {
        if let Some(model) = setting_value(conn, &format!("{MODEL_SETTING_PREFIX}{id}"))?.filter(|m| !m.trim().is_empty()) {
            models.insert(id.to_owned(), model.trim().to_owned());
        }
    }
    Ok(AiSettings { provider, models })
}

fn settings_json(state: &AppState, settings: &AiSettings) -> Value {
    let providers: Vec<Value> = PROVIDERS
        .iter()
        .map(|(id, label, default)| {
            json!({
                "id": id,
                "label": label,
                "has_key": state.secrets.get(id).is_some(),
                "model": settings.models.get(*id).cloned().unwrap_or_else(|| (*default).to_owned()),
                "default_model": default,
            })
        })
        .collect();
    let ready = settings.provider.as_deref().is_some_and(|p| state.secrets.get(p).is_some());
    json!({"provider": settings.provider, "ready": ready, "providers": providers})
}

/// `GET /api/ai/settings`.
pub async fn get_settings(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    match state.blocking(move || db.lease().and_then(|conn| read_settings(&conn))).await {
        Ok(settings) => Json(settings_json(&state, &settings)).into_response(),
        Err(response) => response,
    }
}

/// `PUT /api/ai/settings`: `{provider: "claude"|"openai"|"gemini"|null,
/// models: {provider: model}}` (both optional).
pub async fn put_settings(State(state): State<AppState>, body: Bytes) -> Response {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else { return unprocessable("invalid JSON body") };
    let mut writes: Vec<(String, String)> = Vec::new();
    if let Some(chosen) = value.get("provider") {
        match chosen {
            Value::Null => writes.push((PROVIDER_SETTING.into(), String::new())),
            Value::String(id) if id.is_empty() || provider(id).is_some() => writes.push((PROVIDER_SETTING.into(), id.clone())),
            _ => return unprocessable("unknown provider"),
        }
    }
    if let Some(models) = value.get("models").and_then(Value::as_object) {
        for (id, model) in models {
            if provider(id).is_none() {
                return unprocessable("unknown provider");
            }
            let model = model.as_str().unwrap_or("").trim();
            if model.len() > 200 {
                return unprocessable("model name too long");
            }
            writes.push((format!("{MODEL_SETTING_PREFIX}{id}"), model.to_owned()));
        }
    }
    let db = state.db.clone();
    let result = state
        .blocking(move || {
            db.write(|conn| {
                for (key, value) in &writes {
                    save_setting(conn, key, value)?;
                }
                Ok(())
            })?;
            db.lease().and_then(|conn| read_settings(&conn))
        })
        .await;
    match result {
        Ok(settings) => Json(settings_json(&state, &settings)).into_response(),
        Err(response) => response,
    }
}

/// `PUT /api/ai/keys/{provider}` `{api_key}`.
pub async fn put_key(State(state): State<AppState>, Path(id): Path<String>, body: Bytes) -> Response {
    if provider(&id).is_none() {
        return unprocessable("unknown provider");
    }
    let Ok(value) = serde_json::from_slice::<Value>(&body) else { return unprocessable("invalid JSON body") };
    let Some(key) = value["api_key"].as_str().map(str::trim).filter(|k| !k.is_empty() && k.len() < 4096) else {
        return unprocessable("api_key is required");
    };
    let secrets = state.secrets.clone();
    let key = key.to_owned();
    match state.blocking_result(move || secrets.set(&id, &key)).await {
        Ok(Ok(())) => Json(json!({"saved": true})).into_response(),
        Ok(Err(message)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"detail": message}))).into_response(),
        Err(response) => response,
    }
}

/// `DELETE /api/ai/keys/{provider}`.
pub async fn delete_key(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if provider(&id).is_none() {
        return unprocessable("unknown provider");
    }
    let secrets = state.secrets.clone();
    match state.blocking_result(move || secrets.delete(&id)).await {
        Ok(Ok(())) => Json(json!({"deleted": true})).into_response(),
        Ok(Err(message)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"detail": message}))).into_response(),
        Err(response) => response,
    }
}

const SYSTEM_PROMPT: &str = "You label tracks in a DJ's music library with a genre and a subgenre.\n\
The DJ's own taxonomy is given as verified genre/subgenre pairs; reuse those exact labels whenever one fits, \
and only invent a new label when none fits at all.\n\
For each track you get the title, artist, BPM, timbre features (brightness 0-1, loudness in dB, spectral contrast 0-1, \
noisiness 0-1) and the candidates of an on-device audio classifier trained on the DJ's labels (with probabilities). \
The classifier was unsure about these tracks. Use what you know about the artist and the recording, and the audio \
features, to choose. If you do not recognize the track, prefer the classifier's candidates.\n\
Answer with JSON only, no prose: {\"results\":[{\"track_id\":<int>,\"genre\":<string>,\"subgenre\":<string or null>,\
\"confidence\":\"high\"|\"medium\"|\"low\",\"reason\":<short string>}]} with one entry per track.";

fn round(value: Option<f32>, digits: i32) -> Value {
    value.filter(|v| v.is_finite()).map_or(Value::Null, |v| {
        let factor = 10f64.powi(digits);
        json!((f64::from(v) * factor).round() / factor)
    })
}

/// The user message for one batch: taxonomy and tracks (no tag genres).
fn user_prompt(taxonomy: &[(String, String)], tracks: &[Value]) -> String {
    let pairs: Vec<String> = taxonomy.iter().map(|(g, s)| if s.is_empty() { g.clone() } else { format!("{g} / {s}") }).collect();
    let body = json!({"taxonomy": pairs, "tracks": tracks});
    format!("Label these tracks.\n{}", serde_json::to_string_pretty(&body).unwrap_or_default())
}

fn track_payload(row: &GenreFeatureRow, model: Option<&plumdeck_genre::Model>) -> Value {
    let prediction = model.and_then(|m| features(row).and_then(|f| m.predict(&f)));
    let candidates = prediction.as_ref().map_or(json!([]), |p| {
        json!(p
            .candidates
            .iter()
            .map(|c| json!({"genre": c.genre, "probability": (c.probability * 100.0).round() / 100.0}))
            .collect::<Vec<_>>())
    });
    let pairs = prediction.as_ref().map_or(json!([]), |p| {
        json!(p
            .pair_candidates
            .iter()
            .map(|c| json!({"genre": c.genre, "subgenre": c.subgenre, "probability": (c.probability * 100.0).round() / 100.0}))
            .collect::<Vec<_>>())
    });
    json!({
        "track_id": row.id,
        "title": row.title,
        "artist": row.artist,
        "bpm": round(row.bpm, 1),
        "brightness": round(row.brightness, 2),
        "loudness_db": round(row.loudness, 1),
        "contrast": round(row.contrast, 2),
        "noisiness": round(row.noisiness, 2),
        "classifier_genres": candidates,
        "classifier_subgenres": pairs,
    })
}

/// A provider failure, safe to show (never contains the key).
#[derive(Debug)]
struct ProviderError(String);

fn short(text: &str) -> String {
    let text: String = text.chars().take(300).collect();
    text.replace('\n', " ")
}

async fn call_provider(client: &reqwest::Client, id: &str, model: &str, key: &str, system: &str, user: &str) -> Result<String, ProviderError> {
    let request = match id {
        "claude" => {
            let mut body = json!({
                "model": model,
                "max_tokens": 8000,
                "system": system,
                "messages": [{"role": "user", "content": user}],
                "output_config": {"effort": "low"},
            });
            let fallback = ["claude-opus-5", "claude-fable-5-1", "claude-sonnet-5-5"].iter().any(|p| model.starts_with(p));
            let mut request = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", key)
                .header("anthropic-version", "2026-10-21");
            if fallback {
                body["fallbacks"] = json!("default");
                request = request.header("anthropic-beta", "server-side-fallback-2026-07-01");
            }
            request.json(&body)
        }
        "openai" => client.post("https://api.openai.com/v1/chat/completions").bearer_auth(key).json(&json!({
            "model": model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "response_format": {"type": "json_object"},
        })),
        "gemini" => client
            .post(format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"))
            .header("x-goog-api-key", key)
            .json(&json!({
                "systemInstruction": {"parts": [{"text": system}]},
                "contents": [{"role": "user", "parts": [{"text": user}]}],
                "generationConfig": {"responseMimeType": "application/json"},
            })),
        _ => return Err(ProviderError("unknown provider".into())),
    };
    // reqwest errors can name the URL; no key is ever in a URL here.
    let response = request.send().await.map_err(|e| ProviderError(format!("could not reach the provider: {}", e.without_url())))?;
    let status = response.status();
    let text = response.text().await.map_err(|_| ProviderError("could not read the provider's answer".into()))?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(str::to_owned))
            .unwrap_or_else(|| short(&text));
        return Err(ProviderError(format!("provider answered {}: {}", status.as_u16(), short(&detail))));
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| ProviderError("the provider's answer is not JSON".into()))?;
    let content = match id {
        "claude" => {
            if value["stop_reason"] == "refusal" {
                return Err(ProviderError("the model declined the request".into()));
            }
            value["content"].as_array().map(|blocks| {
                blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("")
            })
        }
        "openai" => value["choices"][0]["message"]["content"].as_str().map(str::to_owned),
        _ => value["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")),
    };
    content.filter(|c| !c.trim().is_empty()).ok_or_else(|| ProviderError("the provider returned no text".into()))
}

/// The JSON object inside a model's text answer.
fn parse_results(text: &str) -> Option<Vec<Value>> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let value: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    value["results"].as_array().cloned()
}

fn normalize_confidence(value: &Value) -> &'static str {
    match value.as_str().map(str::to_lowercase).as_deref() {
        Some("high") => "high",
        Some("low") => "low",
        _ => "medium",
    }
}

/// `POST /api/ai/genre-suggestions` `{track_ids}`: ask the configured
/// provider about tracks (meant for low-confidence ones). Suggestions are
/// returned, not applied; accepting goes through the classifier's accept.
pub async fn genre_suggestions(State(state): State<AppState>, body: Bytes) -> Response {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else { return unprocessable("invalid JSON body") };
    let Some(ids) = value["track_ids"].as_array().map(|ids| ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>()) else {
        return unprocessable("track_ids must be a list of integers");
    };
    if ids.is_empty() {
        return Json(json!({"items": []})).into_response();
    }
    if ids.len() > MAX_TRACKS {
        return unprocessable("too many tracks in one request");
    }
    let db = state.db.clone();
    let loaded = state
        .blocking(move || {
            let conn = db.lease()?;
            let settings = read_settings(&conn)?;
            let rows = genre_feature_rows(&conn, FeatureScope::Ids(&ids))?;
            let mut taxonomy: Vec<(String, String)> = conn
                .prepare(
                    "SELECT DISTINCT genre, coalesce(subgenre, '') FROM tracks WHERE is_genre_verified = true AND genre IS NOT NULL \
                     AND genre != '' AND lower(genre) != 'unknown'",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            taxonomy.sort();
            Ok((settings, rows, taxonomy))
        })
        .await;
    let (settings, rows, taxonomy) = match loaded {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let Some(id) = settings.provider.clone() else {
        return (StatusCode::CONFLICT, Json(json!({"detail": "AI is not set up"}))).into_response();
    };
    let secrets = state.secrets.clone();
    let lookup = id.clone();
    let key = match state.blocking_result(move || secrets.get(&lookup)).await {
        Ok(Some(key)) => key,
        Ok(None) => return (StatusCode::CONFLICT, Json(json!({"detail": "no API key is saved for this provider"}))).into_response(),
        Err(response) => return response,
    };
    let model_name = settings.models.get(&id).cloned().unwrap_or_else(|| provider(&id).map(|p| p.2).unwrap_or_default().to_owned());
    let classifier = state.genre_classifier.model();
    let genre_labels = label_map(&taxonomy.iter().map(|(g, _)| g.clone()).collect::<Vec<_>>());
    let sub_labels = label_map(&taxonomy.iter().map(|(_, s)| s.clone()).filter(|s| !s.is_empty()).collect::<Vec<_>>());
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(180)).build() {
        Ok(client) => client,
        Err(error) => return crate::server_error(&error),
    };
    let mut items: Vec<Value> = Vec::new();
    for batch in rows.chunks(BATCH) {
        let tracks: Vec<Value> = batch.iter().map(|row| track_payload(row, classifier.as_deref())).collect();
        let answer = call_provider(&client, &id, &model_name, &key, SYSTEM_PROMPT, &user_prompt(&taxonomy, &tracks)).await;
        let results = match answer {
            Ok(text) => parse_results(&text).ok_or_else(|| ProviderError("the provider's answer could not be read".into())),
            Err(error) => Err(error),
        };
        for row in batch {
            match &results {
                Ok(results) => match results.iter().find(|r| r["track_id"].as_i64() == Some(row.id)) {
                    Some(result) => {
                        let genre = result["genre"].as_str().map(|g| normalize_genre_label(g, &genre_labels));
                        let subgenre = result["subgenre"].as_str().filter(|s| !s.trim().is_empty()).map(|s| normalize_subgenre_label(s, &sub_labels));
                        items.push(json!({
                            "track_id": row.id,
                            "genre": genre,
                            "subgenre": subgenre,
                            "confidence": normalize_confidence(&result["confidence"]),
                            "reason": result["reason"].as_str().map(|r| short(r)),
                            "error": null,
                        }));
                    }
                    None => items.push(json!({"track_id": row.id, "error": "the provider skipped this track"})),
                },
                Err(ProviderError(message)) => items.push(json!({"track_id": row.id, "error": message})),
            }
        }
    }
    Json(json!({"provider": id, "model": model_name, "items": items})).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use super::*;
    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn settings_and_keys() {
        let library = TestLibrary::new("ai-settings");
        let state = library.state();
        let app = || router(state.clone());
        let json_type = [("content-type", "application/json")];
        let settings = send(app(), Method::GET, "/api/ai/settings", &[], "").await.json();
        assert_eq!(settings["provider"], Value::Null);
        assert_eq!(settings["ready"], false);
        assert_eq!(settings["providers"][0]["default_model"], "claude-opus-5-5");
        let saved = send(app(), Method::PUT, "/api/ai/keys/claude", &json_type, r#"{"api_key":" sk-test "}"#).await.json();
        assert_eq!(saved, json!({"saved": true}));
        assert_eq!(state.secrets.get("claude").as_deref(), Some("sk-test"));
        let body = r#"{"provider":"claude","models":{"openai":"gpt-x"}}"#;
        let settings = send(app(), Method::PUT, "/api/ai/settings", &json_type, body).await.json();
        assert_eq!(settings["provider"], "claude");
        assert_eq!(settings["ready"], true);
        assert_eq!(settings["providers"][0]["has_key"], true);
        assert_eq!(settings["providers"][1]["model"], "gpt-x");
        assert!(!settings.to_string().contains("sk-test"), "keys are never returned");
        let all = send(app(), Method::GET, "/api/settings", &[], "").await.body;
        assert!(!all.contains("sk-test"));
        assert_eq!(send(app(), Method::PUT, "/api/ai/settings", &json_type, r#"{"provider":"x"}"#).await.status.as_u16(), 422);
        assert_eq!(send(app(), Method::PUT, "/api/ai/keys/x", &json_type, r#"{"api_key":"k"}"#).await.status.as_u16(), 422);
        send(app(), Method::DELETE, "/api/ai/keys/claude", &[], "").await;
        assert_eq!(state.secrets.get("claude"), None);
        let none = send(app(), Method::POST, "/api/ai/genre-suggestions", &json_type, r#"{"track_ids":[]}"#).await.json();
        assert_eq!(none, json!({"items": []}));
        let unset = send(app(), Method::POST, "/api/ai/genre-suggestions", &json_type, r#"{"track_ids":[1]}"#).await;
        assert_eq!(unset.status.as_u16(), 409);
    }

    #[test]
    fn prompt_never_carries_tag_genres() {
        let row = GenreFeatureRow {
            id: 7,
            filepath: "/x".into(),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            bpm: Some(124.0),
            brightness: Some(0.21),
            loudness: Some(-8.0),
            contrast: Some(0.3),
            noisiness: Some(0.4),
            genre: Some("TagGenreFromFile".into()),
            subgenre: Some("TagSub".into()),
            verified: false,
            embedding_json: None,
        };
        let prompt = user_prompt(&[("House".into(), "Deep House".into())], &[track_payload(&row, None)]);
        assert!(prompt.contains("Song") && prompt.contains("House / Deep House"));
        assert!(!prompt.contains("TagGenreFromFile") && !prompt.contains("TagSub"));
        let parsed = parse_results("Sure: {\"results\":[{\"track_id\":7,\"genre\":\"House\"}]} ok").unwrap();
        assert_eq!(parsed[0]["track_id"], 7);
        assert_eq!(normalize_confidence(&json!("HIGH")), "high");
    }
}
