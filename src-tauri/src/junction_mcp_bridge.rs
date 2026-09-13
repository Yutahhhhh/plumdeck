//! Private loopback bridge between the Python MCP sidecar and the one native
//! audio engine owned by Tauri.
//!
//! The public MCP endpoint never receives an engine command name. This bridge
//! accepts a fixed Junction action allowlist, authenticates every request with
//! a per-launch random bearer token, and binds only to `127.0.0.1`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter, State};

use crate::dj_engine::supervisor::{EngineReply, EngineSupervisor};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 160 * 1024;
const MAX_CONNECTIONS: usize = 8;
const LIVE_MONITOR_EVENT: &str = "junction://live-monitor";

#[derive(Deserialize)]
struct BridgeRequest {
    action: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

struct BridgeShared {
    monitor_deck: Mutex<Option<String>>,
}

/// Managed for the lifetime of the app. Dropping it closes the accept loop;
/// the bearer token is deliberately not exposed through a Tauri command.
pub struct JunctionMcpBridge {
    url: String,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<BridgeShared>,
}

impl JunctionMcpBridge {
    pub fn start(app: AppHandle, supervisor: Arc<EngineSupervisor>) -> Result<Self, String> {
        let development = development_bridge_config()?;
        let port = development.as_ref().map_or(0, |(port, _)| *port);
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|error| format!("Junction MCPブリッジを開始できません: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("Junction MCPブリッジを設定できません: {error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("Junction MCPブリッジの待受先を確認できません: {error}"))?;
        let token = match development {
            Some((_, token)) => token,
            None => random_token()?,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(BridgeShared {
            monitor_deck: Mutex::new(None),
        });
        let active_connections = Arc::new(AtomicUsize::new(0));

        let thread_stop = Arc::clone(&stop);
        let thread_shared = Arc::clone(&shared);
        let thread_token = token.clone();
        let thread = thread::Builder::new()
            .name("junction-mcp-bridge".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, peer)) => {
                            if !peer.ip().is_loopback() {
                                let _ = write_json(
                                    &mut stream,
                                    403,
                                    &bridge_error(
                                        "forbidden",
                                        "ローカルアプリからのみ利用できます",
                                        false,
                                    ),
                                );
                                continue;
                            }
                            if active_connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS
                            {
                                active_connections.fetch_sub(1, Ordering::AcqRel);
                                let _ = write_json(
                                    &mut stream,
                                    429,
                                    &bridge_error("busy", "Junction操作が混み合っています", true),
                                );
                                continue;
                            }
                            let child_supervisor = Arc::clone(&supervisor);
                            let child_shared = Arc::clone(&thread_shared);
                            let child_token = thread_token.clone();
                            let child_app = app.clone();
                            let child_active = Arc::clone(&active_connections);
                            thread::spawn(move || {
                                let _guard = ConnectionGuard(child_active);
                                handle_connection(
                                    &mut stream,
                                    &child_token,
                                    &child_app,
                                    &child_supervisor,
                                    &child_shared,
                                );
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(error) => {
                            eprintln!("[junction-mcp] accept failed: {error}");
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .map_err(|error| format!("Junction MCPブリッジのスレッドを開始できません: {error}"))?;

        Ok(Self {
            url: format!("http://127.0.0.1:{}", address.port()),
            token,
            stop,
            thread: Some(thread),
            shared,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn monitor_deck(&self) -> Option<String> {
        self.shared.monitor_deck.lock().ok()?.clone()
    }

    fn set_monitor_deck(
        &self,
        app: &AppHandle,
        deck: Option<String>,
    ) -> Result<Option<String>, String> {
        set_monitor_deck(app, &self.shared, deck)
    }
}

impl Drop for JunctionMcpBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ConnectionGuard(Arc<AtomicUsize>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[tauri::command]
pub fn junction_live_monitor_deck(state: State<'_, JunctionMcpBridge>) -> Option<String> {
    state.monitor_deck()
}

#[tauri::command]
pub fn junction_live_monitor_set(
    app: AppHandle,
    state: State<'_, JunctionMcpBridge>,
    deck: Option<String>,
) -> Result<Option<String>, String> {
    state.set_monitor_deck(&app, deck)
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("Junction MCPの認証情報を生成できません: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn development_bridge_config() -> Result<Option<(u16, String)>, String> {
    if !cfg!(debug_assertions) {
        return Ok(None);
    }
    let url = std::env::var("PLUMDECK_JUNCTION_BRIDGE_URL").ok();
    let token = std::env::var("PLUMDECK_JUNCTION_BRIDGE_TOKEN").ok();
    match (url, token) {
        (None, None) => Ok(None),
        (Some(url), Some(token)) => {
            let port = url
                .strip_prefix("http://127.0.0.1:")
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|value| *value != 0)
                .ok_or_else(|| "開発用Junction MCPブリッジのURLが不正です".to_string())?;
            if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("開発用Junction MCPブリッジの認証情報が不正です".into());
            }
            Ok(Some((port, token)))
        }
        _ => Err("開発用Junction MCPブリッジのURLと認証情報が揃っていません".into()),
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn handle_connection(
    stream: &mut TcpStream,
    token: &str,
    app: &AppHandle,
    supervisor: &Arc<EngineSupervisor>,
    shared: &Arc<BridgeShared>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let (authorization, body) = match read_http_request(stream) {
        Ok(value) => value,
        Err((status, code, message)) => {
            let _ = write_json(stream, status, &bridge_error(code, message, false));
            return;
        }
    };
    let expected = format!("Bearer {token}");
    if !constant_time_equal(&authorization, &expected) {
        let _ = write_json(
            stream,
            401,
            &bridge_error(
                "unauthorized",
                "Junction MCPブリッジを認証できません",
                false,
            ),
        );
        return;
    }
    let request: BridgeRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_json(
                stream,
                400,
                &bridge_error("invalid_request", "Junction操作のJSONが不正です", false),
            );
            return;
        }
    };
    let response = dispatch_request(request, app, supervisor, shared);
    let _ = write_json(stream, 200, &response);
}

fn read_http_request(
    stream: &mut TcpStream,
) -> Result<(String, Vec<u8>), (u16, &'static str, &'static str)> {
    let mut received = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|_| (400, "invalid_request", "Junction操作を読み取れません"))?;
        if read == 0 {
            return Err((400, "invalid_request", "Junction操作が空です"));
        }
        received.extend_from_slice(&chunk[..read]);
        if received.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES {
            return Err((413, "request_too_large", "Junction操作が大きすぎます"));
        }
        if let Some(index) = find_bytes(&received, b"\r\n\r\n") {
            break index + 4;
        }
        if received.len() > MAX_HEADER_BYTES {
            return Err((
                431,
                "headers_too_large",
                "Junction操作のヘッダーが大きすぎます",
            ));
        }
    };
    let header = std::str::from_utf8(&received[..header_end])
        .map_err(|_| (400, "invalid_request", "Junction操作のヘッダーが不正です"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    if request_parts.next() != Some("POST") || request_parts.next() != Some("/junction") {
        return Err((404, "not_found", "Junction MCPブリッジの操作先が不正です"));
    }
    let mut authorization = None;
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "authorization" => authorization = Some(value.trim().to_string()),
            "content-length" => {
                content_length = value.trim().parse::<usize>().ok();
            }
            _ => {}
        }
    }
    let length = content_length.ok_or((411, "length_required", "Junction操作の長さが必要です"))?;
    if length > MAX_BODY_BYTES {
        return Err((413, "request_too_large", "Junction操作が大きすぎます"));
    }
    while received.len() < header_end + length {
        let read = stream
            .read(&mut chunk)
            .map_err(|_| (400, "invalid_request", "Junction操作を読み取れません"))?;
        if read == 0 {
            return Err((400, "invalid_request", "Junction操作が途中で終了しました"));
        }
        received.extend_from_slice(&chunk[..read]);
        if received.len() > header_end + MAX_BODY_BYTES {
            return Err((413, "request_too_large", "Junction操作が大きすぎます"));
        }
    }
    Ok((
        authorization.unwrap_or_default(),
        received[header_end..header_end + length].to_vec(),
    ))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_json(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{\"ok\":false}".to_vec());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Service Unavailable",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(&bytes)
}

fn dispatch_request(
    request: BridgeRequest,
    app: &AppHandle,
    supervisor: &Arc<EngineSupervisor>,
    shared: &Arc<BridgeShared>,
) -> Value {
    if request.action.len() > 64 {
        return bridge_error("invalid_action", "Junction操作名が不正です", false);
    }
    let arguments = match request.arguments {
        Value::Object(arguments) => arguments,
        _ => {
            return bridge_error(
                "invalid_arguments",
                "Junction操作の引数はオブジェクトで指定してください",
                false,
            )
        }
    };
    dispatch_action(&request.action, arguments, app, supervisor, shared)
}

fn dispatch_action(
    action: &str,
    mut arguments: Map<String, Value>,
    app: &AppHandle,
    supervisor: &Arc<EngineSupervisor>,
    shared: &Arc<BridgeShared>,
) -> Value {
    if matches!(action, "join" | "exchange.inspect" | "exchange.import") {
        if let Some(text) = arguments.get("text").and_then(Value::as_str) {
            if text.len() > 131_072 {
                return bridge_error(
                    "exchange_too_large",
                    "招待・返答の文字が大きすぎます",
                    false,
                );
            }
        }
    }
    match action {
        "prepare" => match supervisor.start(app, None, None) {
            Ok(status) => match serde_json::to_value(status) {
                Ok(status) => json!({"ok": true, "result": status}),
                Err(_) => {
                    bridge_error("prepare_failed", "音声エンジンの状態を読み取れません", true)
                }
            },
            Err(message) => bridge_error("prepare_failed", &message, true),
        },
        "exchange.export" => export_exchange(arguments, supervisor),
        "live.attach" => {
            let deck = arguments
                .get("deck")
                .and_then(Value::as_str)
                .map(str::to_string);
            let asset_id = arguments.get("assetId").and_then(Value::as_str);
            if !asset_id.is_some_and(valid_asset_id) {
                return bridge_error(
                    "invalid_asset",
                    "asset_idはJunction Liveの現在曲から指定してください",
                    false,
                );
            }
            let snapshot = match raw_snapshot(supervisor) {
                Ok(snapshot) => snapshot,
                Err(message) => return bridge_error("engine_unavailable", &message, true),
            };
            let current = snapshot
                .get("junctionTracks")
                .and_then(Value::as_array)
                .and_then(|tracks| {
                    tracks
                        .iter()
                        .find(|track| track.get("role").and_then(Value::as_str) == Some("current"))
                });
            let current_asset = current
                .and_then(|track| track.get("assetId"))
                .and_then(Value::as_str);
            if current_asset != asset_id {
                return bridge_error(
                    "stale_asset",
                    "Junction Liveの現在曲が更新されています。状態を再取得してください",
                    true,
                );
            }
            if current
                .and_then(|track| track.get("state"))
                .and_then(Value::as_str)
                != Some("ready")
            {
                return bridge_error(
                    "asset_not_ready",
                    "Junction Liveの現在曲はまだ受信・検証中です",
                    true,
                );
            }
            match set_monitor_deck(app, shared, deck) {
                Ok(deck) => json!({"ok": true, "result": {"deck": deck}}),
                Err(message) => bridge_error("invalid_deck", &message, false),
            }
        }
        "live.detach" => {
            let requested = arguments.get("deck").and_then(Value::as_str);
            let current = shared
                .monitor_deck
                .lock()
                .ok()
                .and_then(|deck| deck.clone());
            if current.as_deref() != requested {
                return bridge_error(
                    "monitor_mismatch",
                    "指定したデッキにはJunction Liveが割り当てられていません",
                    false,
                );
            }
            match set_monitor_deck(app, shared, None) {
                Ok(deck) => json!({"ok": true, "result": {"deck": deck}}),
                Err(message) => bridge_error("monitor_failed", &message, true),
            }
        }
        "mic.enabled" => {
            let Some(enabled) = arguments.get("enabled").and_then(Value::as_bool) else {
                return bridge_error(
                    "invalid_arguments",
                    "enabledをtrueまたはfalseで指定してください",
                    false,
                );
            };
            let mut params = json!({"microphone": {"enabled": enabled}});
            if let Ok(snapshot) = raw_snapshot(supervisor) {
                if snapshot.get("active").and_then(Value::as_bool) == Some(true) {
                    params["_junction"] = json!({
                        "sessionId": snapshot.get("sessionId").cloned().unwrap_or(Value::Null),
                        "epoch": snapshot.get("epoch").cloned().unwrap_or(Value::Null),
                        "actorPeerId": snapshot.get("localPeerId").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            engine_response(
                supervisor.send_current("audio.config.set", params),
                &[],
                shared,
            )
        }
        _ => {
            let (operation, revealed_exchange_fields): (&str, &[&str]) = match action {
                "snapshot" => ("junction.snapshot", &[]),
                "exchange.inspect" => ("junction.exchange.inspect", &[]),
                "audio.devices" => ("audio.devices.list", &[]),
                "network.get" => ("junction.network.get", &[]),
                "network.test" => ("junction.network.test", &[]),
                "network.configure" => ("junction.network.configure", &[]),
                "network.clear" => ("junction.network.clear", &[]),
                "create" => ("junction.create", &[]),
                "join" => ("junction.join", &["responseText"]),
                "invite.create" => ("junction.invite.create", &["inviteText"]),
                "exchange.import" => (
                    "junction.exchange.import",
                    &["inviteText", "responseText", "noticeText"],
                ),
                "peer.approve" => {
                    arguments.insert("accept".into(), Value::Bool(true));
                    ("junction.peer.approve", &[])
                }
                "peer.reject" => {
                    arguments.insert("accept".into(), Value::Bool(false));
                    ("junction.peer.approve", &["noticeText"])
                }
                "invite.cancel" => ("junction.invite.cancel", &["noticeText"]),
                "peer.retry" => ("junction.peer.retry", &[]),
                "profile.update" => ("junction.profile.update", &[]),
                "roster.reorder" => ("junction.roster.reorder", &[]),
                "session.start" => ("junction.session.start", &[]),
                "handoff.request" => ("junction.handoff.request", &[]),
                "handoff.cancel" => ("junction.handoff.cancel", &[]),
                "handoff.accept" => ("junction.handoff.accept", &[]),
                "recovery.resume" => ("junction.recovery.resume", &[]),
                "leave" => ("junction.leave", &[]),
                "end" => ("junction.end", &[]),
                "invite.rotate" => ("junction.invite.rotate", &["invite"]),
                "program.configure" => ("junction.program.configure", &[]),
                "program.record.start" => ("junction.program.record.start", &[]),
                "program.record.stop" => ("junction.program.record.stop", &[]),
                "private.load" => ("junction.private.load", &[]),
                "private.play" => ("junction.private.play", &[]),
                "private.pause" => ("junction.private.pause", &[]),
                "private.seek" => ("junction.private.seek", &[]),
                "private.gain" => ("junction.private.gain", &[]),
                "private.unload" => ("junction.private.unload", &[]),
                "private.state" => ("junction.private.state", &[]),
                _ => {
                    return bridge_error(
                        "unsupported_action",
                        "対応していないJunction操作です",
                        false,
                    )
                }
            };
            engine_response(
                supervisor.send_current(operation, Value::Object(arguments)),
                revealed_exchange_fields,
                shared,
            )
        }
    }
}

fn export_exchange(arguments: Map<String, Value>, supervisor: &Arc<EngineSupervisor>) -> Value {
    let kind = arguments.get("kind").and_then(Value::as_str);
    let field = match kind {
        Some("invite") => "inviteText",
        Some("response") => "responseText",
        Some("notice") => "noticeText",
        _ => {
            return bridge_error(
                "invalid_exchange_kind",
                "kindはinvite、response、noticeのいずれかで指定してください",
                false,
            )
        }
    };
    let peer_id = arguments.get("peerId").and_then(Value::as_str);
    if peer_id.is_some_and(|value| value.is_empty() || value.len() > 128) {
        return bridge_error("invalid_peer", "peer_idが不正です", false);
    }
    let snapshot = match raw_snapshot(supervisor) {
        Ok(snapshot) => snapshot,
        Err(message) => return bridge_error("engine_unavailable", &message, true),
    };
    let exchange = if let Some(peer_id) = peer_id {
        snapshot
            .get("participants")
            .and_then(Value::as_array)
            .and_then(|participants| {
                participants.iter().find(|participant| {
                    participant.get("peerId").and_then(Value::as_str) == Some(peer_id)
                })
            })
            .and_then(|participant| participant.get("exchange"))
    } else {
        snapshot.get("exchange")
    };
    let Some(exchange) = exchange.and_then(Value::as_object) else {
        return bridge_error(
            "exchange_not_found",
            "指定したJunction接続操作が見つかりません",
            false,
        );
    };
    let Some(text) = exchange.get(field).and_then(Value::as_str) else {
        return bridge_error(
            "exchange_not_ready",
            "交換テキストはまだ準備中です。少し待って再取得してください",
            true,
        );
    };
    let mut result = json!({
        "kind": kind,
        "peerId": peer_id,
        "state": exchange.get("state").cloned().unwrap_or(Value::Null),
        "text": text,
    });
    if let Value::Object(result) = &mut result {
        for key in ["inviteId", "expiresAt"] {
            if let Some(value) = exchange.get(key) {
                result.insert(key.into(), value.clone());
            }
        }
    }
    json!({"ok": true, "result": result, "revision": snapshot.get("revision")})
}

fn raw_snapshot(supervisor: &Arc<EngineSupervisor>) -> Result<Value, String> {
    let reply = supervisor.send_current("junction.snapshot", json!({}))?;
    if reply.ok {
        Ok(reply.data.unwrap_or(Value::Null))
    } else {
        Err(reply
            .error
            .as_ref()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Junctionの状態を取得できません")
            .to_string())
    }
}

fn engine_response(
    reply: Result<EngineReply, String>,
    revealed_exchange_fields: &[&str],
    shared: &Arc<BridgeShared>,
) -> Value {
    match reply {
        Ok(reply) if reply.ok => {
            let mut result = reply.data.unwrap_or(Value::Null);
            redact_value(&mut result, revealed_exchange_fields);
            if let Value::Object(object) = &mut result {
                object.insert(
                    "liveMonitorDeck".into(),
                    shared
                        .monitor_deck
                        .lock()
                        .ok()
                        .and_then(|deck| deck.clone())
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
            json!({"ok": true, "result": result, "revision": reply.rev})
        }
        Ok(reply) => json!({
            "ok": false,
            "error": reply.error.unwrap_or_else(|| json!({
                "code": "engine_error",
                "message": "Junction操作を完了できませんでした",
                "retryable": false,
            })),
            "revision": reply.rev,
        }),
        Err(message) => bridge_error("engine_unavailable", &message, true),
    }
}

fn redact_value(value: &mut Value, revealed_exchange_fields: &[&str]) {
    match value {
        Value::Object(object) => {
            for key in [
                "avatarDataUrl",
                "credential",
                "directory",
                "filePath",
                "invite",
                "inviteText",
                "noticeText",
                "path",
                "recovery",
                "responseText",
                "secret",
                "token",
            ] {
                if !revealed_exchange_fields.contains(&key) {
                    object.remove(key);
                }
            }
            for nested in object.values_mut() {
                redact_value(nested, revealed_exchange_fields);
            }
        }
        Value::Array(array) => {
            for nested in array {
                redact_value(nested, revealed_exchange_fields);
            }
        }
        _ => {}
    }
}

fn set_monitor_deck(
    app: &AppHandle,
    shared: &Arc<BridgeShared>,
    deck: Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = deck.as_deref() {
        if !matches!(value, "A" | "B" | "C" | "D") {
            return Err("デッキはA、B、C、Dのいずれかで指定してください".into());
        }
    }
    if let Ok(mut current) = shared.monitor_deck.lock() {
        *current = deck.clone();
    } else {
        return Err("Junction Liveの表示状態を更新できません".into());
    }
    app.emit(LIVE_MONITOR_EVENT, json!({"deck": deck}))
        .map_err(|error| format!("Junction Liveの表示を通知できません: {error}"))?;
    Ok(deck)
}

fn valid_asset_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bridge_error(code: &str, message: &str, retryable: bool) -> Value {
    json!({"ok": false, "error": {"code": code, "message": message, "retryable": retryable}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_requires_exact_value() {
        assert!(constant_time_equal("Bearer abc", "Bearer abc"));
        assert!(!constant_time_equal("Bearer abc", "Bearer abd"));
        assert!(!constant_time_equal("Bearer abc", "Bearer abc "));
    }

    #[test]
    fn redaction_removes_packets_paths_and_credentials_recursively() {
        let mut value = json!({
            "invite": "packet",
            "participants": [{"avatarDataUrl": "data:image/png;base64,x", "exchange": {"inviteText": "packet"}}],
            "junctionTracks": [{"path": "/private/cache.wav", "title": "Current"}],
            "turn": {"credential": "secret", "hasSecret": true},
        });
        redact_value(&mut value, &[]);
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("packet"));
        assert!(!encoded.contains("cache.wav"));
        assert!(!encoded.contains("data:image"));
        assert!(!encoded.contains("credential"));
        assert_eq!(value.pointer("/turn/hasSecret"), Some(&Value::Bool(true)));
        assert_eq!(
            value
                .pointer("/junctionTracks/0/title")
                .and_then(Value::as_str),
            Some("Current")
        );
    }

    #[test]
    fn redaction_reveals_only_the_requested_exchange_field() {
        let mut value = json!({
            "inviteText": "safe-for-this-result",
            "responseText": "not-for-this-result",
            "path": "/private/cache.wav",
            "nested": {"inviteText": "also-safe", "token": "never-safe"},
        });
        redact_value(&mut value, &["inviteText"]);
        assert_eq!(
            value.get("inviteText").and_then(Value::as_str),
            Some("safe-for-this-result")
        );
        assert_eq!(
            value.pointer("/nested/inviteText").and_then(Value::as_str),
            Some("also-safe")
        );
        assert!(value.get("responseText").is_none());
        assert!(value.get("path").is_none());
        assert!(value.pointer("/nested/token").is_none());
    }

    #[test]
    fn live_asset_ids_are_lowercase_sha256_hex() {
        assert!(valid_asset_id(&"a".repeat(64)));
        assert!(!valid_asset_id(&"A".repeat(64)));
        assert!(!valid_asset_id("../../etc/passwd"));
    }

    #[test]
    fn request_body_limit_leaves_room_for_exchange_packet() {
        assert!(MAX_BODY_BYTES > 131_072);
        assert!(MAX_BODY_BYTES < 1024 * 1024);
    }
}
