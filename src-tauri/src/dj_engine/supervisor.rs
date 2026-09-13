//! ネイティブ DJ エンジンのスーパーバイザ。
//!
//! 役割は「1 プロセスを長寿命で持ち、コマンド ID で返信を相関し、
//! イベントを webview へ中継する」ことだけ。プロトコルのスキーマは
//! 解釈しない（`kind` / `id` などの封筒だけを見る）ので、シミュレータが
//! Mixxx 由来の C++ ホストへ置き換わってもこの層は変更不要。
//!
//! セキュリティ上の前提:
//! - 実行するバイナリのパスは **webview からは決して受け取らない**。
//!   環境変数 `PLUMDECK_DJ_ENGINE_BIN`（絶対パス）か、開発時のビルド出力のみ。
//! - シェルを経由せず `std::process::Command` で直接起動する。
//! - ネットワークリスナは一切開かない。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

#[path = "notification_relay.rs"]
mod notification_relay;
use notification_relay::NotificationRelay;

/// スーパーバイザが話すプロトコルバージョン。
/// `native/dj-engine-host/src/protocol.rs` の `PROTOCOL_VERSION` と一致させる。
pub const PROTOCOL_VERSION: u64 = 1;

/// webview へのイベントチャネル。
pub const EVENT_CHANNEL: &str = "dj-engine://event";
pub const STATUS_CHANNEL: &str = "dj-engine://status";

/// 同時に待てるコマンド数の上限。超えたら即座に拒否する（無制限に積まない）。
const MAX_IN_FLIGHT: usize = 64;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 60_000;
/// 壁時計モードのティック間隔。
const TICK_MS: u64 = 20;
/// stop 時に正常終了を待つ猶予。
// Give recorder finalization and audio/transport teardown time to finish before
// the bounded fallback, including on Windows with a busy storage device.
const STOP_GRACE: Duration = Duration::from_secs(5);
const WRITER_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// 起動できるバイナリが見つかるか。
    pub installed: bool,
    pub running: bool,
    pub binary_path: Option<String>,
    pub engine_id: Option<String>,
    /// webview が保持すべきクライアントセッション ID。
    pub session_id: Option<String>,
    pub protocol: Option<u64>,
    /// 現在の実装が音を出さないシミュレータか。
    pub simulated: bool,
    pub implementation: Option<String>,
    pub last_error: Option<String>,
    /// 起動できない理由（未インストールなど）。UI はこれを出して素直に劣化する。
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineConnection {
    pub session_id: String,
    pub engine_id: String,
    pub protocol: u64,
    pub engine: Value,
    pub snapshot: Value,
    pub rev: u64,
}

/// エンジンの返信。`ok=false` はエンジンが返した構造化エラー。
/// スーパーバイザ自身の失敗は `Err(String)` で返す（層を混ぜない）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

struct Running {
    child: Arc<Mutex<Child>>,
    notifications: Arc<NotificationRelay>,
    shutdown_warning: Arc<Mutex<Option<String>>>,
    writer: Option<SyncSender<String>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    alive: Arc<AtomicBool>,
    next_command_id: u64,
    binary_path: PathBuf,
    engine_id: String,
    session_id: Option<String>,
    protocol: u64,
    engine_info: Value,
}

pub struct EngineSupervisor {
    state: Mutex<Option<Running>>,
    last_error: Mutex<Option<String>>,
    /// start / stop / reconnect change process or session identity and must not overlap.
    lifecycle: Mutex<()>,
    timeout: Duration,
}

impl Default for EngineSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineSupervisor {
    pub fn new() -> Self {
        let timeout_ms = std::env::var("PLUMDECK_DJ_ENGINE_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        EngineSupervisor {
            state: Mutex::new(None),
            last_error: Mutex::new(None),
            lifecycle: Mutex::new(()),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// 起動していなくても必ず答えられる状態問い合わせ。
    pub fn status(&self) -> EngineStatus {
        let mut guard = lock(&self.state);
        if let Some(running) = guard.as_mut() {
            // 子プロセスが死んでいれば回収して running=false にする。
            if matches!(lock(&running.child).try_wait(), Ok(Some(_))) {
                running.alive.store(false, Ordering::SeqCst);
            }
        }
        let last_error = lock(&self.last_error).clone();
        Self::status_from(&guard, last_error)
    }

    pub fn start(
        &self,
        app: &AppHandle,
        output_device: Option<String>,
        recording_dir: Option<String>,
    ) -> Result<EngineStatus, String> {
        let _lifecycle = lock(&self.lifecycle);
        self.start_serialized(app, output_device, recording_dir)
    }

    fn start_serialized(
        &self,
        app: &AppHandle,
        output_device: Option<String>,
        recording_dir: Option<String>,
    ) -> Result<EngineStatus, String> {
        {
            let guard = lock(&self.state);
            if let Some(running) = guard.as_ref() {
                if running.alive.load(Ordering::SeqCst) {
                    let last_error = lock(&self.last_error).clone();
                    return Ok(Self::status_from(&guard, last_error));
                }
            }
        }
        // 死んだプロセスが残っていれば片付ける。
        self.stop_internal();

        let binary_path = resolve_binary().inspect_err(|error| {
            self.record_error(error);
        })?;

        let output_device = normalize_output_device(output_device)?;
        let mut command = Command::new(&binary_path);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.env("PLUMDECK_WAVEFORM_CACHE", crate::waveform::cache_root(app)?);
        command
            .arg(format!("--tick-ms={TICK_MS}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env_remove("PLUMDECK_MIXXX_OUTPUT_DEVICE");
        if let Some(output_device) = output_device {
            command.env("PLUMDECK_MIXXX_OUTPUT_DEVICE", output_device);
        }
        // 保存先はアプリの設定。未指定ならエンジン既定の ~/Music/plumdeck Recordings。
        if let Some(recording_dir) = normalize_recording_dir(recording_dir)? {
            command.env("PLUMDECK_MIXXX_RECORDING_DIR", recording_dir);
        }
        let mut child = command.spawn().map_err(|error| {
            let message = format!(
                "エンジンを起動できませんでした ({}): {error}",
                binary_path.display()
            );
            self.record_error(&message);
            message
        })?;

        let mut stdin = child.stdin.take().ok_or("stdin を取得できませんでした")?;
        let stdout = child.stdout.take().ok_or("stdout を取得できませんでした")?;
        let stderr = child.stderr.take();

        let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let child = Arc::new(Mutex::new(child));
        let shutdown_warning = Arc::new(Mutex::new(None));
        let notifications = {
            let relay_ref = Arc::new(OnceLock::<Weak<NotificationRelay>>::new());
            let failure_relay_ref = Arc::clone(&relay_ref);
            let scheduler_app = app.clone();
            let emitter_app = app.clone();
            let child = Arc::clone(&child);
            let alive = Arc::clone(&alive);
            let pending = Arc::clone(&pending);
            let shutdown_warning = Arc::clone(&shutdown_warning);
            let relay = NotificationRelay::new(
                Arc::new(move |task| {
                    scheduler_app
                        .run_on_main_thread(task)
                        .map_err(|error| error.to_string())
                }),
                Arc::new(move |channel, payload| {
                    emitter_app
                        .emit_str(channel, payload)
                        .map_err(|error| error.to_string())
                }),
                Arc::new(move |error| {
                    // A broken event stream cannot leave audio running with an
                    // unusable controller. Closing stdin allows recording
                    // finalization; a detached watchdog bounds shutdown time.
                    fail_event_delivery(
                        &child,
                        &alive,
                        &pending,
                        &shutdown_warning,
                        failure_relay_ref.get().cloned(),
                        error,
                    );
                }),
            );
            // No notifications are submitted until after this initialization.
            let _ = relay_ref.set(Arc::downgrade(&relay));
            relay
        };

        // stdout: プロトコル専用。
        {
            let notifications = Arc::clone(&notifications);
            let pending = Arc::clone(&pending);
            let alive = Arc::clone(&alive);
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(error) => {
                            eprintln!("[dj-engine] stdout の読み取りに失敗しました: {error}");
                            break;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("[dj-engine] NDJSON として解釈できません: {error}");
                            continue;
                        }
                    };
                    route_protocol_value(value, &pending, &notifications);
                }

                // EOF = 子プロセスの終了。待っている要求を全部失敗させる。
                alive.store(false, Ordering::SeqCst);
                let waiters: Vec<Sender<Value>> =
                    lock(&pending).drain().map(|(_, tx)| tx).collect();
                for waiter in waiters {
                    let _ = waiter.send(json!({
                        "kind": "error",
                        "error": {
                            "code": "engine_exited",
                            "message": "エンジンプロセスが終了しました",
                            "retryable": true
                        }
                    }));
                }
                notifications.enqueue(
                    STATUS_CHANNEL,
                    json!({ "running": false, "reason": "engine_exited" }).to_string(),
                );
            });
        }

        // stderr: ログ専用。読み捨てないとパイプが詰まる。
        if let Some(stderr) = stderr {
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    match line {
                        Ok(line) => eprintln!("[dj-engine] {line}"),
                        Err(_) => break,
                    }
                }
            });
        }

        // A dedicated writer owns child stdin. dispatch() only performs a bounded,
        // non-blocking enqueue, so a host that stops reading cannot hold `state`
        // or prevent stop() from killing it.
        let (writer, writer_rx) = mpsc::sync_channel::<String>(MAX_IN_FLIGHT);
        {
            let notifications = Arc::clone(&notifications);
            let pending = Arc::clone(&pending);
            let alive = Arc::clone(&alive);
            thread::spawn(move || {
                while alive.load(Ordering::SeqCst) {
                    let line = match writer_rx.recv_timeout(WRITER_POLL) {
                        Ok(line) => line,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    if !alive.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Err(error) = write_line(&mut stdin, &line) {
                        eprintln!("[dj-engine] stdin への書き込みに失敗しました: {error}");
                        alive.store(false, Ordering::SeqCst);
                        let waiters: Vec<Sender<Value>> =
                            lock(&pending).drain().map(|(_, tx)| tx).collect();
                        for waiter in waiters {
                            let _ = waiter.send(json!({
                                "kind": "error",
                                "error": {
                                    "code": "engine_exited",
                                    "message": "エンジンへの送信経路が閉じました",
                                    "retryable": true
                                }
                            }));
                        }
                        notifications.enqueue(
                            STATUS_CHANNEL,
                            json!({ "running": false, "reason": "engine_stdin_closed" })
                                .to_string(),
                        );
                        break;
                    }
                }
            });
        }

        {
            let mut guard = lock(&self.state);
            *guard = Some(Running {
                child,
                notifications: Arc::clone(&notifications),
                shutdown_warning,
                writer: Some(writer),
                pending,
                alive,
                next_command_id: 0,
                binary_path,
                engine_id: String::new(),
                session_id: None,
                protocol: PROTOCOL_VERSION,
                engine_info: Value::Null,
            });
        }

        if let Err(error) = self.handshake() {
            // A failed handshake must not leave an unmanageable child behind.
            // Preserve the root cause for status(), then close/kill the process.
            self.record_error(&error);
            self.stop_internal();
            return Err(error);
        }
        *lock(&self.last_error) = None;

        let status = self.status();
        notifications.enqueue(
            STATUS_CHANNEL,
            json!({ "running": status.running }).to_string(),
        );
        Ok(status)
    }

    pub fn junction_active(&self) -> Result<bool, String> {
        let session = { let state = lock(&self.state); state.as_ref().and_then(|r| r.session_id.clone()) };
        let Some(session) = session else { return Ok(false); };
        let reply = self.dispatch("junction.snapshot", json!({}), Some(&session))?;
        if reply.get("ok").and_then(Value::as_bool) == Some(false) {
            let code = reply.pointer("/error/code").and_then(Value::as_str).unwrap_or("");
            if code == "unknown_op" || code == "unsupported_operation" { return Ok(false); }
            return Err("Junctionの利用状態を確認できません".into());
        }
        Ok(reply.pointer("/data/active").and_then(Value::as_bool).unwrap_or(false))
    }

    pub fn stop(&self) -> Result<EngineStatus, String> {
        let _lifecycle = lock(&self.lifecycle);
        if self.junction_active()? { return Err("Junctionから退出またはセッションを終了してからエンジンを停止してください".into()); }
        self.stop_internal();
        Ok(self.status())
    }

    /// webview の再読み込み後に呼ぶ。セッションを張り直し、
    /// 新しいスナップショットを返す。**再生は止めない。**
    pub fn connect(&self) -> Result<EngineConnection, String> {
        let _lifecycle = lock(&self.lifecycle);
        {
            let guard = lock(&self.state);
            let running = guard
                .as_ref()
                .ok_or_else(|| "エンジンが起動していません".to_string())?;
            if !running.alive.load(Ordering::SeqCst) {
                return Err("エンジンプロセスが終了しています".to_string());
            }
        }

        self.handshake()?;

        let (session_id, engine_id, protocol, engine_info) = {
            let guard = lock(&self.state);
            let running = guard
                .as_ref()
                .ok_or_else(|| "エンジンが起動していません".to_string())?;
            (
                running
                    .session_id
                    .clone()
                    .ok_or_else(|| "セッションが確立していません".to_string())?,
                running.engine_id.clone(),
                running.protocol,
                running.engine_info.clone(),
            )
        };

        let reply = self.dispatch("state.snapshot", json!({}), Some(&session_id))?;
        let snapshot = reply
            .get("data")
            .cloned()
            .ok_or_else(|| "スナップショットを取得できませんでした".to_string())?;
        let rev = reply.get("rev").and_then(Value::as_u64).unwrap_or(0);

        Ok(EngineConnection {
            session_id,
            engine_id,
            protocol,
            engine: engine_info,
            snapshot,
            rev,
        })
    }

    /// webview からの 1 コマンド。`session_id` は connect で得たものに限る。
    pub fn send(&self, session_id: &str, op: &str, params: Value) -> Result<EngineReply, String> {
        if op == "session.hello" {
            return Err(
                "session.hello は connect が管理します。直接呼び出さないでください".to_string(),
            );
        }
        {
            let guard = lock(&self.state);
            let running = guard
                .as_ref()
                .ok_or_else(|| "エンジンが起動していません".to_string())?;
            let current = running.session_id.as_deref().unwrap_or_default();
            if current != session_id {
                // 再読み込み前の古いセッションからの命令は適用しない。
                return Ok(EngineReply {
                    ok: false,
                    data: None,
                    error: Some(json!({
                        "code": "session_mismatch",
                        "message": "セッションが失効しています。再接続してください",
                        "retryable": false,
                        "details": { "expected": current, "received": session_id }
                    })),
                    rev: None,
                });
            }
        }

        let reply = self.dispatch(op, params, Some(session_id))?;
        let rev = reply.get("rev").and_then(Value::as_u64);
        match reply.get("kind").and_then(Value::as_str) {
            Some("result") => Ok(EngineReply {
                ok: true,
                data: reply.get("data").cloned(),
                error: None,
                rev,
            }),
            _ => Ok(EngineReply {
                ok: false,
                data: None,
                error: Some(reply.get("error").cloned().unwrap_or(json!({
                    "code": "internal",
                    "message": "エンジンから想定外の応答を受け取りました",
                    "retryable": false
                }))),
                rev,
            }),
        }
    }

    /// Send through the engine session currently owned by this supervisor.
    ///
    /// This is intentionally crate-local: the authenticated Junction MCP bridge
    /// must share the same long-lived engine as the webview instead of starting a
    /// second audio engine. A session rotation racing this call is still rejected
    /// by `send`, exactly like a stale webview command.
    pub(crate) fn send_current(&self, op: &str, params: Value) -> Result<EngineReply, String> {
        let session_id = {
            let guard = lock(&self.state);
            let running = guard
                .as_ref()
                .ok_or_else(|| "音声エンジンが起動していません".to_string())?;
            running
                .session_id
                .clone()
                .ok_or_else(|| "音声エンジンのセッションが確立していません".to_string())?
        };
        self.send(&session_id, op, params)
    }

    // ------------------------------------------------------------------ 内部

    fn handshake(&self) -> Result<(), String> {
        let hello = self.dispatch(
            "session.hello",
            json!({ "clientName": "plumdeck-tauri", "protocol": PROTOCOL_VERSION }),
            None,
        )?;

        if hello.get("kind").and_then(Value::as_str) != Some("hello") {
            let message = format!("hello 応答が不正です: {hello}");
            self.record_error(&message);
            return Err(message);
        }
        let engine_id = hello
            .get("engineId")
            .and_then(Value::as_str)
            .ok_or_else(|| "hello に engineId がありません".to_string())?
            .to_string();
        let session_id = hello
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "hello に sessionId がありません".to_string())?
            .to_string();
        let protocol = hello
            .get("protocol")
            .and_then(Value::as_u64)
            .unwrap_or(PROTOCOL_VERSION);
        if protocol != PROTOCOL_VERSION {
            let message = format!(
                "エンジンのプロトコル {protocol} はこのビルド（{PROTOCOL_VERSION}）と互換ではありません"
            );
            self.record_error(&message);
            return Err(message);
        }

        let mut guard = lock(&self.state);
        let running = guard
            .as_mut()
            .ok_or_else(|| "エンジンが起動していません".to_string())?;
        running.engine_id = engine_id;
        running.session_id = Some(session_id);
        running.protocol = protocol;
        running.engine_info = hello.get("engine").cloned().unwrap_or(Value::Null);
        Ok(())
    }

    /// 1 コマンドを送り、対応する返信だけを待つ。
    /// ロックは書き込みの間だけ保持し、待機中は解放する。
    fn dispatch(&self, op: &str, params: Value, session_id: Option<&str>) -> Result<Value, String> {
        let params_bytes = serde_json::to_vec(&params)
            .map_err(|error| format!("コマンド引数をJSON化できません: {error}"))?;
        if params_bytes.len() > MAX_COMMAND_BYTES {
            return Err(format!(
                "コマンド引数が上限（{MAX_COMMAND_BYTES} bytes）を超えています"
            ));
        }

        let (receiver, command_id, command_pending) = {
            let mut guard = lock(&self.state);
            let running = guard
                .as_mut()
                .ok_or_else(|| "エンジンが起動していません".to_string())?;
            if !running.alive.load(Ordering::SeqCst) {
                return Err("エンジンプロセスが終了しています".to_string());
            }

            running.next_command_id += 1;
            let command_id = running.next_command_id;

            let mut envelope = serde_json::Map::new();
            envelope.insert("protocol".to_string(), json!(PROTOCOL_VERSION));
            envelope.insert("kind".to_string(), json!("command"));
            envelope.insert("id".to_string(), json!(command_id));
            envelope.insert("op".to_string(), json!(op));
            envelope.insert("params".to_string(), params);
            if op != "session.hello" {
                envelope.insert("sessionId".to_string(), json!(session_id));
                envelope.insert("engineId".to_string(), json!(running.engine_id));
            }
            let line = Value::Object(envelope).to_string();

            let (sender, receiver) = mpsc::channel::<Value>();
            {
                let mut pending = lock(&running.pending);
                if pending.len() >= MAX_IN_FLIGHT {
                    return Err(format!(
                        "エンジンへの未処理要求が上限（{MAX_IN_FLIGHT}）に達しました"
                    ));
                }
                pending.insert(command_id, sender);
            }

            let writer = running
                .writer
                .as_ref()
                .ok_or_else(|| "エンジンへの送信経路が閉じています".to_string())?
                .clone();
            // ID allocation and the non-blocking enqueue share one critical
            // section, preserving wire order across concurrent Tauri commands.
            match try_enqueue(&writer, line) {
                Ok(()) => {}
                Err(EnqueueError::Full) => {
                    lock(&running.pending).remove(&command_id);
                    return Err(format!(
                        "エンジンへの送信キューが上限（{MAX_IN_FLIGHT}）に達しました"
                    ));
                }
                Err(EnqueueError::Disconnected) => {
                    lock(&running.pending).remove(&command_id);
                    return Err("エンジンへの送信経路が閉じています".to_string());
                }
            }
            (receiver, command_id, Arc::clone(&running.pending))
        };

        match receiver.recv_timeout(self.timeout) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => {
                lock(&command_pending).remove(&command_id);
                Err(format!(
                    "エンジンが {} ms 以内に応答しませんでした（op={op}）",
                    self.timeout.as_millis()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                lock(&command_pending).remove(&command_id);
                Err("エンジンプロセスが終了しました".to_string())
            }
        }
    }

    fn stop_internal(&self) {
        let taken = lock(&self.state).take();
        let Some(mut running) = taken else {
            return;
        };
        running.notifications.close();
        // Drop the bounded sender. A responsive writer then closes stdin and the
        // engine exits itself. If it is blocked, the grace timeout below kills it.
        running.writer.take();
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            let child_status = lock(&running.child).try_wait();
            match child_status {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let mut child = lock(&running.child);
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => {
                    let _ = lock(&running.child).kill();
                    break;
                }
            }
        }
        running.alive.store(false, Ordering::SeqCst);
    }

    fn record_error(&self, message: &str) {
        *lock(&self.last_error) = Some(message.to_string());
    }

    fn status_from(state: &Option<Running>, last_error: Option<String>) -> EngineStatus {
        let resolved = resolve_binary();
        let installed = resolved.is_ok();
        let detail = resolved.as_ref().err().cloned();

        match state {
            Some(running) => {
                let notification_error = running.notifications.error();
                let shutdown_warning = lock(&running.shutdown_warning).clone();
                let running_now =
                    running.alive.load(Ordering::SeqCst) && notification_error.is_none();
                let implementation = running
                    .engine_info
                    .get("implementation")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let simulated = running
                    .engine_info
                    .get("simulated")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                EngineStatus {
                    installed: true,
                    running: running_now,
                    binary_path: Some(running.binary_path.display().to_string()),
                    engine_id: if running.engine_id.is_empty() {
                        None
                    } else {
                        Some(running.engine_id.clone())
                    },
                    session_id: running.session_id.clone(),
                    protocol: Some(running.protocol),
                    simulated,
                    implementation,
                    last_error: match (notification_error.or(last_error), shutdown_warning) {
                        (Some(error), Some(warning)) => Some(format!("{error}。{warning}")),
                        (error, warning) => error.or(warning),
                    },
                    detail: None,
                }
            }
            None => EngineStatus {
                installed,
                running: false,
                binary_path: resolved.ok().map(|path| path.display().to_string()),
                engine_id: None,
                session_id: None,
                protocol: None,
                // 現行の実装はシミュレータのみ。実音声が出せるまでこれは true。
                simulated: true,
                implementation: None,
                last_error,
                detail,
            },
        }
    }
}

/// Mutex の poison でアプリを落とさない。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_line<W: Write>(writer: &mut W, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueError {
    Full,
    Disconnected,
}

fn try_enqueue(writer: &SyncSender<String>, line: String) -> Result<(), EnqueueError> {
    match writer.try_send(line) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(EnqueueError::Full),
        Err(TrySendError::Disconnected(_)) => Err(EnqueueError::Disconnected),
    }
}

fn route_protocol_value(
    value: Value,
    pending: &Mutex<HashMap<u64, Sender<Value>>>,
    notifications: &NotificationRelay,
) {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(kind, "result" | "error" | "hello") {
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let waiter = lock(pending).remove(&id);
            if let Some(waiter) = waiter {
                let _ = waiter.send(value);
                return;
            }
        }
    }
    notifications.enqueue(EVENT_CHANNEL, value.to_string());
}

fn fail_event_delivery(
    child: &Arc<Mutex<Child>>,
    alive: &AtomicBool,
    pending: &Mutex<HashMap<u64, Sender<Value>>>,
    shutdown_warning: &Arc<Mutex<Option<String>>>,
    notifications: Option<Weak<NotificationRelay>>,
    error: &str,
) {
    alive.store(false, Ordering::SeqCst);
    let waiters: Vec<Sender<Value>> = lock(pending).drain().map(|(_, tx)| tx).collect();
    for waiter in waiters {
        let _ = waiter.send(json!({
            "kind": "error",
            "error": {
                "code": "engine_event_delivery_failed",
                "message": error,
                "retryable": true
            }
        }));
    }
    let child = Arc::clone(child);
    let shutdown_warning = Arc::clone(shutdown_warning);
    thread::spawn(move || {
        // The writer observes alive=false within WRITER_POLL and drops stdin.
        // EOF lets Mixxx stop recording/finalize the WAV before it exits.
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            if matches!(lock(&child).try_wait(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                let warning =
                    "正常終了が間に合わず強制停止しました。録音ファイルが未完了の可能性があります";
                *lock(&shutdown_warning) = Some(warning.to_string());
                eprintln!("[dj-engine] {warning}");
                let mut child = lock(&child);
                if let Err(error) = child.kill() {
                    let message = format!("エンジンを強制停止できませんでした: {error}");
                    *lock(&shutdown_warning) = Some(message.clone());
                    eprintln!("[dj-engine] {message}");
                } else {
                    let _ = child.try_wait();
                }
                drop(child);
                if let Some(relay) = notifications.as_ref().and_then(Weak::upgrade) {
                    relay.refresh_failure_status();
                }
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
}

/// 保存先ディレクトリ。webview から来る値なので、絶対パスだけを通す。
fn normalize_recording_dir(value: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = value else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 1024 || trimmed.contains(['\0', '\n', '\r']) {
        return Err("録音の保存先が不正です".to_string());
    }
    if !PathBuf::from(trimmed).is_absolute() {
        return Err("録音の保存先は絶対パスで指定してください".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

fn normalize_output_device(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > 256 || trimmed.contains(['\0', '\n', '\r']) {
        return Err("音声デバイス名が不正です".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

/// 実行するバイナリを決める。**webview からの入力は使わない。**
fn resolve_binary() -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("PLUMDECK_DJ_ENGINE_BIN") {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err("PLUMDECK_DJ_ENGINE_BIN は絶対パスで指定してください".to_string());
        }
        if !path.is_file() {
            return Err(format!(
                "PLUMDECK_DJ_ENGINE_BIN のパスにファイルがありません: {}",
                path.display()
            ));
        }
        return Ok(path);
    }

    #[cfg(target_os = "windows")]
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let host = directory.join("engine").join("plumdeck-mixxx-engine-host.exe");
            if host.is_file() { return Ok(host); }
        }
    }

    // Optional packaged Performance build. The nested .app must be bundled as
    // a whole because the executable depends on its Frameworks/Resources.
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(contents_dir) = current_exe
            .parent()
            .and_then(|macos_dir| macos_dir.parent())
        {
            let packaged_host = contents_dir
                .join("Resources")
                .join("PlumdeckMixxxHost.app")
                .join("Contents")
                .join("MacOS")
                .join("plumdeck-mixxx-engine-host");
            if packaged_host.is_file() {
                return Ok(packaged_host);
            }
        }
    }

    #[cfg(not(debug_assertions))]
    {
        Err("音声エンジンが同梱されていません。アプリを再インストールしてください。".to_string())
    }
    #[cfg(debug_assertions)]
    {
        // 開発時のフォールバック。配布バンドルにこのパスは存在しないので、
        // その場合は「未インストール」として素直に劣化する。
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());
        #[cfg(target_os = "windows")]
        {
            let staged = repo_root.join("native/mixxx-engine-host/stage/PlumdeckMixxxHost/plumdeck-mixxx-engine-host.exe");
            if staged.is_file() { return Ok(staged); }
        }
        let staged_host = repo_root
            .join("native")
            .join("mixxx-engine-host")
            .join("stage")
            .join("PlumdeckMixxxHost.app")
            .join("Contents")
            .join("MacOS")
            .join("plumdeck-mixxx-engine-host");
        if staged_host.is_file() {
            return Ok(staged_host);
        }

        let real_host = repo_root
            .join("native")
            .join("mixxx-engine-host")
            .join("build-upstream")
            .join("plumdeck-mixxx-engine-host");
        if real_host.is_file() {
            return Ok(real_host);
        }

        let target_dir = repo_root
            .join("native")
            .join("dj-engine-host")
            .join("target");
        for profile in ["release", "debug"] {
            let candidate = target_dir.join(profile).join(format!("dj-engine-sim{}", std::env::consts::EXE_SUFFIX));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        Err(format!(
            "エンジンバイナリが見つかりません。\
             `bash native/mixxx-engine-host/scripts/build-macos.sh` または \
             `cargo build --manifest-path native/dj-engine-host/Cargo.toml` を実行するか、\
             PLUMDECK_DJ_ENGINE_BIN に絶対パスを設定してください（探索先: {}）",
            target_dir.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[cfg(unix)]
    #[test]
    fn delivery_failure_terminates_its_child_and_fails_pending_commands() {
        // This test owns a new inert process; it never addresses the live app
        // or a native engine process belonging to the user's session.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read ignored; exit 0"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let child = Arc::new(Mutex::new(child));
        let warning = Arc::new(Mutex::new(None));
        let alive = AtomicBool::new(true);
        let (sender, receiver) = mpsc::channel();
        let pending = Mutex::new(HashMap::from([(1, sender)]));
        fail_event_delivery(
            &child,
            &alive,
            &pending,
            &warning,
            None,
            "notification overflow",
        );
        // Simulate the writer observing alive=false and closing its owned stdin.
        drop(stdin);
        assert!(!alive.load(Ordering::SeqCst));
        assert!(lock(&pending).is_empty());
        let reply = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(reply["error"]["code"], "engine_event_delivery_failed");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = lock(&child).try_wait().unwrap() {
                assert!(status.success(), "responsive child must finalize normally");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "failed relay left its child running"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(lock(&warning).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn unresponsive_child_is_forced_to_stop_only_after_grace_with_recording_warning() {
        let child = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let child = Arc::new(Mutex::new(child));
        let alive = AtomicBool::new(true);
        let warning = Arc::new(Mutex::new(None));
        let (scheduled, tasks) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let observed_warnings = Arc::new(Mutex::new(Vec::new()));
        let observations = Arc::clone(&observed_warnings);
        let status_warning = Arc::clone(&warning);
        let notifications = NotificationRelay::new(
            Arc::new(move |task| scheduled.send(task).map_err(|error| error.to_string())),
            Arc::new(move |channel, _| {
                assert_eq!(channel, STATUS_CHANNEL);
                lock(&observations).push(lock(&status_warning).clone());
                Ok(())
            }),
            Arc::new(|_| {}),
        );
        notifications.enqueue(
            EVENT_CHANNEL,
            "x".repeat(notification_relay::MAX_NOTIFICATION_BYTES + 1),
        );
        tasks.recv_timeout(Duration::from_secs(2)).unwrap()();
        assert_eq!(*lock(&observed_warnings), vec![None]);
        let started = Instant::now();
        fail_event_delivery(
            &child,
            &alive,
            &Mutex::new(HashMap::new()),
            &warning,
            Some(Arc::downgrade(&notifications)),
            "overflow",
        );
        let deadline = Instant::now() + STOP_GRACE + Duration::from_secs(2);
        loop {
            if let Some(status) = lock(&child).try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(Instant::now() < deadline, "watchdog did not stop its child");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.elapsed() >= STOP_GRACE);
        assert!(lock(&warning)
            .as_ref()
            .unwrap()
            .contains("録音ファイルが未完了"));
        tasks.recv_timeout(Duration::from_secs(2)).unwrap()();
        assert!(lock(&observed_warnings)[1].is_some());
        notifications.close();
    }

    #[cfg(unix)]
    #[test]
    fn an_old_command_timeout_cannot_remove_a_new_sessions_reused_command_id() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();
        let (writer, commands) = mpsc::sync_channel(1);
        let old_pending = Arc::new(Mutex::new(HashMap::new()));
        let notifications = NotificationRelay::new(
            Arc::new(|_| Ok(())),
            Arc::new(|_, _| Ok(())),
            Arc::new(|error| panic!("unexpected failure: {error}")),
        );
        let supervisor = Arc::new(EngineSupervisor {
            state: Mutex::new(Some(Running {
                child: Arc::new(Mutex::new(child)),
                notifications,
                shutdown_warning: Arc::new(Mutex::new(None)),
                writer: Some(writer),
                pending: Arc::clone(&old_pending),
                alive: Arc::new(AtomicBool::new(true)),
                next_command_id: 0,
                binary_path: PathBuf::from("/usr/bin/true"),
                engine_id: "test".into(),
                session_id: Some("old".into()),
                protocol: PROTOCOL_VERSION,
                engine_info: Value::Null,
            })),
            last_error: Mutex::new(None),
            lifecycle: Mutex::new(()),
            timeout: Duration::from_millis(100),
        });
        let requester = Arc::clone(&supervisor);
        let request = thread::spawn(move || requester.dispatch("test", json!({}), Some("old")));
        commands.recv_timeout(Duration::from_secs(2)).unwrap();
        let (new_sender, _new_receiver) = mpsc::channel();
        let new_pending = Arc::new(Mutex::new(HashMap::from([(1, new_sender)])));
        {
            let mut state = lock(&supervisor.state);
            let running = state.as_mut().unwrap();
            running.pending = Arc::clone(&new_pending);
            running.next_command_id = 1;
            running.session_id = Some("new".into());
        }
        assert!(request.join().unwrap().is_err());
        assert!(lock(&old_pending).is_empty());
        assert!(lock(&new_pending).contains_key(&1));
        supervisor.stop_internal();
    }

    #[test]
    fn stdout_reply_bypasses_notifications_while_main_thread_holds_webview_lock() {
        let (scheduled, tasks) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let webviews = Arc::new(Mutex::new(()));
        let emit_webviews = Arc::clone(&webviews);
        let delivered = Arc::new(AtomicBool::new(false));
        let emit_delivered = Arc::clone(&delivered);
        let main_thread = thread::current().id();
        let notifications = NotificationRelay::new(
            Arc::new(move |task| scheduled.send(task).map_err(|error| error.to_string())),
            Arc::new(move |_, _| {
                assert_eq!(thread::current().id(), main_thread);
                let _guard = lock(&emit_webviews);
                emit_delivered.store(true, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(|error| panic!("unexpected delivery failure: {error}")),
        );
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (reply_sender, reply_receiver) = mpsc::channel();
        lock(&pending).insert(7, reply_sender);
        let main_webview_guard = lock(&webviews);
        let reader_notifications = Arc::clone(&notifications);
        let reader = thread::spawn(move || {
            route_protocol_value(
                json!({"kind": "event", "seq": 1}),
                &pending,
                &reader_notifications,
            );
            route_protocol_value(
                json!({"kind": "result", "id": 7}),
                &pending,
                &reader_notifications,
            );
        });
        // A direct background AppHandle.emit would require this same mutex and
        // stall before the stdout reader could correlate the following reply.
        let reply = reply_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(reply["id"], 7);
        reader.join().unwrap();
        assert!(!delivered.load(Ordering::SeqCst));
        drop(main_webview_guard);
        tasks.recv_timeout(Duration::from_secs(2)).unwrap()();
        assert!(delivered.load(Ordering::SeqCst));
        notifications.close();
    }

    #[test]
    fn writer_queue_rejects_full_without_waiting_for_a_reader() {
        let (writer, _reader) = mpsc::sync_channel(1);
        assert_eq!(try_enqueue(&writer, "first".into()), Ok(()));
        let started = Instant::now();
        assert_eq!(
            try_enqueue(&writer, "second".into()),
            Err(EnqueueError::Full)
        );
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn lifecycle_mutex_serializes_process_transitions() {
        let supervisor = Arc::new(EngineSupervisor::new());
        let entered = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Barrier::new(2));

        let guard = lock(&supervisor.lifecycle);
        let worker_supervisor = Arc::clone(&supervisor);
        let worker_entered = Arc::clone(&entered);
        let worker_ready = Arc::clone(&ready);
        let worker = thread::spawn(move || {
            worker_ready.wait();
            let _guard = lock(&worker_supervisor.lifecycle);
            worker_entered.store(true, Ordering::SeqCst);
        });

        ready.wait();
        thread::sleep(Duration::from_millis(20));
        assert!(!entered.load(Ordering::SeqCst));
        drop(guard);
        worker.join().unwrap();
        assert!(entered.load(Ordering::SeqCst));
    }

    #[test]
    fn output_device_name_is_bounded_and_single_line() {
        assert_eq!(
            normalize_output_device(Some("  BlackHole 2ch  ".into())).unwrap(),
            Some("BlackHole 2ch".into())
        );
        assert!(normalize_output_device(Some("bad\nname".into())).is_err());
        assert!(normalize_output_device(Some("x".repeat(257))).is_err());
    }
}
