from __future__ import annotations

import asyncio
import hashlib
import os
import sys
import uuid
from domain.services.analysis.process_runner import AnalysisExecutor
from domain.services.analysis.light_dsp import WORKER_TIMEOUT as LIGHT_WORKER_TIMEOUT
from pathlib import Path
from typing import Any

from sqlalchemy import text
from sqlmodel import Session, select

from domain.constants import SUPPORTED_EXTENSIONS
from domain.models.track import Track, TrackEmbedding, TrackAnalysis
from utils.ingestion import (
    has_completed_analysis_for_profile,
    has_completed_analysis_result,
)
from app.services.analysis_coordinator import analysis_coordinator
from app.services.analysis_progress import analysis_progress
from infra.repositories.ingestion_repository import IngestionRepository
from infra.repositories.setlist_repository import SetlistRepository
import infra.database.connection as db_connection


def _rows(result) -> list[dict[str, Any]]:
    return [dict(row._mapping) for row in result]


def _sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _exception_chain(exc: BaseException):
    """Yield wrapped errors without trusting translated message text alone."""
    seen: set[int] = set()
    current: BaseException | None = exc
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        yield current
        current = current.__cause__ or current.__context__


def _expand(paths: list[str]) -> list[Path]:
    output: list[Path] = []
    seen: set[str] = set()
    for raw in paths:
        candidate = Path(raw).expanduser()
        if not candidate.is_absolute():
            continue
        try:
            resolved = candidate.resolve(strict=True)
        except (OSError, RuntimeError):
            continue
        found: list[Path]
        if resolved.is_file():
            found = [resolved]
        elif resolved.is_dir():
            found = sorted(
                (item for item in resolved.rglob("*") if item.is_file() and not item.is_symlink()),
                key=lambda item: item.relative_to(resolved).as_posix(),
            )
        else:
            found = []
        for item in found:
            if item.suffix.lower() not in SUPPORTED_EXTENSIONS:
                continue
            real = str(item.resolve())
            if real not in seen:
                seen.add(real)
                output.append(item.resolve())
    return output


class PlayImportService:
    def __init__(self, session: Session):
        self.session = session

    def create(self, request_id: str, target_kind: str, target_id: int | None,
               paths: list[str], origin: str = "native_file_drop",
               analysis_profile: str = "auto") -> dict[str, Any]:
        duplicate = self.session.exec(text("SELECT id FROM import_batches WHERE request_id=:id"), params={"id": request_id}).first()
        if duplicate:
            return self.get(str(duplicate[0]))
        if target_kind not in {"collection", "local_playlist"}:
            raise ValueError("この場所には音源を取り込めません")
        if analysis_profile not in {"auto", "light", "full"}:
            raise ValueError("解析方法が不正です")
        target_name = "Collection"
        if target_kind == "local_playlist":
            target = self.session.exec(text("SELECT name FROM setlists WHERE id=:id"), params={"id": target_id}).first()
            if not target:
                raise ValueError("追加先プレイリストが見つかりません")
            target_name = str(target[0])
        expanded = _expand(paths)
        if not expanded:
            raise ValueError("対応する読み取り可能な音源がありません")
        batch_id = f"import-{uuid.uuid4()}"
        self.session.exec(text("""
            INSERT INTO import_batches
              (id,request_id,target_kind,target_id,target_name_snapshot,origin,analysis_profile,state)
            VALUES (:id,:request,:kind,:target,:name,:origin,:profile,'queued')
        """), params={"id": batch_id, "request": request_id, "kind": target_kind,
                        "target": target_id, "name": target_name, "origin": origin,
                        "profile": analysis_profile})
        for index, path in enumerate(expanded):
            item_id = f"item-{uuid.uuid4()}"
            stat = path.stat()
            self.session.exec(text("""
                INSERT INTO import_items (id,batch_id,input_order,source_path,canonical_path,size_bytes,mtime_ns,state)
                VALUES (:id,:batch,:ordering,:source,:canonical,:size,:mtime,'queued')
            """), params={"id": item_id, "batch": batch_id, "ordering": index,
                            "source": str(path), "canonical": str(path), "size": stat.st_size, "mtime": stat.st_mtime_ns})
            self.session.exec(text("""
                INSERT INTO import_target_intents (item_id,batch_id,target_kind,target_id,reserved_position,state)
                VALUES (:item,:batch,:kind,:target,:position,'pending')
            """), params={"item": item_id, "batch": batch_id, "kind": target_kind,
                            "target": target_id, "position": index})
        self.session.commit()
        return self.get(batch_id)

    def get(self, batch_id: str) -> dict[str, Any]:
        batch = self.session.exec(text("SELECT * FROM import_batches WHERE id=:id"), params={"id": batch_id}).first()
        if not batch:
            raise ValueError("取込ジョブが見つかりません")
        result = dict(batch._mapping)
        result["progress"] = analysis_progress.get(batch_id)
        result["items"] = _rows(self.session.exec(text("""
            SELECT i.*,intent.state AS membership_state,intent.setlist_track_id
            FROM import_items i JOIN import_target_intents intent ON intent.item_id=i.id
            WHERE i.batch_id=:id ORDER BY i.input_order
        """), params={"id": batch_id}))
        result["summary"] = {state: sum(item["state"] == state for item in result["items"])
                             for state in {item["state"] for item in result["items"]}}
        return result

    def list(self, active: bool = False) -> list[dict[str, Any]]:
        where = "WHERE state NOT IN ('completed','completed_with_errors','canceled')" if active else ""
        qualified_where = where.replace("state ", "b.state ")
        rows = _rows(self.session.exec(text(f"""
            SELECT b.*,
              count(i.id) AS total_items,
              count(i.id) FILTER (WHERE i.state IN ('completed','existing')) AS succeeded_items,
              count(i.id) FILTER (WHERE i.state='failed') AS failed_items,
              count(i.id) FILTER (WHERE i.state='skipped') AS skipped_items,
              count(i.id) FILTER (WHERE i.state='queued') AS queued_items,
              max(i.canonical_path) FILTER (WHERE i.state IN ('probing','analyzing')) AS current_file
            FROM import_batches b LEFT JOIN import_items i ON i.batch_id=b.id
            {qualified_where}
            GROUP BY ALL ORDER BY b.created_at DESC {" " if active else "LIMIT 100"}
        """)))
        return [{**row, "progress": analysis_progress.get(row["id"])} for row in rows]

    def set_state(self, batch_id: str, action: str) -> dict[str, Any]:
        if action not in {"pause", "resume", "cancel", "retry"}:
            raise ValueError("不正な操作です")
        if action == "pause":
            self.session.exec(text("UPDATE import_batches SET paused=true,state='paused',updated_at=CURRENT_TIMESTAMP WHERE id=:id"), params={"id": batch_id})
        elif action == "resume":
            self.session.exec(text("UPDATE import_batches SET paused=false,state='queued',updated_at=CURRENT_TIMESTAMP WHERE id=:id AND NOT cancel_requested"), params={"id": batch_id})
        elif action == "cancel":
            self.session.exec(text("UPDATE import_batches SET cancel_requested=true,state='canceled',updated_at=CURRENT_TIMESTAMP WHERE id=:id"), params={"id": batch_id})
            self.session.exec(text("UPDATE import_target_intents SET state='cancelled' WHERE batch_id=:id AND state='pending'"), params={"id": batch_id})
        else:
            self.session.exec(text("UPDATE import_items SET state='queued',analysis_level=NULL,error_code=NULL,error_message=NULL WHERE batch_id=:id AND state IN ('failed','probing','analyzing')"), params={"id": batch_id})
            self.session.exec(text("UPDATE import_target_intents SET state='pending' WHERE batch_id=:id AND state='cancelled'"), params={"id": batch_id})
            self.session.exec(text("UPDATE import_batches SET paused=false,cancel_requested=false,state='queued',updated_at=CURRENT_TIMESTAMP WHERE id=:id"), params={"id": batch_id})
        self.session.commit()
        return self.get(batch_id)


def process_batch(batch_id: str) -> None:
    """Run durable import batches one at a time across every analysis entrypoint."""
    token = analysis_coordinator.acquire("Play取り込み", wait=True)
    if token is None:  # blocking acquisition only returns None defensively
        return
    try:
        _process_batch(batch_id)
    except Exception as exc:
        # Never leave a batch permanently spinning because setup, teardown, or
        # a transaction outside the per-item handler failed.
        try:
            with db_connection.database_activity, Session(db_connection.engine) as session:
                message = f"{type(exc).__name__}: {exc}"[:1000]
                session.exec(text("""
                    UPDATE import_items SET state='failed',analysis_level='failed',error_code='batch_failed',error_message=:error
                    WHERE batch_id=:id AND state IN ('queued','probing','analyzing')
                """), params={"id": batch_id, "error": message})
                session.exec(text("""
                    UPDATE import_batches SET state='completed_with_errors',updated_at=CURRENT_TIMESTAMP
                    WHERE id=:id AND state NOT IN ('paused','canceled')
                """), params={"id": batch_id})
                session.commit()
        except Exception as cleanup_error:
            print(f"CRITICAL: could not finalize failed import {batch_id}: {cleanup_error}", flush=True)
    finally:
        analysis_progress.clear(batch_id)
        try:
            # Playable tracks get their optional embedding from the background
            # job; it starts once this import releases the analysis slot.
            from app.services.analysis_job_service import analysis_job_service
            with db_connection.database_activity, Session(db_connection.engine) as session:
                track_ids = list(session.exec(text("SELECT DISTINCT track_id FROM import_items WHERE batch_id=:id AND load_ready AND track_id IS NOT NULL"), params={"id": batch_id}).scalars())
            analysis_job_service.enqueue_embedding(track_ids=track_ids)
        except Exception as exc:
            print(f"WARNING: could not queue optional embeddings: {exc}", flush=True)
        analysis_coordinator.release(token)


def _process_batch(batch_id: str) -> None:
    """Single-worker processor. Re-fetches state at every commit boundary."""
    worker_timeout = 180 if sys.platform == "win32" else 570
    with db_connection.database_activity, AnalysisExecutor(max_workers=1, task_timeout=worker_timeout) as executor, Session(db_connection.engine) as session:
        service = PlayImportService(session)
        try:
            batch = service.get(batch_id)
        except ValueError:
            return
        if batch["cancel_requested"] or batch["paused"]:
            return
        requested_profile = batch.get("analysis_profile") or "auto"
        effective_profile = batch.get("effective_analysis_profile")
        if sys.platform != "win32":
            # Keep the established macOS/Essentia path untouched.
            effective_profile = "full"
        elif requested_profile == "auto":
            effective_profile = effective_profile or "full"
        else:
            effective_profile = requested_profile
        # ``auto`` prefers a detailed result for new Windows imports, but an
        # already-playable light result is still a completed automatic import.
        # Requiring ``effective_profile`` here would retry the known-slow full
        # path for up to 180 seconds every time that track is added again.
        # macOS and an explicit full request continue to require full analysis.
        reuse_profile = (
            "full"
            if sys.platform != "win32" or requested_profile == "full"
            else requested_profile
        )
        if requested_profile == "full":
            executor.task_timeout = 570
        elif effective_profile == "light":
            executor.task_timeout = LIGHT_WORKER_TIMEOUT
        session.exec(text("UPDATE import_batches SET state='processing',updated_at=CURRENT_TIMESTAMP WHERE id=:id"), params={"id": batch_id})
        session.commit()
        for item in batch["items"]:
            current = session.exec(text("SELECT paused,cancel_requested FROM import_batches WHERE id=:id"), params={"id": batch_id}).first()
            if not current or current[0] or current[1]:
                return
            if item["state"] not in {"queued", "failed", "probing", "analyzing"}:
                continue
            path = Path(item["canonical_path"])
            def progress(event):
                analysis_progress.update(batch_id, str(path), event)
            try:
                progress({"stage": "checking", "label": "音源の内容・登録済み情報を確認しています"})
                if not path.is_file() or not os.access(path, os.R_OK) or path.stat().st_size == 0:
                    raise ValueError("音源を読み取れません")
                before = path.stat()
                identity = _sha(path)
                session.exec(text("UPDATE import_items SET state='probing',sha256=:hash,attempts=attempts+1 WHERE id=:id"),
                             params={"id": item["id"], "hash": identity})
                session.commit()
                track = session.exec(select(Track).where(Track.filepath == str(path))).first()
                existing_embedding = session.get(TrackEmbedding, track.id) if track else None
                if track:
                    known = session.exec(text("SELECT sha256 FROM track_media WHERE track_id=:id"), params={"id": track.id}).first()
                    if known and known[0] and known[0] != identity:
                        raise ValueError("登録済み音源の内容が変化しています。参照修復または再解析を行ってください")
                reused = bool(track and has_completed_analysis_for_profile(
                    track, existing_embedding, reuse_profile, session.get(TrackAnalysis, track.id)
                ))
                if not track:
                    matches = _rows(session.exec(text("""
                        SELECT t.* FROM track_media m JOIN tracks t ON t.id=m.track_id
                        WHERE m.sha256=:hash
                    """), params={"hash": identity}))
                    if len(matches) == 1 and Path(matches[0]["filepath"]).is_file() and _sha(Path(matches[0]["filepath"])) == identity:
                        track = session.get(Track, matches[0]["id"])
                        existing_embedding = session.get(TrackEmbedding, track.id) if track else None
                        reused = bool(track and has_completed_analysis_for_profile(
                            track, existing_embedding, reuse_profile, session.get(TrackAnalysis, track.id)
                        ))
                    elif len(matches) > 1:
                        raise ValueError("同じ内容の既存曲が複数あり、選択が必要です")
                if not reused:
                    session.exec(text("UPDATE import_items SET state='analyzing' WHERE id=:id"), params={"id": item["id"]})
                    session.commit()
                    from domain.services.ingestion_domain_service import IngestionDomainService
                    domain = IngestionDomainService()

                    async def analyze(profile: str):
                        loop = asyncio.get_running_loop()
                        return await domain.process_track_ingestion(
                            str(path), bool(track), loop, executor=executor,
                            timeout=600 if profile == "full" else LIGHT_WORKER_TIMEOUT + 30,
                            save_to_db=False, write_source_metadata=False,
                            analysis_profile=profile,
                            on_progress=progress,
                        )

                    try:
                        result = asyncio.run(analyze(effective_profile))
                    except Exception as exc:
                        timed_out = any(
                            isinstance(error, TimeoutError)
                            for error in _exception_chain(exc)
                        ) or "timed out" in str(exc).lower()
                        if not (
                            sys.platform == "win32"
                            and requested_profile == "auto"
                            and effective_profile == "full"
                            and timed_out
                        ):
                            raise
                        # Remember the fallback durably so the rest of this
                        # batch (and a resumed batch) does not repeat a known
                        # over-budget detailed analysis.
                        effective_profile = "light"
                        executor.task_timeout = LIGHT_WORKER_TIMEOUT
                        session.exec(text("""
                            UPDATE import_batches
                            SET effective_analysis_profile='light',updated_at=CURRENT_TIMESTAMP
                            WHERE id=:id
                        """), params={"id": batch_id})
                        session.commit()
                        result = asyncio.run(analyze("light"))
                    if not result or not has_completed_analysis_result(result, existing_embedding):
                        raise ValueError("解析に必要な情報（BPM・長さ・特徴量・ビートなど）が不足しています")
                    progress({"stage": "saving", "label": "音源の整合性を確認し、解析結果を保存しています"})
                    after = path.stat()
                    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns) or _sha(path) != identity:
                        raise ValueError("解析中に音源が変更されました。再試行してください")
                    saved = IngestionRepository().save_track_result(session, result, True)
                    track = session.get(Track, saved["track_id"])
                if not track:
                    raise ValueError("曲を保存できませんでした")
                # End the read transaction before observing control changes made
                # while inference was running. A canceled intent must never insert.
                session.commit()
                current = session.exec(text("SELECT paused,cancel_requested FROM import_batches WHERE id=:id"), params={"id": batch_id}).first()
                if not current or current[0] or current[1]:
                    session.exec(text("UPDATE import_items SET state='queued' WHERE id=:id"), params={"id": item["id"]})
                    session.commit()
                    return
                stat = Path(track.filepath).stat()
                session.exec(text("""
                    INSERT INTO track_media (track_id,sha256,size_bytes,mtime_ns,status,last_verified_at)
                    VALUES (:track,:hash,:size,:mtime,'available',now())
                    ON CONFLICT(track_id) DO UPDATE SET sha256=excluded.sha256,size_bytes=excluded.size_bytes,
                      mtime_ns=excluded.mtime_ns,status='available',last_verified_at=now()
                """), params={"track": track.id, "hash": identity, "size": stat.st_size, "mtime": stat.st_mtime_ns})
                intent = session.exec(text("SELECT * FROM import_target_intents WHERE item_id=:id"), params={"id": item["id"]}).first()
                membership = "not_required"
                entry_id = None
                if intent and intent._mapping["state"] != "cancelled" and intent._mapping["target_kind"] == "local_playlist":
                    target_id = intent._mapping["target_id"]
                    target = session.exec(text("SELECT id FROM setlists WHERE id=:id"), params={"id": target_id}).first()
                    if not target:
                        membership = "target_missing"
                    else:
                        previous = session.exec(text("""
                            SELECT intent.setlist_track_id FROM import_target_intents intent
                            JOIN import_items old_item ON old_item.id=intent.item_id
                            JOIN import_batches old_batch ON old_batch.id=intent.batch_id
                            JOIN setlist_tracks entry ON entry.id=intent.setlist_track_id AND entry.setlist_id=intent.target_id AND entry.track_id=:track
                            WHERE intent.target_id=:target AND old_item.canonical_path=:path
                              AND intent.state IN ('applied','applied_existing') AND intent.setlist_track_id IS NOT NULL
                              AND old_batch.id<>:batch ORDER BY old_batch.created_at DESC LIMIT 1
                        """), params={"target": target_id, "path": str(path), "batch": batch_id, "track": track.id}).first()
                        if previous:
                            entry_id, membership = int(previous[0]), "applied_existing"
                        else:
                            entry_id = SetlistRepository(session).insert_track(int(target_id), int(track.id), None, commit=False)
                            membership = "applied"
                session.exec(text("""
                    UPDATE import_items SET track_id=:track,state=:state,load_ready=true,
                      analysis_level=:level,error_code=NULL,error_message=NULL WHERE id=:id
                """), params={
                    "track": track.id, "state": "existing" if reused else "completed",
                    "level": track.analysis_level or "full", "id": item["id"],
                })
                session.exec(text("UPDATE import_target_intents SET state=:state,setlist_track_id=:entry WHERE item_id=:id"),
                             params={"state": membership, "entry": entry_id, "id": item["id"]})
                session.commit()
            except Exception as exc:
                session.rollback()
                session.exec(text("UPDATE import_items SET state='failed',analysis_level='failed',error_code='processing_failed',error_message=:error WHERE id=:id"),
                             params={"id": item["id"], "error": str(exc)[:1000]})
                session.commit()
        failed = int(session.exec(text("SELECT count(*) FROM import_items WHERE batch_id=:id AND state='failed'"), params={"id": batch_id}).one()[0])
        unfinished = int(session.exec(text("SELECT count(*) FROM import_items WHERE batch_id=:id AND state NOT IN ('completed','existing','failed','skipped')"), params={"id": batch_id}).one()[0])
        state = "paused" if unfinished else "completed_with_errors" if failed else "completed"
        session.exec(text("UPDATE import_batches SET state=:state,updated_at=CURRENT_TIMESTAMP WHERE id=:id AND NOT cancel_requested AND NOT paused"),
                     params={"id": batch_id, "state": state})
        session.commit()
