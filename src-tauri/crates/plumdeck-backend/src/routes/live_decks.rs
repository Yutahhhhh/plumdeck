//! The Play screen's own decks, mirrored for local readers (the cluster show director).
//!
//! The engine lives behind the Tauri bridge, so the UI reports each deck here; `GET` answers with an
//! `AssistSnapshot`-shaped document so a reader that understands rekordbox decks needs no new model.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::AppState;

/// A report older than this is treated as "Play is not open".
const STALE_SECONDS: f64 = 5.0;
const MAX_BODY: usize = 64 * 1024;

#[derive(Clone, Default)]
pub struct LiveDecks(Arc<Mutex<Option<(Value, f64)>>>);

fn str_field(v: &Value, key: &str, max: usize) -> Value {
    match v.get(key).and_then(Value::as_str) {
        Some(s) => Value::String(s.chars().take(max).collect()),
        None => Value::Null,
    }
}

fn num_field(v: &Value, key: &str) -> Value {
    match v.get(key).and_then(Value::as_f64) {
        Some(n) if n.is_finite() => json!(n),
        _ => Value::Null,
    }
}

fn bad(message: &str) -> Response {
    (axum::http::StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": message}))).into_response()
}

/// `PUT /api/play/live-decks` — body `{"decks": [{slot, loaded, status, title, artist, track_bpm, tempo_bpm, display_key, elapsed_seconds}]}`.
pub async fn report(State(state): State<AppState>, body: Bytes) -> Response {
    if body.len() > MAX_BODY {
        return bad("body too large");
    }
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else { return bad("invalid json") };
    let Some(list) = parsed.get("decks").and_then(Value::as_array) else { return bad("decks required") };
    if list.len() > 4 {
        return bad("at most 4 decks");
    }
    let mut decks = Vec::new();
    for d in list {
        let Some(slot) = d.get("slot").and_then(Value::as_i64).filter(|s| (1..=4).contains(s)) else {
            return bad("slot must be 1-4");
        };
        decks.push(json!({
            "slot": slot,
            "loaded": d.get("loaded").and_then(Value::as_bool).unwrap_or(false),
            "playing": d.get("playing").and_then(Value::as_bool).unwrap_or(false),
            "title": str_field(d, "title", 512),
            "artist": str_field(d, "artist", 512),
            "track_bpm": num_field(d, "track_bpm"),
            "tempo_bpm": num_field(d, "tempo_bpm"),
            "display_key": str_field(d, "display_key", 32),
            "elapsed_seconds": num_field(d, "elapsed_seconds"),
        }));
    }
    let now = state.clock.epoch_seconds();
    *state.live_decks.0.lock().expect("live decks") = Some((Value::Array(decks), now));
    Json(json!({"ok": true})).into_response()
}

/// `GET /api/play/live-decks`.
pub async fn read(State(state): State<AppState>) -> Response {
    let now = state.clock.epoch_seconds();
    let held = state.live_decks.0.lock().expect("live decks").clone();
    let (decks, age) = match held {
        Some((decks, at)) => (decks, Some(now - at)),
        None => (Value::Array(Vec::new()), None),
    };
    let open = age.is_some_and(|a| a <= STALE_SECONDS);
    Json(json!({
        "open": open,
        "reported_seconds_ago": age,
        "decks": if open { decks } else { Value::Array(Vec::new()) },
    }))
    .into_response()
}
