//! The MCP Junction tools (backend/mcp_server/tools/junction.py and
//! junction_bridge.py; S12). Every tool maps to one semantic desktop action
//! through a [`JunctionPort`]: in the desktop app the port dispatches
//! in-process; the headless server falls back to the authenticated loopback
//! bridge the desktop process announces (`PLUMDECK_JUNCTION_BRIDGE_URL` /
//! `PLUMDECK_JUNCTION_BRIDGE_TOKEN`), as the Python server did.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Map, Value};

const MAX_EXCHANGE_TEXT_BYTES: usize = 128 * 1024;
const MAX_BRIDGE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const UNAVAILABLE: &str = "Junction bridge is unavailable; open plumdeck desktop and keep it running";
const RETIRED_LIVE_MONITOR: &str = "Junction Liveは廃止されました。音源は転送せず、前のDJの曲名・BPM・再生位置はjunction_get_state の junctionInput.decks（JUNCTION MASTER）で確認できます";
const RETIRED_HANDOFF: &str = "この操作は廃止されました。次のDJがフェーダーを上げると交代します。順番は junction_join_turn / junction_leave_turn / junction_reorder_roster、ホストの介入は junction_force_turn / junction_skip_turn / junction_release_tail を使ってください";

pub type PortFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// Where Junction actions go: the engine in the desktop process.
pub trait JunctionPort: Send + Sync {
    fn dispatch<'a>(&'a self, action: &'a str, arguments: Value) -> PortFuture<'a>;
}

/// The loopback bridge of a running desktop app.
pub struct BridgePort;

fn endpoint() -> Result<(String, String), String> {
    let url = std::env::var("PLUMDECK_JUNCTION_BRIDGE_URL").unwrap_or_default();
    let url = url.trim();
    let token = std::env::var("PLUMDECK_JUNCTION_BRIDGE_TOKEN").unwrap_or_default();
    if url.is_empty() || token.is_empty() {
        return Err(UNAVAILABLE.into());
    }
    if token.len() > 8192 || token.contains('\r') || token.contains('\n') {
        return Err("Junction bridge credentials are invalid".into());
    }
    let rest = url.strip_prefix("http://").ok_or("Junction bridge must use a loopback HTTP address")?;
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if !path.is_empty() || authority.contains('@') || authority.contains('?') || authority.contains('#') {
        return Err("Junction bridge must use a loopback HTTP address".into());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => (host.trim_start_matches('[').trim_end_matches(']'), port),
        _ => return Err("Junction bridge address must include a port".into()),
    };
    port.parse::<u16>().map_err(|_| "Junction bridge address is invalid".to_owned())?;
    let loopback = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false);
    if !loopback {
        return Err("Junction bridge must use a loopback HTTP address".into());
    }
    Ok((format!("http://{authority}/junction"), token))
}

impl JunctionPort for BridgePort {
    fn dispatch<'a>(&'a self, action: &'a str, arguments: Value) -> PortFuture<'a> {
        Box::pin(async move {
            let (url, token) = endpoint()?;
            let client = reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(15)).build().map_err(|_| UNAVAILABLE.to_owned())?;
            let response = client
                .post(url)
                .bearer_auth(token)
                .header("Content-Type", "application/json; charset=utf-8")
                .header("Accept", "application/json")
                .body(serde_json::to_vec(&json!({"action": action, "arguments": arguments})).unwrap_or_default())
                .send()
                .await
                .map_err(|_| UNAVAILABLE.to_owned())?;
            if !response.status().is_success() {
                return Err(format!("Junction bridge rejected the request (HTTP {})", response.status().as_u16()));
            }
            let bytes = response.bytes().await.map_err(|_| UNAVAILABLE.to_owned())?;
            if bytes.len() > MAX_BRIDGE_RESPONSE_BYTES {
                return Err("Junction bridge returned an oversized response".into());
            }
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(value @ Value::Object(_)) => Ok(value),
                _ => Err("Junction bridge returned an invalid response".into()),
            }
        })
    }
}

/// The port the app installed, else the loopback bridge.
pub fn default_port() -> Arc<dyn JunctionPort> {
    Arc::new(BridgePort)
}

type Checked<T> = Result<T, String>;

fn text(a: &Value, key: &str, max: usize, empty: bool) -> Checked<String> {
    let Some(value) = a.get(key).and_then(Value::as_str) else { return Err(format!("{key} must be a string")) };
    let cleaned = value.trim().to_owned();
    if !empty && cleaned.is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    if cleaned.chars().count() > max {
        return Err(format!("{key} must be at most {max} characters"));
    }
    Ok(cleaned)
}

fn opt_text(a: &Value, key: &str, max: usize, empty: bool) -> Checked<Option<String>> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text(a, key, max, empty).map(Some),
    }
}

fn network_text(value: &Value, field: &str, max: usize, min: usize) -> Checked<String> {
    let Some(value) = value.as_str() else { return Err(format!("{field} must be a string")) };
    let size = value.len();
    if size < min || size > max {
        return Err(format!("{field} must be {min}..{max} UTF-8 bytes"));
    }
    if value.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(value.to_owned())
}

fn network_urls(value: Option<&Value>, field: &str, required: bool) -> Checked<Vec<String>> {
    let items = value.and_then(Value::as_array).cloned().unwrap_or_default();
    if value.is_some_and(|v| !v.is_array() && !v.is_null()) || items.len() > 8 || (required && items.is_empty()) {
        return Err(format!("{field} must contain {} URLs", if required { "1..8" } else { "at most 8" }));
    }
    items.iter().map(|v| network_text(v, &format!("{field} item"), 1024, 1)).collect()
}

fn exchange(a: &Value) -> Checked<String> {
    let Some(value) = a.get("exchange_text").and_then(Value::as_str) else { return Err("exchange_text must be a string".into()) };
    let value = value.trim();
    if value.is_empty() {
        return Err("exchange_text must not be empty".into());
    }
    if value.len() > MAX_EXCHANGE_TEXT_BYTES {
        return Err(format!("exchange_text must be at most {MAX_EXCHANGE_TEXT_BYTES} UTF-8 bytes"));
    }
    Ok(value.to_owned())
}

fn profile(a: &Value) -> Checked<Map<String, Value>> {
    let mut out = Map::new();
    out.insert("djName".into(), json!(text(a, "dj_name", 40, false)?));
    if let Some(avatar) = opt_text(a, "avatar_data_url", 4096, true)? {
        out.insert("avatarDataUrl".into(), json!(avatar));
    }
    if let Some(color) = opt_text(a, "theme_color", 32, false)? {
        out.insert("themeColor".into(), json!(color));
    }
    Ok(out)
}

fn boolean(a: &Value, key: &str, default: bool) -> Checked<bool> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key}: Input should be a valid boolean")),
    }
}

fn number(a: &Value, key: &str) -> Checked<Option<f64>> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(_) => Err(format!("{key}: Input should be a valid number")),
    }
}

fn peer(a: &Value, key: &str) -> Checked<Map<String, Value>> {
    let mut out = Map::new();
    if let Some(id) = opt_text(a, key, 128, false)? {
        out.insert("peerId".into(), json!(id));
    }
    Ok(out)
}

/// (action, arguments) for a tool, after the Python wrappers' checks.
fn request(name: &str, a: &Value) -> Checked<(&'static str, Value)> {
    let o = |m: Map<String, Value>| Value::Object(m);
    Ok(match name {
        "junction_get_state" => ("snapshot", json!({})),
        "junction_prepare_engine" => ("prepare", json!({})),
        "junction_inspect_exchange" => ("exchange.inspect", json!({"text": exchange(a)?})),
        "junction_get_exchange_text" => {
            let kind = a.get("kind").and_then(Value::as_str).filter(|k| ["invite", "response", "notice"].contains(k)).ok_or("kind must be invite, response or notice")?;
            let mut args = peer(a, "peer_id")?;
            args.insert("kind".into(), json!(kind));
            ("exchange.export", o(args))
        }
        "junction_list_audio_devices" => ("audio.devices", json!({})),
        "junction_get_network" => ("network.get", json!({})),
        "junction_test_network" => {
            let (poll, cancel) = (boolean(a, "poll", false)?, boolean(a, "cancel", false)?);
            if poll && cancel {
                return Err("poll and cancel cannot both be true".into());
            }
            ("network.test", json!({"poll": poll, "cancel": cancel}))
        }
        "junction_configure_network" => {
            let stun = network_urls(a.get("stun_urls"), "stun_urls", false)?;
            if a.get("stun_urls").is_none() {
                return Err("stun_urls: Field required".into());
            }
            let save = boolean(a, "save", true)?;
            let present = |k: &str| a.get(k).is_some_and(|v| !v.is_null());
            let supplied = ["turn_urls", "turn_secret", "turn_username", "turn_credential", "turn_expires_at"].iter().any(|k| present(k));
            let turn = match a.get("turn_mode").and_then(Value::as_str) {
                None => {
                    if supplied {
                        return Err("turn_mode is required when TURN fields are supplied".into());
                    }
                    json!({})
                }
                Some(mode) if mode == "rest" || mode == "temporary" => {
                    let urls = network_urls(a.get("turn_urls"), "turn_urls", true)?;
                    let mut turn = Map::new();
                    turn.insert("mode".into(), json!(mode));
                    turn.insert("urls".into(), json!(urls));
                    if mode == "rest" {
                        if ["turn_username", "turn_credential", "turn_expires_at"].iter().any(|k| present(k)) {
                            return Err("REST TURN accepts turn_secret only; temporary credential fields must be omitted".into());
                        }
                        let secret = a.get("turn_secret").filter(|v| !v.is_null()).ok_or("turn_secret is required for REST TURN")?;
                        turn.insert("secret".into(), json!(network_text(secret, "turn_secret", 512, 32)?));
                    } else {
                        if present("turn_secret") {
                            return Err("turn_secret must be omitted for temporary TURN".into());
                        }
                        let (Some(username), Some(credential), Some(expires)) =
                            (a.get("turn_username").filter(|v| !v.is_null()), a.get("turn_credential").filter(|v| !v.is_null()), a.get("turn_expires_at").and_then(Value::as_i64))
                        else {
                            return Err("temporary TURN requires turn_username, turn_credential, and turn_expires_at".into());
                        };
                        let username = network_text(username, "turn_username", 256, 1)?;
                        turn.insert("username".into(), json!(username));
                        turn.insert("credential".into(), json!(network_text(credential, "turn_credential", 1024, 1)?));
                        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                        if expires <= now_ms || expires > now_ms + 86_400_000 {
                            return Err("turn_expires_at must be a future Unix epoch millisecond value within 24 hours".into());
                        }
                        let seconds: i64 = username.split(':').next().unwrap_or("").parse().map_err(|_| "turn_username must begin with its Unix expiry in seconds".to_owned())?;
                        if seconds < 1 || seconds > (now_ms + 86_400_000) / 1000 || seconds * 1000 < expires - 1000 {
                            return Err("turn_username expiry must match turn_expires_at".into());
                        }
                        turn.insert("expiresAt".into(), json!(expires));
                    }
                    Value::Object(turn)
                }
                Some(_) => return Err("turn_mode must be rest or temporary".into()),
            };
            let config = serde_json::to_string(&json!({"stunUrls": stun, "turn": turn})).unwrap_or_default();
            if config.len() > 16_384 {
                return Err("network configuration must be at most 16384 UTF-8 bytes".into());
            }
            ("network.configure", json!({"stunUrls": stun, "save": save, "turn": turn}))
        }
        "junction_clear_network" => ("network.clear", json!({})),
        "junction_create_session" => {
            let mut args = profile(a)?;
            args.insert("sessionName".into(), json!(text(a, "session_name", 80, false)?));
            args.insert("programDevice".into(), json!(text(a, "program_device", 1024, false)?));
            args.insert("adoptCurrent".into(), json!(boolean(a, "adopt_current", false)?));
            args.insert("startInLobby".into(), json!(boolean(a, "start_in_lobby", true)?));
            args.insert("exchangeMode".into(), json!("manual"));
            ("create", o(args))
        }
        "junction_join_session" => {
            let mut args = profile(a)?;
            args.insert("text".into(), json!(exchange(a)?));
            ("join", o(args))
        }
        "junction_create_invite" => {
            let mut args = peer(a, "peer_id")?;
            if let Some(name) = opt_text(a, "dj_name", 40, false)? {
                args.insert("djName".into(), json!(name));
            }
            if let Some(avatar) = opt_text(a, "avatar_data_url", 4096, true)? {
                args.insert("avatarDataUrl".into(), json!(avatar));
            }
            if let Some(color) = opt_text(a, "theme_color", 32, false)? {
                args.insert("themeColor".into(), json!(color));
            }
            ("invite.create", o(args))
        }
        "junction_import_exchange" => {
            let mut args = peer(a, "peer_id")?;
            args.insert("text".into(), json!(exchange(a)?));
            ("exchange.import", o(args))
        }
        "junction_approve_participant" => ("peer.approve", json!({"peerId": text(a, "peer_id", 128, false)?, "accept": true})),
        "junction_reject_participant" => ("peer.reject", json!({"peerId": text(a, "peer_id", 128, false)?})),
        "junction_cancel_invite" => ("invite.cancel", json!({"peerId": text(a, "peer_id", 128, false)?})),
        "junction_retry_participant" => ("peer.retry", json!({"peerId": text(a, "peer_id", 128, false)?})),
        "junction_update_profile" => ("profile.update", o(profile(a)?)),
        "junction_reorder_roster" => {
            let ids = a.get("peer_ids").and_then(Value::as_array).cloned().unwrap_or_default();
            if !(1..=64).contains(&ids.len()) {
                return Err("peer_ids must contain 1..64 participants".into());
            }
            let mut cleaned: Vec<String> = Vec::new();
            for id in &ids {
                let value = text(&json!({"peer_id": id}), "peer_id", 128, false)?;
                if cleaned.contains(&value) {
                    return Err("peer_ids must not contain duplicates".into());
                }
                cleaned.push(value);
            }
            ("roster.reorder", json!({"peerIds": cleaned}))
        }
        "junction_start_session" => ("session.start", json!({"performerPeerId": text(a, "performer_peer_id", 128, false)?})),
        "junction_request_handoff" | "junction_cancel_handoff" | "junction_accept_handoff" => return Err(RETIRED_HANDOFF.into()),
        "junction_join_turn" => ("turn.join", o(peer(a, "peer_id")?)),
        "junction_leave_turn" => ("turn.leave", o(peer(a, "peer_id")?)),
        "junction_set_b2b_repeat" => ("turn.repeat", json!({"enabled": a.get("enabled").and_then(Value::as_bool).ok_or("enabled: Field required")?})),
        "junction_set_failover" => ("turn.failover", json!({"auto": a.get("automatic").and_then(Value::as_bool).ok_or("automatic: Field required")?})),
        "junction_go_on_air" => ("turn.onair", json!({})),
        "junction_force_turn" => ("turn.force", json!({})),
        "junction_skip_turn" => ("turn.skip", json!({})),
        "junction_release_tail" => ("turn.release", json!({})),
        "junction_send_cue" => {
            let kind = a.get("kind").and_then(Value::as_str).filter(|k| ["one_more", "go_ahead", "hold", "ok"].contains(k)).ok_or("kind must be one_more, go_ahead, hold or ok")?;
            ("turn.cue", json!({"kind": kind}))
        }
        "junction_resume_recovery" => ("recovery.resume", json!({})),
        "junction_leave_session" => ("leave", json!({})),
        "junction_end_session" => ("end", json!({})),
        "junction_configure_program" => {
            let mut args = Map::new();
            args.insert("programDevice".into(), json!(text(a, "program_device", 1024, false)?));
            if let Some(gain) = number(a, "gain")? {
                if !gain.is_finite() || !(0.0..=2.0).contains(&gain) {
                    return Err("gain must be finite and within 0..2".into());
                }
                args.insert("gain".into(), json!(gain));
            }
            ("program.configure", o(args))
        }
        "junction_start_program_recording" => ("program.record.start", json!({"path": text(a, "path", 4096, false)?})),
        "junction_stop_program_recording" => ("program.record.stop", json!({})),
        "junction_load_private_preview" => ("private.load", json!({"path": text(a, "path", 4096, false)?})),
        "junction_play_private_preview" => ("private.play", json!({})),
        "junction_pause_private_preview" => ("private.pause", json!({})),
        "junction_seek_private_preview" => {
            let position = number(a, "position_ms")?.ok_or("position_ms: Field required")?;
            if !position.is_finite() || position < 0.0 {
                return Err("position_ms must be finite and non-negative".into());
            }
            ("private.seek", json!({"positionMs": position}))
        }
        "junction_set_private_preview_gain" => {
            let gain = number(a, "gain")?.ok_or("gain: Field required")?;
            if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
                return Err("gain must be finite and within 0..1".into());
            }
            ("private.gain", json!({"gain": gain}))
        }
        "junction_unload_private_preview" => ("private.unload", json!({})),
        "junction_get_private_preview_state" => ("private.state", json!({})),
        "junction_set_microphone_enabled" => ("mic.enabled", json!({"enabled": a.get("enabled").and_then(Value::as_bool).ok_or("enabled: Field required")?})),
        "junction_attach_live_monitor" | "junction_detach_live_monitor" => return Err(RETIRED_LIVE_MONITOR.into()),
        "junction_configure_input" => {
            let mut args = Map::new();
            for (source, target, max) in [("volume", "volume", 1.0), ("eq_low", "eqLow", 4.0), ("eq_mid", "eqMid", 4.0), ("eq_high", "eqHigh", 4.0)] {
                if let Some(value) = number(a, source)? {
                    if !value.is_finite() || !(0.0..=max).contains(&value) {
                        return Err(format!("{target} must be a finite number"));
                    }
                    args.insert(target.into(), json!(value));
                }
            }
            if let Some(assign) = a.get("assign").and_then(Value::as_str) {
                let orientation = match assign {
                    "left" => 0,
                    "thru" => 1,
                    "right" => 2,
                    _ => return Err("assign must be left, thru or right".into()),
                };
                args.insert("orientation".into(), json!(orientation));
            }
            if let Some(cue) = a.get("cue").and_then(Value::as_bool) {
                args.insert("pfl".into(), json!(cue));
            }
            if args.is_empty() {
                return Err("at least one JUNCTION input setting is required".into());
            }
            ("input.set", o(args))
        }
        "junction_release_input" => ("input.release", json!({})),
        _ => return Err(format!("Unknown tool: {name}")),
    })
}

/// Run one Junction tool; `Err` is the ToolError text.
pub async fn call(port: &dyn JunctionPort, name: &str, arguments: &Value) -> Result<Value, String> {
    let (action, args) = request(name, arguments)?;
    port.dispatch(action, args).await
}
