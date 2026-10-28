"""Application services for the library workflow tools introduced in schema v5.

The module deliberately keeps filesystem mutation behind explicit plan/apply
operations.  Audio files are read for identity/serialization but are never
renamed, tagged or deleted by these workflows.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import uuid
import zipfile
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable
from urllib.parse import quote
from xml.etree import ElementTree as ET

from sqlalchemy import text
from sqlmodel import Session, select

from config import settings
from domain.constants import SUPPORTED_EXTENSIONS
from domain.models.setlist import SetlistTrack
from domain.models.track import Track
from domain.services.set_duration import calculate_set_duration
from infra.database.schema import ALL_TABLES, CURRENT_SCHEMA_VERSION
import infra.database.connection as db_connection
from infra import removable_devices


BACKUP_FORMAT_VERSION = 1
MAX_BACKUP_ENTRIES = 100_000
MAX_BACKUP_UNCOMPRESSED = 2 * 1024**4
WORKFLOW_LOCK = threading.RLock()
SENSITIVE_SETTING_WORDS = ("secret", "token", "credential", "invite", "answer", "recovery", "private", "password")

CONTROLLER_ACTIONS = {
    "library.browse", "library.load", "deck.play", "deck.cue", "deck.sync",
    "deck.tempo", "deck.jog", "deck.jog_touch", "deck.nudge", "deck.search",
    "mixer.trim", "mixer.eq_low", "mixer.eq_mid", "mixer.eq_high",
    "mixer.channel_fader", "mixer.crossfader", "mixer.filter", "mixer.cue",
    "pad.hotcue", "pad.loop", "pad.beatjump", "pad.sampler",
    "loop.in", "loop.out", "loop.exit", "loop.size",
}


def _rows(result) -> list[dict[str, Any]]:
    return [dict(row._mapping) for row in result]


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), default=str)


def _allowed_ui_setting(key: str, value: Any) -> bool:
    lowered = key.casefold()
    return isinstance(value, str) and (key == "vite-ui-theme" or key.startswith("plumdeck.")) \
        and not any(word in lowered for word in SENSITIVE_SETTING_WORDS)


def _app_version() -> str:
    try:
        package = Path(__file__).resolve().parents[3] / "package.json"
        return str(json.loads(package.read_text(encoding="utf-8"))["version"])
    except (OSError, KeyError, TypeError, json.JSONDecodeError):
        return "unknown"


def _sha256(path: Path, limit: int | None = None) -> str:
    digest = hashlib.sha256()
    read = 0
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            read += len(chunk)
            if limit is not None and read > limit:
                raise ValueError("ファイルが許可されたサイズを超えています")
            digest.update(chunk)
    return digest.hexdigest()


def _safe_real_file(value: str) -> Path:
    path = Path(value).expanduser()
    if not path.is_absolute():
        raise ValueError("絶対パスを指定してください")
    resolved = path.resolve(strict=True)
    if not resolved.is_file():
        raise ValueError("ファイルが見つかりません")
    return resolved


def _file_identity(path: Path, with_hash: bool = True) -> dict[str, Any]:
    stat = path.stat()
    return {
        "path": str(path),
        "size_bytes": stat.st_size,
        "mtime_ns": stat.st_mtime_ns,
        "sha256": _sha256(path) if with_hash else None,
    }


def _operation(session: Session, operation_id: str, kind: str, target: str, state: str,
               detail: dict[str, Any], progress: float = 0) -> None:
    session.exec(text("""
        INSERT INTO operation_journal (id,kind,target,state,progress,detail_json)
        VALUES (:id,:kind,:target,:state,:progress,:detail)
        ON CONFLICT (id) DO UPDATE SET state=excluded.state,progress=excluded.progress,
          detail_json=excluded.detail_json,updated_at=now()
    """), params={"id": operation_id, "kind": kind, "target": target, "state": state,
                    "progress": progress, "detail": _json(detail)})


class MediaRepairService:
    def __init__(self, session: Session):
        self.session = session

    def diagnose(self, track_ids: list[int] | None = None, old_root: str | None = None,
                 new_root: str | None = None) -> dict[str, Any]:
        query = select(Track)
        if track_ids:
            query = query.where(Track.id.in_(track_ids))
        tracks = self.session.exec(query.order_by(Track.id)).all()
        plan_id = f"repair-{uuid.uuid4()}"
        items: list[dict[str, Any]] = []
        old_root_path = Path(old_root).expanduser() if old_root else None
        new_root_path = Path(new_root).expanduser() if new_root else None
        candidates_by_name: dict[str, list[Path]] = {}
        if new_root_path and new_root_path.is_dir():
            for index, path in enumerate(new_root_path.rglob("*")):
                if index >= 20_000:
                    break
                if path.is_file() and not path.is_symlink():
                    candidates_by_name.setdefault(path.name, []).append(path)
        for track in tracks:
            source = Path(track.filepath).expanduser()
            state = "available" if source.is_file() and os.access(source, os.R_OK) else "missing"
            candidate: Path | None = None
            if old_root_path and new_root_path:
                try:
                    candidate = new_root_path / source.relative_to(old_root_path)
                except ValueError:
                    candidate = None
            stored = self.session.exec(text("SELECT * FROM track_media WHERE track_id=:id"), params={"id": track.id}).first()
            expected = dict(stored._mapping) if stored else {}
            if state == "available":
                identity = _file_identity(source)
                self.session.exec(text("""
                    INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at)
                    VALUES (:track_id,:sha256,:size_bytes,:mtime_ns,'available',now())
                    ON CONFLICT (track_id) DO UPDATE SET sha256=excluded.sha256,size_bytes=excluded.size_bytes,
                      mtime_ns=excluded.mtime_ns,status='available',last_verified_at=now(),
                      revision=track_media.revision+1
                """), params={"track_id": track.id, **identity})
                expected = identity
            if state == "missing" and (not candidate or not candidate.is_file()):
                same_name = candidates_by_name.get(source.name, [])
                sized = [path for path in same_name if not expected.get("size_bytes") or path.stat().st_size == expected["size_bytes"]]
                if expected.get("sha256"):
                    candidate = next((path for path in sized if _sha256(path) == expected["sha256"]), None)
                elif len(sized) == 1:
                    candidate = sized[0]
            candidate_state = "none"
            reason = "現在の音源を利用できます" if state == "available" else "候補を指定してください"
            if candidate and candidate.is_file():
                identity = _file_identity(candidate)
                if expected.get("sha256") and expected["sha256"] == identity["sha256"]:
                    candidate_state, reason = "confirmed", "保存済みSHA-256と一致"
                else:
                    candidate_state, reason = "needs_confirmation", "ファイル名・サイズ候補（内容の確認が必要）"
            items.append({"track_id": track.id, "title": track.title, "artist": track.artist,
                          "old_path": track.filepath, "old_revision": expected.get("revision", 1),
                          "access_state": state, "candidate_path": str(candidate) if candidate else None,
                          "candidate_state": candidate_state, "reason": reason})
        plan = {"id": plan_id, "state": "planned", "items": items,
                "summary": {key: sum(item["access_state"] == key for item in items)
                            for key in ("available", "missing")}}
        self.session.exec(text("INSERT INTO media_repair_operations (id,state,plan_json) VALUES (:id,'planned',:plan)"),
                          params={"id": plan_id, "plan": _json(plan)})
        _operation(self.session, plan_id, "media_repair", "tracks", "planned", plan)
        self.session.commit()
        return plan

    def apply(self, plan_id: str, selections: dict[str, str]) -> dict[str, Any]:
        row = self.session.exec(text("SELECT * FROM media_repair_operations WHERE id=:id"), params={"id": plan_id}).first()
        if not row:
            raise ValueError("修復計画が見つかりません")
        plan = json.loads(row._mapping["plan_json"])
        changed, conflicts = [], []
        try:
            for item in plan["items"]:
                chosen = selections.get(str(item["track_id"]))
                if not chosen:
                    continue
                track = self.session.get(Track, item["track_id"])
                if not track or track.filepath != item["old_path"]:
                    conflicts.append({"track_id": item["track_id"], "reason": "プレビュー後に参照が変更されました"})
                    continue
                candidate = _safe_real_file(chosen)
                owner = self.session.exec(select(Track).where(Track.filepath == str(candidate))).first()
                if owner and owner.id != track.id:
                    conflicts.append({"track_id": track.id, "reason": "候補は別の曲が使用しています"})
                    continue
                identity = _file_identity(candidate)
                saved = self.session.exec(text("SELECT sha256 FROM track_media WHERE track_id=:id"), params={"id": track.id}).first()
                if saved and saved[0] and saved[0] != identity["sha256"]:
                    conflicts.append({"track_id": track.id, "reason": "候補の内容が保存済み音源と一致しません"})
                    continue
                old_path = track.filepath
                track.filepath = str(candidate)
                self.session.add(track)
                self.session.exec(text("""
                    INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at)
                    VALUES (:track_id,:sha256,:size_bytes,:mtime_ns,'available',now())
                    ON CONFLICT(track_id) DO UPDATE SET sha256=excluded.sha256,size_bytes=excluded.size_bytes,
                      mtime_ns=excluded.mtime_ns,status='available',last_verified_at=now(),
                      revision=track_media.revision+1
                """), params={"track_id": track.id, **identity})
                changed.append({"track_id": track.id, "old_path": old_path, "new_path": str(candidate)})
            result = {"changed": changed, "conflicts": conflicts, "state": "completed_with_conflicts" if conflicts else "completed"}
            self.session.exec(text("UPDATE media_repair_operations SET state=:state,result_json=:result,updated_at=now() WHERE id=:id"),
                              params={"id": plan_id, "state": result["state"], "result": _json(result)})
            _operation(self.session, plan_id, "media_repair", "tracks", result["state"], result, 1)
            self.session.commit()
            return result
        except Exception:
            self.session.rollback()
            raise

    def undo(self, plan_id: str) -> dict[str, Any]:
        row = self.session.exec(text("SELECT result_json FROM media_repair_operations WHERE id=:id"), params={"id": plan_id}).first()
        if not row:
            raise ValueError("修復履歴が見つかりません")
        result = json.loads(row[0] or "{}")
        reverted, conflicts = [], []
        for change in result.get("changed", []):
            track = self.session.get(Track, change["track_id"])
            if not track or track.filepath != change["new_path"] or not Path(change["old_path"]).is_file():
                conflicts.append({"track_id": change["track_id"], "reason": "現在値または元ファイルが変化しています"})
                continue
            track.filepath = change["old_path"]
            self.session.add(track)
            reverted.append(change["track_id"])
        self.session.commit()
        return {"reverted": reverted, "conflicts": conflicts}


class VersionService:
    def __init__(self, session: Session):
        self.session = session

    def create(self, track_ids: list[int], name: str | None, labels: dict[str, dict[str, str]] | None = None) -> dict[str, Any]:
        unique = list(dict.fromkeys(track_ids))
        if len(unique) < 2:
            raise ValueError("2曲以上を選択してください")
        if len(self.session.exec(select(Track).where(Track.id.in_(unique))).all()) != len(unique):
            raise ValueError("曲が見つかりません")
        occupied = []
        for track_id in unique:
            row = self.session.exec(text("SELECT track_id,group_id FROM track_version_members WHERE track_id=:id"), params={"id": track_id}).first()
            if row:
                occupied.append(dict(row._mapping))
        if occupied:
            raise ValueError("選択曲の一部は既に別バージョングループに属しています")
        group_id = int(self.session.exec(text("INSERT INTO track_version_groups (name,preferred_track_id) VALUES (:name,:preferred) RETURNING id"),
                                         params={"name": name, "preferred": unique[0]}).one()[0])
        for track_id in unique:
            label = (labels or {}).get(str(track_id), {})
            self.session.exec(text("""
                INSERT INTO track_version_members (group_id,track_id,version_label,content_label,note)
                VALUES (:group_id,:track_id,:version,:content,:note)
            """), params={"group_id": group_id, "track_id": track_id,
                            "version": label.get("version_label", "Original"),
                            "content": label.get("content_label", "Unknown"), "note": label.get("note")})
        self.session.commit()
        return self.get(group_id)

    def get(self, group_id: int) -> dict[str, Any]:
        group = self.session.exec(text("SELECT * FROM track_version_groups WHERE id=:id"), params={"id": group_id}).first()
        if not group:
            raise ValueError("バージョングループが見つかりません")
        result = dict(group._mapping)
        result["members"] = _rows(self.session.exec(text("""
            SELECT m.*,t.title,t.artist,t.filepath,t.duration,t.bpm,t.key
            FROM track_version_members m JOIN tracks t ON t.id=m.track_id
            WHERE m.group_id=:id ORDER BY (m.track_id=:preferred) DESC,m.track_id
        """), params={"id": group_id, "preferred": result.get("preferred_track_id")}))
        return result

    def for_track(self, track_id: int) -> dict[str, Any] | None:
        row = self.session.exec(text("SELECT group_id FROM track_version_members WHERE track_id=:id"), params={"id": track_id}).first()
        return self.get(int(row[0])) if row else None

    def update(self, group_id: int, revision: int, name: str | None = None,
               preferred_track_id: int | None = None, members: list[dict[str, Any]] | None = None) -> dict[str, Any]:
        current = self.get(group_id)
        if int(current["revision"]) != revision:
            raise ValueError("別の画面でグループが更新されました")
        member_ids = {int(row["track_id"]) for row in current["members"]}
        if preferred_track_id is not None and preferred_track_id not in member_ids:
            raise ValueError("優先版は現在のメンバーから選択してください")
        self.session.exec(text("""
            UPDATE track_version_groups SET name=coalesce(:name,name),
              preferred_track_id=coalesce(:preferred,preferred_track_id),revision=revision+1,
              updated_at=now() WHERE id=:id
        """), params={"id": group_id, "name": name, "preferred": preferred_track_id})
        for member in members or []:
            if int(member["track_id"]) not in member_ids:
                raise ValueError("グループ外の曲は更新できません")
            self.session.exec(text("""
                UPDATE track_version_members SET version_label=:version,content_label=:content,note=:note
                WHERE group_id=:group AND track_id=:track
            """), params={"group": group_id, "track": member["track_id"],
                            "version": member.get("version_label", "Original"),
                            "content": member.get("content_label", "Unknown"), "note": member.get("note")})
        self.session.commit()
        return self.get(group_id)

    def swap_entry(self, entry_id: int, new_track_id: int, revision: int) -> dict[str, Any]:
        entry = self.session.get(SetlistTrack, entry_id)
        if not entry:
            raise ValueError("セットの行が見つかりません")
        if entry.revision != revision:
            raise ValueError("セットの行が別の画面で更新されました")
        old_group = self.for_track(entry.track_id)
        if not old_group or new_track_id not in {member["track_id"] for member in old_group["members"]}:
            raise ValueError("同じバージョングループの曲だけに差し替えられます")
        if not self.session.get(Track, new_track_id):
            raise ValueError("差し替え先の曲が見つかりません")
        entry.track_id = new_track_id
        entry.in_ms = 0
        entry.out_ms = None
        entry.playback_rate = 1
        entry.extra_duration_ms = 0
        entry.overlap_next_ms = 0
        entry.wordplay_json = None
        entry.revision += 1
        self.session.add(entry)
        previous = self.session.exec(select(SetlistTrack).where(
            SetlistTrack.setlist_id == entry.setlist_id, SetlistTrack.position == entry.position - 1)).first()
        if previous:
            previous.overlap_next_ms = 0
            previous.revision += 1
            self.session.add(previous)
        self.session.commit()
        return {"entry_id": entry.id, "track_id": entry.track_id, "revision": entry.revision}


DDJ400_PROFILE = {
    "id": "builtin-ddj400-v1", "schemaVersion": 1, "name": "Pioneer DDJ-400",
    "adapterId": "ddj400",
    "capabilities": ["midi-input", "midi-learn", "jog-relative", "high-resolution"],
    "source": {"url": "https://github.com/mixxxdj/mixxx/blob/f8b3523bb3be90621996c300ad23cfbe48ba8a5a/res/controllers/Pioneer-DDJ-400.midi.xml", "commit": "f8b3523bb3be90621996c300ad23cfbe48ba8a5a", "license": "GPL-2.0-or-later"},
    "bindings": [
        {"id": "play-a", "input": {"kind": "note", "channel": 0, "number": 11}, "encoding": "button", "actionId": "deck.play", "deck": "A", "trigger": "press"},
        {"id": "play-b", "input": {"kind": "note", "channel": 1, "number": 11}, "encoding": "button", "actionId": "deck.play", "deck": "B", "trigger": "press"},
        {"id": "load-a", "input": {"kind": "note", "channel": 6, "number": 70}, "encoding": "button", "actionId": "library.load", "deck": "A", "trigger": "press"},
        {"id": "load-b", "input": {"kind": "note", "channel": 6, "number": 71}, "encoding": "button", "actionId": "library.load", "deck": "B", "trigger": "press"},
        {"id": "browse", "input": {"kind": "cc", "channel": 6, "number": 64}, "encoding": "relative-twos-complement", "actionId": "library.browse"},
        {"id": "cue-a", "input": {"kind": "note", "channel": 0, "number": 12}, "encoding": "button", "actionId": "deck.cue", "deck": "A", "trigger": "hold"},
        {"id": "cue-b", "input": {"kind": "note", "channel": 1, "number": 12}, "encoding": "button", "actionId": "deck.cue", "deck": "B", "trigger": "hold"},
        {"id": "sync-a", "input": {"kind": "note", "channel": 0, "number": 88}, "encoding": "button", "actionId": "deck.sync", "deck": "A", "trigger": "toggle"},
        {"id": "sync-b", "input": {"kind": "note", "channel": 1, "number": 88}, "encoding": "button", "actionId": "deck.sync", "deck": "B", "trigger": "toggle"},
        {"id": "jog-a", "input": {"kind": "cc", "channel": 0, "number": 34}, "encoding": "relative-offset", "actionId": "deck.jog", "deck": "A"},
        {"id": "jog-b", "input": {"kind": "cc", "channel": 1, "number": 34}, "encoding": "relative-offset", "actionId": "deck.jog", "deck": "B"},
        {"id": "jog-touch-a", "input": {"kind": "note", "channel": 0, "number": 54}, "encoding": "button", "actionId": "deck.jog_touch", "deck": "A", "trigger": "hold"},
        {"id": "jog-touch-b", "input": {"kind": "note", "channel": 1, "number": 54}, "encoding": "button", "actionId": "deck.jog_touch", "deck": "B", "trigger": "hold"},
        {"id": "tempo-a", "input": {"kind": "cc14", "channel": 0, "number": 0}, "encoding": "absolute", "actionId": "deck.tempo", "deck": "A"},
        {"id": "tempo-b", "input": {"kind": "cc14", "channel": 1, "number": 0}, "encoding": "absolute", "actionId": "deck.tempo", "deck": "B"},
        {"id": "fader-a", "input": {"kind": "cc14", "channel": 0, "number": 19}, "encoding": "absolute", "actionId": "mixer.channel_fader", "deck": "A"},
        {"id": "fader-b", "input": {"kind": "cc14", "channel": 1, "number": 19}, "encoding": "absolute", "actionId": "mixer.channel_fader", "deck": "B"},
        {"id": "eq-high-a", "input": {"kind": "cc14", "channel": 0, "number": 7}, "encoding": "absolute", "actionId": "mixer.eq_high", "deck": "A"},
        {"id": "eq-high-b", "input": {"kind": "cc14", "channel": 1, "number": 7}, "encoding": "absolute", "actionId": "mixer.eq_high", "deck": "B"},
        {"id": "eq-mid-a", "input": {"kind": "cc14", "channel": 0, "number": 11}, "encoding": "absolute", "actionId": "mixer.eq_mid", "deck": "A"},
        {"id": "eq-mid-b", "input": {"kind": "cc14", "channel": 1, "number": 11}, "encoding": "absolute", "actionId": "mixer.eq_mid", "deck": "B"},
        {"id": "eq-low-a", "input": {"kind": "cc14", "channel": 0, "number": 15}, "encoding": "absolute", "actionId": "mixer.eq_low", "deck": "A"},
        {"id": "eq-low-b", "input": {"kind": "cc14", "channel": 1, "number": 15}, "encoding": "absolute", "actionId": "mixer.eq_low", "deck": "B"},
        {"id": "crossfader", "input": {"kind": "cc14", "channel": 6, "number": 31}, "encoding": "absolute", "actionId": "mixer.crossfader"},
        {"id": "loop-in-a", "input": {"kind": "note", "channel": 0, "number": 16}, "encoding": "button", "actionId": "loop.in", "deck": "A", "trigger": "press"},
        {"id": "loop-out-a", "input": {"kind": "note", "channel": 0, "number": 17}, "encoding": "button", "actionId": "loop.out", "deck": "A", "trigger": "press"},
        {"id": "loop-in-b", "input": {"kind": "note", "channel": 1, "number": 16}, "encoding": "button", "actionId": "loop.in", "deck": "B", "trigger": "press"},
        {"id": "loop-out-b", "input": {"kind": "note", "channel": 1, "number": 17}, "encoding": "button", "actionId": "loop.out", "deck": "B", "trigger": "press"},
    ],
    "feedback": [],
    "limitations": ["二次PADページとHIDディスプレイは未対応", "本体マイクはPC録音入力へ戻りません"],
}


class PresetService:
    def __init__(self, session: Session):
        self.session = session

    @staticmethod
    def validate_audio(config: dict[str, Any]) -> None:
        if config.get("sample_rate") not in {None, 44100} or config.get("buffer_size") not in {None, 256}:
            raise ValueError("現在の音声ホストで適用できるのは44,100 Hz / 256 framesのみです")
        master = config.get("master", {})
        channels = master.get("channels", [])
        cue = (config.get("cue") or {}).get("channels")
        for route, name in ((channels, "MASTER"), (cue, "CUE")):
            if route is None:
                continue
            if len(route) != 2 or any(isinstance(value, bool) or not isinstance(value, int) or value < 0 for value in route) or route[0] == route[1]:
                raise ValueError(f"{name}は異なる2つの有効なチャンネルで指定してください")
        if cue and set(channels) & set(cue):
            raise ValueError("MASTERとCUEのチャンネルは重複できません")

    @staticmethod
    def validate_profile(definition: dict[str, Any]) -> None:
        if definition.get("schemaVersion") != 1 or definition.get("adapterId") not in {"generic-midi", "ddj1000", "ddj400"}:
            raise ValueError("未対応のコントローラープロファイルです")
        bindings = definition.get("bindings")
        if not isinstance(bindings, list) or len(bindings) > 512:
            raise ValueError("MIDI割り当て数が不正です")
        ids: set[str] = set()
        inputs: set[tuple[str, int, int]] = set()
        for binding in bindings:
            if binding.get("id") in ids or binding.get("actionId") not in CONTROLLER_ACTIONS:
                raise ValueError("重複IDまたは未許可の操作があります")
            ids.add(binding["id"])
            source = binding.get("input", {})
            if source.get("kind") not in {"note", "cc", "cc14", "pitchbend"} or not 0 <= int(source.get("channel", -1)) <= 15:
                raise ValueError("MIDI入力の形式が不正です")
            number = int(source.get("number", -1))
            if not 0 <= number <= (31 if source.get("kind") == "cc14" else 127):
                raise ValueError("MIDI番号が範囲外です")
            input_key = (source["kind"], int(source["channel"]), number)
            if input_key in inputs:
                raise ValueError("同じMIDI入力が複数の操作へ割り当てられています")
            inputs.add(input_key)
            if binding.get("deck") not in {None, "A", "B", "C", "D"}:
                raise ValueError("デッキ指定が不正です")

    def audio_list(self) -> list[dict[str, Any]]:
        rows = _rows(self.session.exec(text("SELECT * FROM audio_presets ORDER BY updated_at DESC")))
        for row in rows:
            row["config"] = json.loads(row.pop("config_json"))
        return rows

    def save_audio(self, data: dict[str, Any]) -> dict[str, Any]:
        config = data.get("config", {})
        self.validate_audio(config)
        preset_id = data.get("id") or f"audio-{uuid.uuid4()}"
        current = self.session.exec(text("SELECT revision FROM audio_presets WHERE id=:id"), params={"id": preset_id}).first()
        if current and data.get("revision") != current[0]:
            raise ValueError("プリセットが別の画面で更新されました")
        self.session.exec(text("""
            INSERT INTO audio_presets (id,name,config_json) VALUES (:id,:name,:config)
            ON CONFLICT(id) DO UPDATE SET name=excluded.name,config_json=excluded.config_json,
              revision=audio_presets.revision+1,updated_at=now()
        """), params={"id": preset_id, "name": str(data.get("name") or "名称未設定")[:200], "config": _json(config)})
        self.session.commit()
        return next(row for row in self.audio_list() if row["id"] == preset_id)

    def delete_audio(self, preset_id: str) -> bool:
        deleted = self.session.exec(text("DELETE FROM audio_presets WHERE id=:id RETURNING id"), params={"id": preset_id}).first()
        self.session.commit()
        return bool(deleted)

    def controllers(self) -> list[dict[str, Any]]:
        rows = _rows(self.session.exec(text("SELECT * FROM controller_profiles ORDER BY built_in DESC,name")))
        definitions = [{**json.loads(row["definition_json"]), "revision": row["revision"]} for row in rows]
        if not any(item.get("id") == DDJ400_PROFILE["id"] for item in definitions):
            definitions.insert(0, DDJ400_PROFILE)
        return definitions

    def save_controller(self, definition: dict[str, Any]) -> dict[str, Any]:
        if len(_json(definition).encode()) > 512 * 1024:
            raise ValueError("プロファイルが大きすぎます")
        self.validate_profile(definition)
        profile_id = str(definition.get("id") or f"controller-{uuid.uuid4()}")
        if profile_id.startswith("builtin-"):
            profile_id = f"controller-{uuid.uuid4()}"
        definition = {**definition, "id": profile_id}
        current = self.session.exec(text("SELECT built_in,revision FROM controller_profiles WHERE id=:id"), params={"id": profile_id}).first()
        if current and current[0]:
            raise ValueError("組込プリセットは複製して編集してください")
        if current and definition.get("revision") != current[1]:
            raise ValueError("プロファイルが別の画面で更新されました")
        definition.pop("revision", None)
        self.session.exec(text("""
            INSERT INTO controller_profiles (id,name,adapter_id,definition_json)
            VALUES (:id,:name,:adapter,:definition)
            ON CONFLICT(id) DO UPDATE SET name=excluded.name,adapter_id=excluded.adapter_id,
              definition_json=excluded.definition_json,revision=controller_profiles.revision+1,
              updated_at=now()
        """), params={"id": profile_id, "name": str(definition.get("name") or "MIDI mapping")[:200],
                        "adapter": definition["adapterId"], "definition": _json(definition)})
        self.session.commit()
        saved = self.session.exec(text("SELECT revision FROM controller_profiles WHERE id=:id"), params={"id": profile_id}).one()
        return {**definition, "revision": int(saved[0])}


# A per-beat grid becomes tempo segments: a segment continues while each beat
# stays this close to its fitted period, so ms rounding jitter gives one marker.
TEMPO_SEGMENT_TOLERANCE_MS = 10.0
# Segments of a real track stay within this tempo ratio; a grid that does not
# (an irregular manual grid) is handed over as one constant marker instead.
TEMPO_SEGMENT_MAX_RATIO = 1.5


def _tempo_markers(grid: Any) -> list[tuple[float, float, int, int]]:
    """Rekordbox TEMPO markers (start ms, BPM, beat in bar, beats per bar) for a saved BeatGrid.

    A constant grid is one marker at its first non-negative beat. A per-beat
    grid gets one marker per tempo segment (see TEMPO_SEGMENT_TOLERANCE_MS).
    """
    if not isinstance(grid, dict):
        return []
    try:
        bpm, first = float(grid["bpm"]), float(grid["first_beat_ms"])
        bar = int(grid.get("beats_per_bar") or 4)
    except (KeyError, TypeError, ValueError):
        return []
    if not (math.isfinite(bpm) and bpm > 0 and math.isfinite(first) and 1 <= bar <= 16):
        return []
    times = grid.get("beat_times_ms") or []
    numbers = grid.get("beat_numbers") or []

    def number_at(index):
        return int(numbers[index]) if index < len(numbers) else index % bar + 1

    def constant():
        period = 60_000 / bpm
        skipped = max(0, math.ceil(-first / period))
        return [(first + skipped * period, bpm, (number_at(0) - 1 + skipped) % bar + 1, bar)]

    times = [float(value) for value in times[:100_000]]
    start = next((index for index, value in enumerate(times) if value >= 0), None)
    if len(times) < 2 or start is None or start == len(times) - 1:
        return constant()
    if any(not math.isfinite(b - a) or b <= a for a, b in zip(times, times[1:])):
        return constant()
    markers, end = [], start + 1
    while end < len(times):
        # Grow the segment while the next beat stays on the fitted period.
        while end + 1 < len(times):
            period = (times[end] - times[start]) / (end - start)
            predicted = times[start] + (end + 1 - start) * period
            if abs(times[end + 1] - predicted) > TEMPO_SEGMENT_TOLERANCE_MS:
                break
            end += 1
        period = (times[end] - times[start]) / (end - start)
        markers.append((times[start], 60_000 / period, number_at(start), bar))
        start, end = end, end + 1
    tempos = [marker[1] for marker in markers]
    if max(tempos) / min(tempos) > TEMPO_SEGMENT_MAX_RATIO:
        return constant()
    return markers


class UsbHandoffService:
    def __init__(self, session: Session):
        self.session = session

    def _snapshot(self, setlist_id: int) -> dict[str, Any]:
        setlist = self.session.exec(text("SELECT * FROM setlists WHERE id=:id"), params={"id": setlist_id}).first()
        if not setlist:
            raise ValueError("セットリストが見つかりません")
        entries = _rows(self.session.exec(text("""
            SELECT st.id AS entry_id,st.position,st.track_id,st.in_ms,st.out_ms,st.playback_rate,
              st.extra_duration_ms,st.overlap_next_ms,st.revision,t.filepath,t.title,t.artist,t.album,
              t.genre,t.bpm,t.duration,t.key,m.sha256,m.size_bytes,pm.cue_points_json,
              pm.loops_json,pm.beat_grid_json,pm.revision AS metadata_revision
            FROM setlist_tracks st JOIN tracks t ON t.id=st.track_id
            LEFT JOIN track_media m ON m.track_id=t.id
            LEFT JOIN track_performance_metadata pm ON pm.track_id=t.id
            WHERE st.setlist_id=:id ORDER BY st.position,st.id
        """), params={"id": setlist_id}))
        missing = [row["track_id"] for row in entries if not Path(row["filepath"]).is_file()]
        if missing:
            raise ValueError(f"音源が見つからない曲があります: {missing}")
        identities: dict[str, dict[str, Any]] = {}
        for row in entries:
            if row["filepath"] not in identities:
                identities[row["filepath"]] = _file_identity(Path(row["filepath"]))
            identity = identities[row["filepath"]]
            row["snapshot_sha256"] = identity["sha256"]
            row["snapshot_size_bytes"] = identity["size_bytes"]
        timing = calculate_set_duration(entries)
        snapshot = {"schema_version": 1, "serializer_version": 1,
                    "setlist": dict(setlist._mapping), "entries": entries, "timing": timing}
        snapshot["hash"] = hashlib.sha256(_json(snapshot).encode()).hexdigest()
        return snapshot

    @staticmethod
    def _location(path: str) -> str:
        return Path(path).resolve().as_uri()

    def _xml(self, snapshot: dict[str, Any]) -> tuple[bytes, list[str]]:
        root = ET.Element("DJ_PLAYLISTS", Version="1.0.0")
        ET.SubElement(root, "PRODUCT", Name="plumdeck", Version="1", Company="plumdeck")
        collection = ET.SubElement(root, "COLLECTION", Entries=str(len({row['track_id'] for row in snapshot['entries']})))
        limitations: list[str] = []
        by_track: dict[int, int] = {}
        for index, row in enumerate(snapshot["entries"], 1):
            if row["track_id"] in by_track:
                continue
            by_track[row["track_id"]] = index
            attrs = {"TrackID": str(index), "Name": row["title"] or "", "Artist": row["artist"] or "",
                     "Location": self._location(row["filepath"])}
            if row.get("album"):
                attrs["Album"] = str(row["album"])
            if row.get("genre"):
                attrs["Genre"] = str(row["genre"])
            if row.get("key"):
                attrs["Tonality"] = str(row["key"])
            if row.get("bpm") is not None and math.isfinite(float(row["bpm"])) and float(row["bpm"]) > 0:
                attrs["AverageBpm"] = f"{float(row['bpm']):.2f}"
            if row.get("duration") is not None:
                attrs["TotalTime"] = str(max(0, round(float(row["duration"]))))
            track = ET.SubElement(collection, "TRACK", attrs)
            cues = json.loads(row.get("cue_points_json") or "[]")
            for cue in cues:
                slot = int(cue.get("slot", -1))
                if slot > 7:
                    limitations.append(f"{row['title']}: Hot Cue {slot + 1} はXML転送対象外")
                    continue
                position_ms = cue.get("position_ms")
                if slot < 0 or position_ms is None or not math.isfinite(float(position_ms)) or float(position_ms) < 0:
                    limitations.append(f"{row['title']}: 不正なCueを除外")
                    continue
                ET.SubElement(track, "POSITION_MARK", Name=str(cue.get("label") or f"Hot Cue {chr(65 + slot)}"),
                              Type="0", Start=f"{float(position_ms) / 1000:.6f}", Num=str(slot))
            for loop in json.loads(row.get("loops_json") or "[]"):
                start, end = loop.get("start_ms"), loop.get("end_ms")
                if start is None or end is None or float(end) <= float(start):
                    limitations.append(f"{row['title']}: 不正なLoopを除外")
                    continue
                ET.SubElement(track, "POSITION_MARK", Name=str(loop.get("label") or "Memory Loop"), Type="4",
                              Start=f"{float(start) / 1000:.6f}", End=f"{float(end) / 1000:.6f}", Num="-1")
            markers = _tempo_markers(json.loads(row.get("beat_grid_json") or "null"))
            if markers:
                for time_ms, bpm, number, bar in markers:
                    ET.SubElement(track, "TEMPO", Inizio=f"{time_ms / 1000:.6f}", Bpm=f"{bpm:.2f}",
                                  Metro=f"{bar}/4", Battito=str(number))
            elif row.get("bpm"):
                ET.SubElement(track, "TEMPO", Inizio="0.000000", Bpm=f"{float(row['bpm']):.2f}", Metro="4/4", Battito="1")
        playlists = ET.SubElement(root, "PLAYLISTS")
        folder = ET.SubElement(playlists, "NODE", Type="0", Name="ROOT", Count="1")
        playlist = ET.SubElement(folder, "NODE", Type="1", Name=str(snapshot["setlist"]["name"]), KeyType="0", Entries=str(len(snapshot["entries"])))
        for row in snapshot["entries"]:
            ET.SubElement(playlist, "TRACK", Key=str(by_track[row["track_id"]]))
        ET.indent(root)
        return ET.tostring(root, encoding="utf-8", xml_declaration=True), limitations

    def _persist_handoff(self, snapshot: dict[str, Any], usb_device_id: str | None = None) -> dict[str, Any]:
        if usb_device_id:
            device = self.session.exec(text("SELECT connected FROM usb_devices WHERE id=:id"), params={"id": usb_device_id}).first()
            if not device or not device[0]:
                raise ValueError("選択したUSB媒体が接続されていません")
        xml, limitations = self._xml(snapshot)
        export_id = f"usb-{uuid.uuid4()}"
        root = Path(settings.USER_DATA_DIR) / "usb-handoffs" / export_id
        root.mkdir(parents=True, exist_ok=False)
        xml_path = root / "plumdeck-rekordbox.xml"
        manifest_path = root / "manifest.json"
        xml_path.write_bytes(xml)
        snapshot["limitations"] = limitations
        manifest_path.write_text(json.dumps(snapshot, ensure_ascii=False, indent=2, default=str), encoding="utf-8")
        self.session.exec(text("""
            INSERT INTO usb_exports (id,usb_device_id,setlist_id,state,snapshot_hash,snapshot_json,handoff_path)
            VALUES (:id,:usb,:setlist,'awaiting_rekordbox',:hash,:snapshot,:path)
        """), params={"id": export_id, "usb": usb_device_id, "setlist": int(snapshot["setlist"]["id"]),
                        "hash": snapshot["hash"], "snapshot": _json(snapshot), "path": str(root)})
        _operation(self.session, export_id, "usb_handoff", str(snapshot["setlist"]["id"]), "awaiting_rekordbox", {"path": str(root)}, 1)
        self.session.commit()
        return {"id": export_id, "state": "awaiting_rekordbox", "handoff_path": str(root),
                "xml_path": str(xml_path), "snapshot_hash": snapshot["hash"], "limitations": limitations,
                "instructions": ["rekordboxでXMLの参照先を選択", "プレイリストをコレクションへ取り込み", "rekordboxから対象USBへExport", "書き出し完了後にplumdeckでUSBを確認"]}

    def create(self, setlist_id: int, usb_device_id: str | None = None) -> dict[str, Any]:
        return self._persist_handoff(self._snapshot(setlist_id), usb_device_id)

    def duplicate(self, export_id: str, usb_device_id: str | None = None) -> dict[str, Any]:
        row = self.session.exec(text("SELECT snapshot_json FROM usb_exports WHERE id=:id"), params={"id": export_id}).first()
        if not row:
            raise ValueError("元のUSB受け渡しが見つかりません")
        snapshot = json.loads(row[0])
        # Preserve the exact original selection and revisions; verification is
        # intentionally new for the spare medium.
        return self._persist_handoff(snapshot, usb_device_id)

    def list(self) -> list[dict[str, Any]]:
        rows = _rows(self.session.exec(text("SELECT id,usb_device_id,setlist_id,state,snapshot_hash,handoff_path,verification_json,created_at,updated_at FROM usb_exports ORDER BY created_at DESC")))
        for row in rows:
            try:
                current_hash = self._snapshot(int(row["setlist_id"]))["hash"]
            except ValueError:
                current_hash = None
            row["current_snapshot_hash"] = current_hash
            row["stale"] = current_hash != row["snapshot_hash"]
            if row["stale"]:
                row["state"] = "stale"
        return rows

    def devices(self) -> list[dict[str, Any]]:
        rows = removable_devices.devices()
        for row in rows:
            self.session.exec(text("""
                INSERT INTO usb_devices (id,device_identifier,volume_uuid,label,mount_path,filesystem,
                  capacity_bytes,free_bytes,read_only,connected,last_seen_at)
                VALUES (:id,:device_identifier,:volume_uuid,:label,:mount_path,:filesystem,
                  :capacity_bytes,:free_bytes,:read_only,true,now())
                ON CONFLICT(id) DO UPDATE SET label=excluded.label,mount_path=excluded.mount_path,
                  filesystem=excluded.filesystem,capacity_bytes=excluded.capacity_bytes,
                  free_bytes=excluded.free_bytes,read_only=excluded.read_only,connected=true,last_seen_at=now()
            """), params=row)
        connected_ids = {row["id"] for row in rows}
        if connected_ids:
            placeholders = ",".join(f":id{index}" for index in range(len(connected_ids)))
            self.session.exec(text(f"UPDATE usb_devices SET connected=false WHERE id NOT IN ({placeholders})"),
                              params={f"id{index}": value for index, value in enumerate(connected_ids)})
        else:
            self.session.exec(text("UPDATE usb_devices SET connected=false"))
        self.session.commit()
        return rows

    def eject(self, device_id: str) -> dict[str, Any]:
        current = next((row for row in self.devices() if row["id"] == device_id), None)
        if not current:
            raise ValueError("接続中のUSBが見つかりません")
        try:
            detail = removable_devices.eject(current)
        except (OSError, subprocess.SubprocessError) as exc:
            raise ValueError(f"USBを取り外せませんでした: {exc}") from exc
        self.session.exec(text("UPDATE usb_devices SET connected=false,mount_path=NULL WHERE id=:id"), params={"id": device_id})
        self.session.commit()
        return {"id": device_id, "ejected": True, "detail": detail}

    def mark_checked(self, export_id: str, evidence: dict[str, Any]) -> dict[str, Any]:
        row = self.session.exec(text("SELECT * FROM usb_exports WHERE id=:id"), params={"id": export_id}).first()
        if not row:
            raise ValueError("USB受け渡しが見つかりません")
        current_hash = self._snapshot(int(row._mapping["setlist_id"]))["hash"]
        if current_hash != row._mapping["snapshot_hash"]:
            raise ValueError("セットリストまたは音源が変更されています。新しいUSB受け渡しを作成してください")
        level = evidence.get("level")
        if level not in {"user_rekordbox_check", "user_hardware_check"}:
            raise ValueError("確認レベルが不正です")
        if level == "user_hardware_check" and not all(evidence.get(key) for key in ("model", "firmware", "checked_at", "checks")):
            raise ValueError("実機確認には機種・firmware・日時・確認項目が必要です")
        verification = {**evidence, "snapshot_hash": row._mapping["snapshot_hash"]}
        state = "verified" if level == "user_hardware_check" else "needs_user_check"
        self.session.exec(text("UPDATE usb_exports SET state=:state,verification_json=:verification,updated_at=now() WHERE id=:id"),
                          params={"id": export_id, "state": state, "verification": _json(verification)})
        self.session.commit()
        return {"id": export_id, "state": state, "verification": verification}


class RecordingTimelineService:
    def __init__(self, session: Session):
        self.session = session

    def list(self, recording_id: int) -> list[dict[str, Any]]:
        return _rows(self.session.exec(text("SELECT * FROM recording_segments WHERE recording_id=:id ORDER BY start_ms,position,id"), params={"id": recording_id}))

    def replace_manual(self, recording_id: int, revision: int, segments: list[dict[str, Any]]) -> list[dict[str, Any]]:
        recording = self.session.exec(text("SELECT * FROM recordings WHERE id=:id"), params={"id": recording_id}).first()
        if not recording:
            raise ValueError("録音が見つかりません")
        if int(recording._mapping.get("revision") or 1) != revision:
            raise ValueError("録音が別の画面で更新されました")
        duration_ms = int(recording._mapping["duration_ms"] or 0)
        checked: list[dict[str, Any]] = []
        for index, segment in enumerate(segments):
            start = int(segment.get("start_ms", -1))
            end = segment.get("end_ms")
            end = int(end) if end is not None else None
            if start < 0 or start > duration_ms or end is not None and (end <= start or end > duration_ms):
                raise ValueError("曲目時刻が録音範囲外です")
            track = self.session.get(Track, segment.get("track_id")) if segment.get("track_id") else None
            checked.append({**segment, "position": index, "start_ms": start, "end_ms": end,
                            "title_snapshot": segment.get("title_snapshot") or (track.title if track else "曲名未設定"),
                            "artist_snapshot": segment.get("artist_snapshot") or (track.artist if track else ""),
                            "event_key": segment.get("event_key") or f"manual:{recording_id}:{uuid.uuid4()}"})
        self.session.exec(text("DELETE FROM recording_segments WHERE recording_id=:id AND source='manual'"), params={"id": recording_id})
        for segment in checked:
            self.session.exec(text("""
                INSERT INTO recording_segments (recording_id,event_key,track_id,deck,start_ms,end_ms,title_snapshot,
                  artist_snapshot,version_snapshot,source,confidence,position)
                VALUES (:recording_id,:event_key,:track_id,:deck,:start_ms,:end_ms,:title_snapshot,
                  :artist_snapshot,:version_snapshot,'manual','confirmed',:position)
            """), params={"recording_id": recording_id, "deck": segment.get("deck"),
                            "track_id": segment.get("track_id"), "version_snapshot": segment.get("version_snapshot"), **{key: segment[key] for key in ("event_key", "start_ms", "end_ms", "title_snapshot", "artist_snapshot", "position")}})
        self.session.exec(text("UPDATE recordings SET revision=revision+1 WHERE id=:id"), params={"id": recording_id})
        self.session.commit()
        return self.list(recording_id)

    def text(self, recording_id: int) -> str:
        lines = []
        for row in self.list(recording_id):
            seconds = int(row["start_ms"] // 1000)
            stamp = f"{seconds // 3600:02d}:{seconds // 60 % 60:02d}:{seconds % 60:02d}"
            version = f" [{row['version_snapshot']}]" if row.get("version_snapshot") else ""
            lines.append(f"{stamp} {row['artist_snapshot']} - {row['title_snapshot']}{version}".strip())
        return "\n".join(lines)

    def upsert_engine(self, recording_id: int, sample_rate_hz: int, frame_count: int,
                      dropped_events: int, segments: list[dict[str, Any]]) -> list[dict[str, Any]]:
        recording = self.session.exec(text("SELECT id FROM recordings WHERE id=:id"), params={"id": recording_id}).first()
        if not recording:
            raise ValueError("録音が見つかりません")
        if sample_rate_hz < 8000 or sample_rate_hz > 384000 or frame_count < 0:
            raise ValueError("録音フレーム情報が不正です")
        duration_ms = frame_count * 1000 // sample_rate_hz
        for position, segment in enumerate(segments[:10_000]):
            start_frame = int(segment.get("startFrame", -1))
            raw_end = segment.get("endFrame")
            end_frame = frame_count if raw_end is None else min(int(raw_end), frame_count)
            if start_frame < 0 or end_frame <= start_frame or start_frame > frame_count:
                continue
            raw_track_id = segment.get("trackId")
            track_id = int(raw_track_id) if str(raw_track_id).isdigit() else None
            self.session.exec(text("""
                INSERT INTO recording_segments (recording_id,event_key,track_id,deck,load_generation,
                  start_frame,end_frame,start_ms,end_ms,title_snapshot,artist_snapshot,source,confidence,position)
                VALUES (:recording,:event,:track,:deck,:generation,:start_frame,:end_frame,:start_ms,:end_ms,
                  :title,:artist,'engine_observed',:confidence,:position)
                ON CONFLICT(event_key) DO UPDATE SET end_frame=excluded.end_frame,end_ms=excluded.end_ms,
                  confidence=excluded.confidence,revision=recording_segments.revision+1
            """), params={"recording": recording_id, "event": f"engine:{recording_id}:" + str(segment.get("eventKey") or position),
                            "track": track_id, "deck": segment.get("deck"), "generation": segment.get("loadGeneration"),
                            "start_frame": start_frame, "end_frame": end_frame,
                            "start_ms": start_frame * 1000 // sample_rate_hz,
                            "end_ms": end_frame * 1000 // sample_rate_hz,
                            "title": str(segment.get("title") or "曲名未設定"),
                            "artist": str(segment.get("artist") or ""),
                            "confidence": "incomplete" if dropped_events else "estimated", "position": position})
        self.session.exec(text("""
            UPDATE recordings SET sample_rate_hz=:rate,frame_count=:frames,
              timeline_quality=:quality,timeline_dropped_events=:dropped,
              duration_ms=:duration,revision=revision+1 WHERE id=:id
        """), params={"rate": sample_rate_hz, "frames": frame_count, "dropped": dropped_events,
                        "quality": "incomplete" if dropped_events else "engine_sampled",
                        "duration": duration_ms, "id": recording_id})
        self.session.commit()
        return self.list(recording_id)


def create_backup(session: Session, destination: str, include_media: bool,
                  include_recordings: bool, ui_settings: dict[str, Any]) -> dict[str, Any]:
    target = Path(destination).expanduser()
    if not target.is_absolute():
        raise ValueError("バックアップ先は絶対パスで指定してください")
    if target.suffix != ".plumdeck-backup":
        target = target.with_suffix(".plumdeck-backup")
    target.parent.mkdir(parents=True, exist_ok=True)
    operation_id = f"backup-{uuid.uuid4()}"
    allowed_settings = {key: value for key, value in ui_settings.items() if _allowed_ui_setting(key, value)}
    stage = Path(tempfile.mkdtemp(prefix="plumdeck-backup-", dir=target.parent))
    archive_tmp = target.with_name(f".{target.name}.{uuid.uuid4().hex}.partial")
    files: list[dict[str, Any]] = []
    missing: list[str] = []
    try:
        session.commit()
        with WORKFLOW_LOCK, db_connection.background_analysis_paused(), db_connection.database_activity, db_connection.db_lock, db_connection.exclusive_database():
            session.exec(text("CHECKPOINT"))
            db_snapshot = stage / "plumdeck.duckdb"
            # DuckDB owns an exclusive file handle on Windows. Export through
            # the existing database connection while the maintenance gate is
            # held, preserving schema/data/sequences without reopening the file.
            database = session.exec(text("SELECT current_database()")).one()[0].replace('"', '""')
            quoted_snapshot = str(db_snapshot).replace("'", "''")
            session.exec(text(f"ATTACH '{quoted_snapshot}' AS plumdeck_backup_snapshot"))
            try:
                session.exec(text(f'COPY FROM DATABASE "{database}" TO plumdeck_backup_snapshot'))
                session.commit()
                session.exec(text("CHECKPOINT plumdeck_backup_snapshot"))
                session.commit()
            finally:
                session.rollback()
                session.exec(text("DETACH plumdeck_backup_snapshot"))
                session.commit()
            analysis_path = Path(str(db_connection.DB_PATH) + ".analysis-jobs.sqlite3")
            if analysis_path.is_file():
                _snapshot_sqlite(analysis_path, stage / "analysis-jobs.sqlite3")
            table_counts = {}
            for table in ALL_TABLES:
                table_counts[table] = int(session.exec(text(f'SELECT count(*) FROM "{table}"')).one()[0])
        import duckdb
        with duckdb.connect(str(db_snapshot), read_only=True) as snapshot:
            assets: list[tuple[str, str]] = []
            expected_hashes = dict(snapshot.execute("SELECT t.filepath,m.sha256 FROM tracks t JOIN track_media m ON m.track_id=t.id WHERE m.sha256 IS NOT NULL").fetchall())
            if include_media:
                assets.extend((row[0], "media") for row in snapshot.execute("SELECT filepath FROM tracks ORDER BY id").fetchall())
                try:
                    sampler_paths = json.loads(allowed_settings.get("plumdeck.sampler.paths", "[]"))
                except (TypeError, json.JSONDecodeError):
                    sampler_paths = []
                if isinstance(sampler_paths, list):
                    assets.extend((path, "sampler") for path in sampler_paths if isinstance(path, str) and path)
            if include_recordings:
                assets.extend((row[0], "recording") for row in snapshot.execute("SELECT filepath FROM recordings WHERE status='completed' ORDER BY id").fetchall())
        seen: dict[str, str] = {}
        for original, category in assets:
            source = Path(original)
            if not source.is_file():
                missing.append(original)
                continue
            identity = _file_identity(source)
            if category == "media" and expected_hashes.get(original) not in (None, identity["sha256"]):
                raise ValueError("保存済みの解析対象と音源内容が異なります。再解析してからバックアップしてください")
            relative = seen.get(identity["sha256"])
            if relative is None:
                relative = f"assets/content/{identity['sha256']}{source.suffix.lower()}"
                destination_path = stage / relative
                destination_path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, destination_path)
                if _sha256(destination_path) != identity["sha256"]:
                    raise ValueError("収集中に音源内容が変化しました")
                seen[identity["sha256"]] = relative
            files.append({**identity, "path": relative, "category": category, "source_path": original})
        (stage / "ui-settings.json").write_text(json.dumps(allowed_settings, ensure_ascii=False), encoding="utf-8")
        base_files = [(stage / "plumdeck.duckdb", "database"), (stage / "ui-settings.json", "settings")]
        if (stage / "analysis-jobs.sqlite3").exists():
            base_files.append((stage / "analysis-jobs.sqlite3", "jobs"))
        for path, category in base_files:
            files.append({"path": path.relative_to(stage).as_posix(), "category": category,
                          "size_bytes": path.stat().st_size, "sha256": _sha256(path)})
        manifest = {"format": "plumdeck-backup", "format_version": BACKUP_FORMAT_VERSION,
                    "app_version": _app_version(),
                    "created_at": datetime.now(timezone.utc).isoformat(), "schema_version": CURRENT_SCHEMA_VERSION,
                    "snapshot_id": operation_id,
                    "library_identity": hashlib.sha256(str(Path(db_connection.DB_PATH).resolve()).encode()).hexdigest(),
                    "table_counts": table_counts, "files": files,
                    "included": {"media": include_media, "recordings": include_recordings, "ui_settings": True},
                    "excluded": ["credentials", "keychain", "os_permissions", "live_playback", "live_recording_handles", "rebuildable_waveform_cache"],
                    "missing_assets": missing}
        (stage / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
        with zipfile.ZipFile(archive_tmp, "w", allowZip64=True) as archive:
            for path in stage.rglob("*"):
                if path.is_file():
                    archive.write(path, path.relative_to(stage).as_posix(),
                                  compress_type=zipfile.ZIP_STORED if path.suffix.lower() in {".mp3", ".flac", ".ogg", ".zip"} else zipfile.ZIP_DEFLATED)
        inspect_backup(str(archive_tmp))
        os.replace(archive_tmp, target)
        return {"id": operation_id, "path": str(target), "missing_assets": missing,
                "file_count": len(files), "table_counts": table_counts}
    finally:
        shutil.rmtree(stage, ignore_errors=True)
        if archive_tmp.exists():
            archive_tmp.unlink()


def _snapshot_sqlite(source: Path, destination: Path) -> None:
    # A raw copy loses committed pages that still live in a WAL. backup()
    # produces a standalone, transactionally consistent database.
    from contextlib import closing
    with closing(sqlite3.connect(source)) as live, closing(sqlite3.connect(destination)) as saved:
        live.backup(saved)
        if saved.execute("PRAGMA quick_check").fetchone()[0] != "ok":
            raise ValueError("解析ジョブDBの検査に失敗しました")


def _validated_archive(path: str) -> tuple[zipfile.ZipFile, dict[str, Any]]:
    archive = zipfile.ZipFile(_safe_real_file(path), "r")
    try:
        infos = archive.infolist()
        if len(infos) > MAX_BACKUP_ENTRIES or sum(info.file_size for info in infos) > MAX_BACKUP_UNCOMPRESSED:
            raise ValueError("バックアップの展開サイズが上限を超えています")
        names: set[str] = set()
        for info in infos:
            posix = Path(info.filename)
            if info.filename in names or posix.is_absolute() or ".." in posix.parts or "\\" in info.filename or info.is_dir():
                raise ValueError("安全でないアーカイブパスです")
            names.add(info.filename)
        if not {"manifest.json", "plumdeck.duckdb", "ui-settings.json"} <= names:
            raise ValueError("必須データがありません")
        if archive.getinfo("manifest.json").file_size > 32 * 1024 * 1024:
            raise ValueError("manifestが大きすぎます")
        manifest = json.loads(archive.read("manifest.json"))
        if manifest.get("format") != "plumdeck-backup" or manifest.get("format_version") != BACKUP_FORMAT_VERSION:
            raise ValueError("未対応のバックアップ形式です")
        snapshot_id = manifest.get("snapshot_id")
        if not isinstance(snapshot_id, str) or not snapshot_id or len(snapshot_id) > 100 or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_" for c in snapshot_id):
            raise ValueError("snapshot IDが不正です")
        if int(manifest.get("schema_version", 0)) > CURRENT_SCHEMA_VERSION:
            raise ValueError("新しいバージョンのバックアップは復元できません")
        checked: dict[str, tuple[str, int]] = {}
        for item in manifest.get("files", []):
            name = item.get("path")
            if name not in names or name == "manifest.json":
                raise ValueError("manifestに記載されたファイルがありません")
            identity = (item.get("sha256"), item.get("size_bytes"))
            if name in checked:
                if checked[name] != identity:
                    raise ValueError("重複したファイル情報が一致しません")
                continue
            digest = hashlib.sha256()
            with archive.open(name) as source:
                while chunk := source.read(1024 * 1024):
                    digest.update(chunk)
            if digest.hexdigest() != identity[0] or archive.getinfo(name).file_size != identity[1]:
                raise ValueError("バックアップのハッシュまたはサイズが一致しません")
            checked[name] = identity
        if set(checked) != names - {"manifest.json"}:
            raise ValueError("manifestに未検証のファイルがあります")
        return archive, manifest
    except Exception:
        archive.close()
        raise


def inspect_backup(path: str) -> dict[str, Any]:
    archive, manifest = _validated_archive(path)
    archive.close()
    return manifest


def restore_backup(path: str, confirmed: bool) -> dict[str, Any]:
    if not confirmed:
        raise ValueError("現在のアプリデータを置き換える確認が必要です")
    archive, manifest = _validated_archive(path)
    db_path = Path(db_connection.DB_PATH)
    restore_root = db_path.parent / "restore-staging" / manifest["snapshot_id"]
    rollback = db_path.with_name(f"{db_path.name}.pre-restore-{uuid.uuid4().hex}")
    restore_root.mkdir(parents=True, exist_ok=False)
    try:
        for info in archive.infolist():
            destination = restore_root / info.filename
            destination.parent.mkdir(parents=True, exist_ok=True)
            if info.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                with archive.open(info) as source, destination.open("wb") as target:
                    shutil.copyfileobj(source, target, 1024 * 1024)
        staged_db = restore_root / "plumdeck.duckdb"
        # Validate/migrate the staging copy without mutating the archive.
        from infra.database.compaction import ensure_healthy_db
        ensure_healthy_db(str(staged_db), keep_backup=False)
        # Restore included files under a content-addressed managed root and
        # rewrite only the matching archived references in the staging DB.
        managed_root = db_path.parent / "restored-assets" / manifest["snapshot_id"]
        import duckdb
        staged = duckdb.connect(str(staged_db))
        sampler_mapping: dict[str, str] = {}
        try:
            for item in manifest.get("files", []):
                if item.get("category") not in {"media", "recording", "sampler"}:
                    continue
                source = restore_root / item["path"]
                target = managed_root / item["path"]
                target.parent.mkdir(parents=True, exist_ok=True)
                if target.exists() and _sha256(target) != item["sha256"]:
                    raise ValueError("復元先に同名で内容の異なるファイルがあります")
                if not target.exists():
                    shutil.copy2(source, target)
                if item["category"] == "sampler":
                    sampler_mapping[str(item.get("source_path"))] = str(target)
                else:
                    table = "tracks" if item["category"] == "media" else "recordings"
                    staged.execute(f"UPDATE {table} SET filepath=? WHERE filepath=?", [str(target), item.get("source_path")])
            staged.execute("UPDATE import_items SET state='queued' WHERE state IN ('probing','analyzing')")
            staged.execute("UPDATE import_batches SET state='paused',paused=true WHERE state IN ('queued','processing','pausing')")
            staged.execute("UPDATE operation_journal SET state='paused',updated_at=now() WHERE state IN ('queued','running','processing')")
            staged.execute("CHECKPOINT")
        finally:
            staged.close()
        jobs = restore_root / "analysis-jobs.sqlite3"
        if jobs.exists():
            import sqlite3
            with closing(sqlite3.connect(jobs)) as queue, queue:
                queue.execute("UPDATE jobs SET status=CASE WHEN json_extract(config, '$.background') "
                              "THEN 'queued' ELSE 'paused' END WHERE status IN ('running','pausing')")
                queue.execute("UPDATE items SET status='pending' WHERE status='running'")
        queue_path = Path(str(db_path) + ".analysis-jobs.sqlite3")
        queue_rollback = Path(str(rollback) + ".analysis-jobs.sqlite3")
        with WORKFLOW_LOCK, db_connection.background_analysis_paused(), db_connection.database_activity, db_connection.db_lock, db_connection.exclusive_database():
            db_connection.checkpoint_db()
            db_connection.close_db()
            queue_existed = queue_path.exists()
            if db_path.exists():
                shutil.copy2(db_path, rollback)
            if queue_existed:
                _snapshot_sqlite(queue_path, queue_rollback)
            replacement = db_path.with_name(f".{db_path.name}.restore")
            shutil.copy2(staged_db, replacement)
            from infra.database.restore_recovery import begin_restore, finish_restore, sync_restored_files
            begin_restore(db_path, rollback, queue_existed)
            try:
                os.replace(replacement, db_path)
                # Both databases form one restore unit, including an absent queue.
                for suffix in ("-wal", "-shm"):
                    Path(str(queue_path) + suffix).unlink(missing_ok=True)
                queue_path.unlink(missing_ok=True)
                if jobs.exists():
                    _snapshot_sqlite(jobs, queue_path)
                sync_restored_files(db_path)
                db_connection.reopen_db(str(db_path))
                finish_restore(db_path)
            except Exception:
                db_connection.close_db()
                if rollback.exists():
                    shutil.copy2(rollback, db_path)
                for suffix in ("", "-wal", "-shm"):
                    Path(str(queue_path) + suffix).unlink(missing_ok=True)
                if queue_existed:
                    _snapshot_sqlite(queue_rollback, queue_path)
                sync_restored_files(db_path)
                db_connection.reopen_db(str(db_path))
                finish_restore(db_path)
                raise
        restored_settings = json.loads((restore_root / "ui-settings.json").read_text(encoding="utf-8")) if (restore_root / "ui-settings.json").exists() else {}
        if sampler_mapping and "plumdeck.sampler.paths" in restored_settings:
            try:
                sampler_paths = json.loads(restored_settings["plumdeck.sampler.paths"])
                restored_settings["plumdeck.sampler.paths"] = json.dumps(
                    [sampler_mapping.get(path, path) for path in sampler_paths], ensure_ascii=False,
                )
            except (TypeError, json.JSONDecodeError):
                pass
        return {"restored": True, "snapshot_id": manifest["snapshot_id"], "rollback_path": str(rollback),
                "ui_settings": restored_settings}
    finally:
        archive.close()
        shutil.rmtree(restore_root, ignore_errors=True)
