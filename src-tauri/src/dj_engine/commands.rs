//! webview へ公開する型付きコマンド。
//!
//! ここでは **バイナリのパスを引数に取らない**。実行対象はスーパーバイザが
//! 環境変数か開発時のビルド出力からのみ決める（任意コマンド実行の経路を作らない）。
//! ブロッキング処理は `spawn_blocking` に逃がし、UI スレッドを止めない。

use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, State};

use super::supervisor::{EngineConnection, EngineReply, EngineStatus, EngineSupervisor};

async fn run_blocking<T, F>(task: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(task).await {
        Ok(result) => result,
        Err(error) => Err(format!("エンジン処理タスクが失敗しました: {error}")),
    }
}

/// エンジンが使えるかどうかを常に返す。未インストールでもエラーにしない。
#[tauri::command]
pub async fn dj_engine_status(
    state: State<'_, Arc<EngineSupervisor>>,
) -> Result<EngineStatus, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || Ok(supervisor.status())).await
}

/// 明示的なオプトイン起動。既に動いていれば現状を返す。
#[tauri::command]
pub async fn dj_engine_start(
    app: AppHandle,
    state: State<'_, Arc<EngineSupervisor>>,
    output_device: Option<String>,
    recording_dir: Option<String>,
) -> Result<EngineStatus, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || supervisor.start(&app, output_device, recording_dir)).await
}

#[tauri::command]
pub async fn dj_engine_stop(
    state: State<'_, Arc<EngineSupervisor>>,
) -> Result<EngineStatus, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || supervisor.stop()).await
}

/// webview の再読み込み後に呼ぶ。セッションを張り直してスナップショットを返す。
#[tauri::command]
pub async fn dj_engine_connect(
    state: State<'_, Arc<EngineSupervisor>>,
) -> Result<EngineConnection, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || supervisor.connect()).await
}

/// 1 コマンドを送る。`session_id` は connect が返したものに限る。
#[tauri::command]
pub async fn dj_engine_send(
    state: State<'_, Arc<EngineSupervisor>>,
    session_id: String,
    op: String,
    params: Option<Value>,
) -> Result<EngineReply, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || supervisor.send(&session_id, &op, params.unwrap_or(Value::Null))).await
}

#[derive(serde::Deserialize)]
pub enum JunctionOperation {
    #[serde(rename = "exchange.inspect")] ExchangeInspect,
    #[serde(rename = "exchange.import")] ExchangeImport,
    #[serde(rename = "invite.create")] InviteCreate,
    #[serde(rename = "invite.cancel")] InviteCancel,
    #[serde(rename = "peer.retry")] PeerRetry,
    #[serde(rename = "network.get")] NetworkGet,
    #[serde(rename = "network.configure")] NetworkConfigure,
    #[serde(rename = "network.clear")] NetworkClear,
    #[serde(rename = "network.test")] NetworkTest,
    #[serde(rename = "lite.join")] LiteJoin,
    #[serde(rename = "lite.exchange")] LiteExchange,
    #[serde(rename = "lite.guest.offer")] LiteGuestOffer,
    #[serde(rename = "lite.peer.ensure")] LitePeerEnsure,
    #[serde(rename = "lite.peer.answer")] LitePeerAnswer,
    #[serde(rename = "lite.peer.remove")] LitePeerRemove,
    #[serde(rename = "lite.owner.set")] LiteOwnerSet,
    #[serde(rename = "lite.roster.set")] LiteRosterSet,
    #[serde(rename = "input.set")] InputSet,
    #[serde(rename = "input.release")] InputRelease,
    #[serde(rename = "snapshot")] Snapshot,
    #[serde(rename = "create")] Create,
    #[serde(rename = "join")] Join,
    #[serde(rename = "leave")] Leave,
    #[serde(rename = "end")] End,
    #[serde(rename = "invite.rotate")] InviteRotate,
    #[serde(rename = "peer.approve")] PeerApprove,
    #[serde(rename = "profile.update")] ProfileUpdate,
    #[serde(rename = "roster.reorder")] RosterReorder,
    #[serde(rename = "session.start")] SessionStart,
    #[serde(rename = "handoff.request")] HandoffRequest,
    #[serde(rename = "handoff.cancel")] HandoffCancel,
    #[serde(rename = "handoff.accept")] HandoffAccept,
    #[serde(rename = "recovery.resume")] RecoveryResume,
    #[serde(rename = "program.configure")] ProgramConfigure,
    #[serde(rename = "program.record.start")] RecordStart,
    #[serde(rename = "program.record.stop")] RecordStop,
    #[serde(rename = "private.load")] PrivateLoad,
    #[serde(rename = "private.play")] PrivatePlay,
    #[serde(rename = "private.pause")] PrivatePause,
    #[serde(rename = "private.seek")] PrivateSeek,
    #[serde(rename = "private.gain")] PrivateGain,
    #[serde(rename = "private.unload")] PrivateUnload,
    #[serde(rename = "private.state")] PrivateState,
}
impl JunctionOperation {
    fn wire(&self) -> &'static str {
        match self {
            Self::ExchangeInspect => "exchange.inspect", Self::ExchangeImport => "exchange.import",
            Self::InviteCreate => "invite.create", Self::InviteCancel => "invite.cancel", Self::PeerRetry => "peer.retry",
            Self::NetworkGet => "network.get", Self::NetworkConfigure => "network.configure",
            Self::NetworkClear => "network.clear", Self::NetworkTest => "network.test",
            Self::LiteJoin => "lite.join", Self::LiteExchange => "lite.exchange",
            Self::LiteGuestOffer => "lite.guest.offer",
            Self::LitePeerEnsure => "lite.peer.ensure", Self::LitePeerAnswer => "lite.peer.answer",
            Self::LitePeerRemove => "lite.peer.remove", Self::LiteOwnerSet => "lite.owner.set",
            Self::LiteRosterSet => "lite.roster.set",
            Self::InputSet => "input.set", Self::InputRelease => "input.release",
            Self::Snapshot => "snapshot", Self::Create => "create", Self::Join => "join",
            Self::Leave => "leave", Self::End => "end", Self::InviteRotate => "invite.rotate",
            Self::PeerApprove => "peer.approve", Self::HandoffRequest => "handoff.request",
            Self::ProfileUpdate => "profile.update", Self::RosterReorder => "roster.reorder",
            Self::SessionStart => "session.start",
            Self::HandoffCancel => "handoff.cancel", Self::HandoffAccept => "handoff.accept",
            Self::RecoveryResume => "recovery.resume",
            Self::ProgramConfigure => "program.configure", Self::RecordStart => "program.record.start",
            Self::RecordStop => "program.record.stop", Self::PrivateLoad => "private.load",
            Self::PrivatePlay => "private.play", Self::PrivatePause => "private.pause", Self::PrivateSeek => "private.seek",
            Self::PrivateGain => "private.gain", Self::PrivateUnload => "private.unload", Self::PrivateState => "private.state",
        }
    }
}
/// Local typed gateway only: all session authority belongs to the native runtime.
#[tauri::command]
pub async fn junction_command(state: State<'_, Arc<EngineSupervisor>>, session_id: String,
    op: JunctionOperation, params: Option<Value>) -> Result<EngineReply, String> {
    let supervisor = state.inner().clone();
    run_blocking(move || supervisor.send(&session_id, &format!("junction.{}", op.wire()), params.unwrap_or(serde_json::json!({})))).await
}
