"""Explicit MCP controls for Junction sessions.

Every tool maps to exactly one semantic desktop-bridge action.  There is no raw
operation escape hatch, so an MCP client cannot reach unrelated engine APIs.
"""

from __future__ import annotations

import json
import math
import re
import time
from typing import Annotated, Any, Dict, List, Literal, Optional

from mcp.server.mcpserver.exceptions import ToolError
from pydantic import Field

from mcp_server.instance import mcp
from mcp_server.junction_bridge import (
    JunctionBridgeError,
    MAX_EXCHANGE_TEXT_BYTES,
    bounded_exchange_text,
    post_junction_action,
)


Deck = Literal["A", "B", "C", "D"]
JunctionInputAssign = Literal["left", "thru", "right"]
TurnMode = Literal["rest", "temporary"]
ExchangeKind = Literal["invite", "response", "notice"]
TurnCue = Literal["one_more", "go_ahead", "hold", "ok"]
_RETIRED_HANDOFF = (
    "この操作は廃止されました。次のDJがフェーダーを上げると交代します。"
    "順番は junction_join_turn / junction_leave_turn / junction_reorder_roster、"
    "ホストの介入は junction_force_turn / junction_skip_turn / junction_release_tail を使ってください"
)
UnixEpochMilliseconds = Annotated[
    int,
    Field(
        description=(
            "TURN temporary credential expiry as Unix epoch milliseconds; "
            "it must be in the future and no more than 24 hours away"
        )
    ),
]
ExchangeText = Annotated[
    str, Field(min_length=1, max_length=MAX_EXCHANGE_TEXT_BYTES)
]
AssetId = Annotated[str, Field(pattern=r"^[0-9a-f]{64}$")]
JunctionInputLevel = Annotated[float, Field(ge=0.0, le=1.0)]
JunctionInputEq = Annotated[float, Field(ge=0.0, le=4.0)]
_ASSET_ID = re.compile(r"^[0-9a-f]{64}$")


def _call(action: str, arguments: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    try:
        return post_junction_action(action, arguments or {})
    except (JunctionBridgeError, ValueError) as exc:
        raise ToolError(str(exc)) from None


def _text(value: str, field: str, maximum: int, *, empty: bool = False) -> str:
    if not isinstance(value, str):
        raise ToolError(f"{field} must be a string")
    cleaned = value.strip()
    if not empty and not cleaned:
        raise ToolError(f"{field} must not be empty")
    if len(cleaned) > maximum:
        raise ToolError(f"{field} must be at most {maximum} characters")
    return cleaned


def _network_text(
    value: str,
    field: str,
    maximum_bytes: int,
    *,
    minimum_bytes: int = 1,
) -> str:
    """Match NetworkSettings::boundedText (UTF-8 bytes and no controls)."""
    if not isinstance(value, str):
        raise ToolError(f"{field} must be a string")
    size = len(value.encode("utf-8"))
    if size < minimum_bytes or size > maximum_bytes:
        raise ToolError(
            f"{field} must be {minimum_bytes}..{maximum_bytes} UTF-8 bytes"
        )
    if any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise ToolError(f"{field} must not contain control characters")
    return value


def _network_urls(values: List[str], field: str, *, required: bool) -> List[str]:
    if not isinstance(values, list) or len(values) > 8 or (required and not values):
        count = "1..8" if required else "at most 8"
        raise ToolError(f"{field} must contain {count} URLs")
    return [
        _network_text(value, f"{field} item", 1024)
        for value in values
    ]


def _profile(dj_name: str, avatar_data_url: Optional[str], theme_color: Optional[str]) -> Dict[str, Any]:
    result: Dict[str, Any] = {"djName": _text(dj_name, "dj_name", 40)}
    if avatar_data_url is not None:
        result["avatarDataUrl"] = _text(avatar_data_url, "avatar_data_url", 4096, empty=True)
    if theme_color is not None:
        result["themeColor"] = _text(theme_color, "theme_color", 32)
    return result


def _exchange(value: str) -> str:
    try:
        return bounded_exchange_text(value)
    except ValueError as exc:
        raise ToolError(str(exc)) from None


@mcp.tool()
def junction_get_state() -> Dict[str, Any]:
    """Junctionの参加者、順番、接続、引き継ぎ、Program、Live受信の最新状態を取得する。"""
    return _call("snapshot")


@mcp.tool()
def junction_prepare_engine() -> Dict[str, Any]:
    """アプリの新規起動後は最初に呼ぶ。デスクトップが所有する音声エンジンを安全に起動し、既に動作中ならそのインスタンスを再利用する。"""
    return _call("prepare")


@mcp.tool()
def junction_inspect_exchange(exchange_text: ExchangeText) -> Dict[str, Any]:
    """招待・返答・通知テキストを取り込まずに検証し、秘密を除いた概要だけを返す。"""
    return _call("exchange.inspect", {"text": _exchange(exchange_text)})


@mcp.tool()
def junction_get_exchange_text(
    kind: ExchangeKind,
    peer_id: Optional[str] = None,
) -> Dict[str, Any]:
    """非同期生成が完了した手動交換テキストを取得する。host側のinvite/noticeは通常peer_idが必要で、guest側のresponseは省略できる。既存の招待・参加処理は再実行しない。"""
    arguments: Dict[str, Any] = {"kind": kind}
    if peer_id is not None:
        arguments["peerId"] = _text(peer_id, "peer_id", 128)
    return _call("exchange.export", arguments)


@mcp.tool()
def junction_list_audio_devices() -> Dict[str, Any]:
    """Junction Program出力に選択できるローカル音声デバイスを取得する。"""
    return _call("audio.devices")


@mcp.tool()
def junction_get_network() -> Dict[str, Any]:
    """保存済みSTUN/TURN構成を、シークレットを伏せた状態で取得する。"""
    return _call("network.get")


@mcp.tool()
def junction_test_network(poll: bool = False, cancel: bool = False) -> Dict[str, Any]:
    """TURNを含む実ネットワーク疎通テストを開始・確認・中止する。開始は既定値、確認はpoll=true、中止はcancel=true。"""
    if poll and cancel:
        raise ToolError("poll and cancel cannot both be true")
    return _call("network.test", {"poll": poll, "cancel": cancel})


@mcp.tool()
def junction_configure_network(
    stun_urls: List[str],
    save: bool = True,
    turn_mode: Optional[TurnMode] = None,
    turn_urls: Optional[List[str]] = None,
    turn_secret: Optional[str] = None,
    turn_username: Optional[str] = None,
    turn_credential: Optional[str] = None,
    turn_expires_at: Optional[UnixEpochMilliseconds] = None,
) -> Dict[str, Any]:
    """JunctionのSTUNと任意のTURNを設定する。TURNなしはturn_modeを省略する。turn_expires_atはUnix epochミリ秒で、現在より後かつ24時間以内を指定する。秘密は応答に含まれない。"""
    arguments: Dict[str, Any] = {
        "stunUrls": _network_urls(stun_urls, "stun_urls", required=False),
        "save": save,
    }
    supplied_turn = any(value is not None for value in (turn_urls, turn_secret, turn_username, turn_credential, turn_expires_at))
    if turn_mode is None:
        if supplied_turn:
            raise ToolError("turn_mode is required when TURN fields are supplied")
        arguments["turn"] = {}
    else:
        urls = _network_urls(turn_urls or [], "turn_urls", required=True)
        turn: Dict[str, Any] = {"mode": turn_mode, "urls": urls}
        if turn_mode == "rest":
            if any(value is not None for value in (turn_username, turn_credential, turn_expires_at)):
                raise ToolError(
                    "REST TURN accepts turn_secret only; temporary credential fields must be omitted"
                )
            if turn_secret is None:
                raise ToolError("turn_secret is required for REST TURN")
            turn["secret"] = _network_text(
                turn_secret, "turn_secret", 512, minimum_bytes=32
            )
        else:
            if turn_secret is not None:
                raise ToolError("turn_secret must be omitted for temporary TURN")
            if turn_username is None or turn_credential is None or turn_expires_at is None:
                raise ToolError(
                    "temporary TURN requires turn_username, turn_credential, and turn_expires_at"
                )
            turn["username"] = _network_text(
                turn_username, "turn_username", 256
            )
            turn["credential"] = _network_text(
                turn_credential, "turn_credential", 1024
            )
            now_ms = int(time.time() * 1000)
            if turn_expires_at <= now_ms or turn_expires_at > now_ms + 86_400_000:
                raise ToolError(
                    "turn_expires_at must be a future Unix epoch millisecond value within 24 hours"
                )
            try:
                username_expiry_seconds = int(turn_username.split(":", 1)[0])
            except ValueError:
                raise ToolError(
                    "turn_username must begin with its Unix expiry in seconds"
                ) from None
            if (
                username_expiry_seconds < 1
                or username_expiry_seconds > (now_ms + 86_400_000) // 1000
                or username_expiry_seconds * 1000 < turn_expires_at - 1000
            ):
                raise ToolError(
                    "turn_username expiry must match turn_expires_at"
                )
            turn["expiresAt"] = turn_expires_at
        arguments["turn"] = turn
    config_bytes = json.dumps(
        {"stunUrls": arguments["stunUrls"], "turn": arguments["turn"]},
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    if len(config_bytes) > 16_384:
        raise ToolError("network configuration must be at most 16384 UTF-8 bytes")
    return _call("network.configure", arguments)


@mcp.tool()
def junction_clear_network() -> Dict[str, Any]:
    """保存済みのJunctionネットワーク構成を消去して既定値へ戻す。"""
    return _call("network.clear")


@mcp.tool()
def junction_create_session(
    dj_name: str,
    session_name: str,
    program_device: str,
    adopt_current: bool = False,
    start_in_lobby: bool = True,
    avatar_data_url: Optional[str] = None,
    theme_color: Optional[str] = None,
) -> Dict[str, Any]:
    """手動交換方式のJunctionセッションをホストとして作成する。通常はロビー開始を使う。"""
    arguments = _profile(dj_name, avatar_data_url, theme_color)
    arguments.update({
        "sessionName": _text(session_name, "session_name", 80),
        "programDevice": _text(program_device, "program_device", 1024),
        "adoptCurrent": adopt_current,
        "startInLobby": start_in_lobby,
        "exchangeMode": "manual",
    })
    return _call("create", arguments)


@mcp.tool()
def junction_join_session(
    exchange_text: ExchangeText,
    dj_name: str,
    avatar_data_url: Optional[str] = None,
    theme_color: Optional[str] = None,
) -> Dict[str, Any]:
    """ホストから受け取った手動招待テキストでJunctionセッションへ参加し、返答を生成する。"""
    arguments = _profile(dj_name, avatar_data_url, theme_color)
    arguments["text"] = _exchange(exchange_text)
    return _call("join", arguments)


@mcp.tool()
def junction_create_invite(
    peer_id: Optional[str] = None,
    dj_name: Optional[str] = None,
    avatar_data_url: Optional[str] = None,
    theme_color: Optional[str] = None,
) -> Dict[str, Any]:
    """ホストのロビーに参加枠を作り招待テキストを生成する。peer_id指定時はその参加者の招待を作り直す。"""
    arguments: Dict[str, Any] = {}
    if peer_id is not None:
        arguments["peerId"] = _text(peer_id, "peer_id", 128)
    if dj_name is not None:
        arguments["djName"] = _text(dj_name, "dj_name", 40)
    if avatar_data_url is not None:
        arguments["avatarDataUrl"] = _text(avatar_data_url, "avatar_data_url", 4096, empty=True)
    if theme_color is not None:
        arguments["themeColor"] = _text(theme_color, "theme_color", 32)
    return _call("invite.create", arguments)


@mcp.tool()
def junction_import_exchange(exchange_text: ExchangeText, peer_id: Optional[str] = None) -> Dict[str, Any]:
    """相手から届いた返答・更新招待・通知テキストを、任意の参加者枠へ取り込む。"""
    arguments: Dict[str, Any] = {"text": _exchange(exchange_text)}
    if peer_id is not None:
        arguments["peerId"] = _text(peer_id, "peer_id", 128)
    return _call("exchange.import", arguments)


@mcp.tool()
def junction_approve_participant(peer_id: str) -> Dict[str, Any]:
    """返答を確認済みの参加者をホストとして明示的に承認する。"""
    return _call("peer.approve", {"peerId": _text(peer_id, "peer_id", 128), "accept": True})


@mcp.tool()
def junction_reject_participant(peer_id: str) -> Dict[str, Any]:
    """返答待ちの参加者をホストとして拒否し、相手向け通知を生成する。"""
    return _call("peer.reject", {"peerId": _text(peer_id, "peer_id", 128)})


@mcp.tool()
def junction_cancel_invite(peer_id: str) -> Dict[str, Any]:
    """指定参加者の未完了招待・交換を取り消す。"""
    return _call("invite.cancel", {"peerId": _text(peer_id, "peer_id", 128)})


@mcp.tool()
def junction_retry_participant(peer_id: str) -> Dict[str, Any]:
    """指定参加者の一時切断を再試行し、必要なら再交換状態へ移す。"""
    return _call("peer.retry", {"peerId": _text(peer_id, "peer_id", 128)})


@mcp.tool()
def junction_update_profile(
    dj_name: str,
    avatar_data_url: Optional[str] = None,
    theme_color: Optional[str] = None,
) -> Dict[str, Any]:
    """Junction内の自分のDJ名、任意のアイコンとテーマ色を更新する。"""
    return _call("profile.update", _profile(dj_name, avatar_data_url, theme_color))


@mcp.tool()
def junction_reorder_roster(peer_ids: List[str]) -> Dict[str, Any]:
    """ホストとして全参加者のpeer_idを希望順に並べ替える。重複や欠落は許可されない。"""
    if not 1 <= len(peer_ids) <= 64:
        raise ToolError("peer_ids must contain 1..64 participants")
    cleaned = [_text(value, "peer_id", 128) for value in peer_ids]
    if len(set(cleaned)) != len(cleaned):
        raise ToolError("peer_ids must not contain duplicates")
    return _call("roster.reorder", {"peerIds": cleaned})


@mcp.tool()
def junction_start_session(performer_peer_id: str) -> Dict[str, Any]:
    """ホストとしてロビーからセッションを開始する。指定したDJが最初の順番（STANDBY）になり、そのDJがフェーダーを上げた瞬間にON AIRになる。"""
    return _call("session.start", {"performerPeerId": _text(performer_peer_id, "performer_peer_id", 128)})


@mcp.tool()
def junction_request_handoff(target_peer_id: Optional[str] = None) -> Dict[str, Any]:
    """廃止：指名による引き継ぎ要求。フェーダースタート方式では常にエラーで理由を返す。"""
    raise ToolError(_RETIRED_HANDOFF)


@mcp.tool()
def junction_cancel_handoff() -> Dict[str, Any]:
    """廃止：引き継ぎの取り消し。フェーダースタート方式では常にエラーで理由を返す（順番から外すには junction_leave_turn）。"""
    raise ToolError(_RETIRED_HANDOFF)


@mcp.tool()
def junction_accept_handoff() -> Dict[str, Any]:
    """廃止：準備OK・交代確定。フェーダースタート方式では常にエラーで理由を返す（READYのDJは junction_go_on_air）。"""
    raise ToolError(_RETIRED_HANDOFF)


@mcp.tool()
def junction_join_turn(peer_id: Optional[str] = None) -> Dict[str, Any]:
    """順番（タイムテーブル）の最後に入る。ホストはpeer_idで他のDJを入れられる。ゲストは省略して自分だけ。"""
    arguments: Dict[str, Any] = {}
    if peer_id is not None:
        arguments["peerId"] = _text(peer_id, "peer_id", 128)
    return _call("turn.join", arguments)


@mcp.tool()
def junction_leave_turn(peer_id: Optional[str] = None) -> Dict[str, Any]:
    """順番から外れる。ON AIRのDJは外せない。ホストはpeer_idで他のDJを外せる。"""
    arguments: Dict[str, Any] = {}
    if peer_id is not None:
        arguments["peerId"] = _text(peer_id, "peer_id", 128)
    return _call("turn.leave", arguments)


@mcp.tool()
def junction_set_b2b_repeat(enabled: bool) -> Dict[str, Any]:
    """ホスト：B2Bの繰り返し。有効にすると、交代を終えたDJを順番の最後に戻す（A,B,A,B…）。"""
    return _call("turn.repeat", {"enabled": enabled})


@mcp.tool()
def junction_set_failover(automatic: bool) -> Dict[str, Any]:
    """ホスト：ON AIRのDJが切断したとき、READYの次のDJへ自動で交代するか（False は確認してから）。"""
    return _call("turn.failover", {"auto": automatic})


@mcp.tool()
def junction_go_on_air() -> Dict[str, Any]:
    """自分がREADYのとき、フェーダーを上げずに今すぐON AIRにする。READYでなければ理由を返す。"""
    return _call("turn.onair")


@mcp.tool()
def junction_force_turn() -> Dict[str, Any]:
    """ホスト：次のDJを強制的にON AIRにする（フェーダーを待たない）。前のDJの曲が残っている間は使えない。"""
    return _call("turn.force")


@mcp.tool()
def junction_skip_turn() -> Dict[str, Any]:
    """ホスト：次のDJを順番の最後へ回し、その次のDJをSTANDBYにする。"""
    return _call("turn.skip")


@mcp.tool()
def junction_release_tail() -> Dict[str, Any]:
    """前のDJの残りの曲を止めて交代を完了する。ON AIRのDJかホストが使える。"""
    return _call("turn.release")


@mcp.tool()
def junction_send_cue(kind: TurnCue) -> Dict[str, Any]:
    """ブースの合図を送る：one_more（あと1曲）、go_ahead（次どうぞ）、hold（少し待って）、ok（OK）。"""
    return _call("turn.cue", {"kind": kind})


@mcp.tool()
def junction_resume_recovery() -> Dict[str, Any]:
    """ホストの復旧状態から、ホスト手元の演奏を新しいepochで安全に再開する。"""
    return _call("recovery.resume")


@mcp.tool()
def junction_leave_session() -> Dict[str, Any]:
    """自分が演奏者でないことを確認したうえでJunctionセッションから退出する。"""
    return _call("leave")


@mcp.tool()
def junction_end_session() -> Dict[str, Any]:
    """ホストとしてJunctionセッション全体を終了する。"""
    return _call("end")


@mcp.tool()
def junction_configure_program(program_device: str, gain: Optional[float] = None) -> Dict[str, Any]:
    """ホストのJunction Program（統合音源）の会場出力先と任意のゲインを設定する。"""
    arguments: Dict[str, Any] = {"programDevice": _text(program_device, "program_device", 1024)}
    if gain is not None:
        if not math.isfinite(gain) or not 0 <= gain <= 2:
            raise ToolError("gain must be finite and within 0..2")
        arguments["gain"] = gain
    return _call("program.configure", arguments)


@mcp.tool()
def junction_start_program_recording(path: str) -> Dict[str, Any]:
    """ホストの統合済みJunction Program音声を指定したローカルパスへ録音開始する。"""
    return _call("program.record.start", {"path": _text(path, "path", 4096)})


@mcp.tool()
def junction_stop_program_recording() -> Dict[str, Any]:
    """実行中のJunction Program録音を停止して確定する。"""
    return _call("program.record.stop")


@mcp.tool()
def junction_load_private_preview(path: str) -> Dict[str, Any]:
    """手元だけに聞こえるJunctionプライベートプレビューへローカル音源をロードする。"""
    return _call("private.load", {"path": _text(path, "path", 4096)})


@mcp.tool()
def junction_play_private_preview() -> Dict[str, Any]:
    """ロード済みのプライベートプレビューを再生する（Programには送らない）。"""
    return _call("private.play")


@mcp.tool()
def junction_pause_private_preview() -> Dict[str, Any]:
    """プライベートプレビューを一時停止する。"""
    return _call("private.pause")


@mcp.tool()
def junction_seek_private_preview(position_ms: float) -> Dict[str, Any]:
    """プライベートプレビューをミリ秒位置へ移動する。"""
    if not math.isfinite(position_ms) or position_ms < 0:
        raise ToolError("position_ms must be finite and non-negative")
    return _call("private.seek", {"positionMs": position_ms})


@mcp.tool()
def junction_set_private_preview_gain(gain: float) -> Dict[str, Any]:
    """手元だけに聞こえるプライベートプレビュー音量を0..1で設定する。"""
    if not math.isfinite(gain) or not 0 <= gain <= 1:
        raise ToolError("gain must be finite and within 0..1")
    return _call("private.gain", {"gain": gain})


@mcp.tool()
def junction_unload_private_preview() -> Dict[str, Any]:
    """プライベートプレビューから音源を取り外す。"""
    return _call("private.unload")


@mcp.tool()
def junction_get_private_preview_state() -> Dict[str, Any]:
    """プライベートプレビューのロード、再生、位置、長さ、エラー状態を取得する。"""
    return _call("private.state")


@mcp.tool()
def junction_set_microphone_enabled(enabled: bool) -> Dict[str, Any]:
    """Junctionの権限制御に従ってローカルDJマイクのProgram送出を有効・無効にする。"""
    return _call("mic.enabled", {"enabled": enabled})


@mcp.tool()
def junction_attach_live_monitor(deck: Deck, asset_id: AssetId) -> Dict[str, Any]:
    """受信済みJunction Live音源をA〜Dの表示専用モニターデッキへ割り当てる。Master音声や実デッキは変更しない。"""
    if not _ASSET_ID.fullmatch(asset_id):
        raise ToolError("asset_id must be a lowercase SHA-256 hex string")
    return _call("live.attach", {"deck": deck, "assetId": asset_id})


@mcp.tool()
def junction_detach_live_monitor(deck: Deck) -> Dict[str, Any]:
    """A〜Dの表示専用Junction Liveモニターを外し、下のローカルデッキ表示へ戻す。"""
    return _call("live.detach", {"deck": deck})


@mcp.tool()
def junction_configure_input(
    volume: Optional[JunctionInputLevel] = None,
    assign: Optional[JunctionInputAssign] = None,
    eq_low: Optional[JunctionInputEq] = None,
    eq_mid: Optional[JunctionInputEq] = None,
    eq_high: Optional[JunctionInputEq] = None,
    cue: Optional[bool] = None,
) -> Dict[str, Any]:
    """受信中のJUNCTION MASTER（前のDJの現在の音を受ける仮想入力チャンネル）の音量、クロスフェーダー割当、3バンドEQ、ヘッドホンCUEを変更する。少なくとも1項目を指定する。"""
    arguments: Dict[str, Any] = {}
    for source, target in (
        (volume, "volume"),
        (eq_low, "eqLow"),
        (eq_mid, "eqMid"),
        (eq_high, "eqHigh"),
    ):
        if source is not None:
            if not isinstance(source, (int, float)) or isinstance(source, bool) or not math.isfinite(source):
                raise ToolError(f"{target} must be a finite number")
            arguments[target] = float(source)
    if assign is not None:
        arguments["orientation"] = {"left": 0, "thru": 1, "right": 2}[assign]
    if cue is not None:
        arguments["pfl"] = cue
    if not arguments:
        raise ToolError("at least one JUNCTION input setting is required")
    return _call("input.set", arguments)


@mcp.tool()
def junction_release_input() -> Dict[str, Any]:
    """JUNCTION MASTERから前のDJを解放し、フェード済みの前任DJへ送出停止を通知する。"""
    return _call("input.release")
