"""Database/filesystem regression tests; no audio hardware or inference required.

Run: PYTHONPATH=backend pytest --confcutdir=backend/tests/workflow_integrity backend/tests/workflow_integrity
"""
import json
import asyncio
import sqlite3
import zipfile
from pathlib import Path

import pytest
from sqlmodel import Session, select
from sqlalchemy import text

import infra.database.connection as db
from infra.database.schema import init_raw_db
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.models.setlist import Setlist
from app.services.play_import_service import PlayImportService, process_batch
from app.services import workflow_service as workflow
from infra.repositories.analysis_job_repository import AnalysisJobRepository


@pytest.fixture(autouse=True)
def defer_optional_embeddings(monkeypatch):
    # Queue/model integration has dedicated coverage; these fixtures use fake audio.
    from app.services.analysis_job_service import analysis_job_service
    monkeypatch.setattr(analysis_job_service, "enqueue_embedding", lambda **_kwargs: None)


@pytest.fixture
def library(tmp_path, monkeypatch):
    path = tmp_path / "library.duckdb"
    engine = db.create_library_engine(f"duckdb:///{path}")
    monkeypatch.setattr(db, "engine", engine)
    monkeypatch.setattr(db, "DB_PATH", str(path))
    monkeypatch.setattr(db, "DATABASE_URL", f"duckdb:///{path}")
    init_raw_db(engine)
    with Session(engine) as session:
        yield session
    engine.dispose()


def track(session, tmp_path):
    path = tmp_path / "song.wav"
    path.write_bytes(b"audio content")
    value = Track(filepath=str(path), title="Song", artist="DJ", album="", genre="House", duration=60, bpm=120,
                  energy=.6, danceability=.7, brightness=.3, noisiness=.2)
    session.add(value)
    session.flush()
    session.add(TrackEmbedding(
        track_id=value.id,
        embedding_json=json.dumps([0.1] * 200),
        model_name="musicnn",
    ))
    add_waveform(session, value.id)
    session.commit()
    session.refresh(value)
    return value


def add_waveform(session, track_id):
    analysis = TrackAnalysis(track_id=track_id)
    analysis.waveform_peaks = [.2, .7]
    session.add(analysis)


def test_only_completed_analysis_is_marked_and_failed_files_remain_retryable(library, tmp_path):
    from app.services.filesystem_app_service import FilesystemAppService

    complete_path = tmp_path / "complete.mp3"
    missing_bpm_path = tmp_path / "missing-bpm.mp3"
    missing_embedding_path = tmp_path / "missing-embedding.mp3"
    missing_timbre_path = tmp_path / "missing-timbre.mp3"
    for path in (complete_path, missing_bpm_path, missing_embedding_path, missing_timbre_path):
        path.touch()
    timbre = dict(energy=.6, danceability=.7, brightness=.3, noisiness=.2)
    complete = Track(filepath=str(complete_path), title="Complete", artist="DJ", genre="House", duration=60, bpm=120, **timbre)
    missing_bpm = Track(filepath=str(missing_bpm_path), title="Retry BPM", artist="DJ", genre="House", duration=60, **timbre)
    missing_embedding = Track(filepath=str(missing_embedding_path), title="Embedding pending", artist="DJ", genre="House", duration=60, bpm=120, **timbre)
    missing_timbre = Track(filepath=str(missing_timbre_path), title="Retry timbre", artist="DJ", genre="House", duration=60, bpm=120)
    library.add_all([complete, missing_bpm, missing_embedding, missing_timbre])
    library.flush()
    for value in (complete, missing_bpm, missing_timbre):
        library.add(TrackEmbedding(track_id=value.id, embedding_json=json.dumps([0.1] * 200)))
    for value in (complete, missing_bpm, missing_embedding, missing_timbre):
        add_waveform(library, value.id)
    library.add(TrackEmbedding(track_id=missing_embedding.id, embedding_json="[]"))
    library.commit()

    rows = FilesystemAppService(library).list_directory(str(tmp_path))
    files = {row["name"]: row for row in rows if not row["is_dir"]}
    assert files["complete.mp3"]["is_analyzed"] is True
    assert files["missing-bpm.mp3"]["is_analyzed"] is False
    assert files["missing-embedding.mp3"]["is_analyzed"] is True
    assert files["missing-timbre.mp3"]["is_analyzed"] is False
    # This is the exact set left visible by Hide Analyzed.
    assert {name for name, row in files.items() if not row["is_analyzed"]} == {
        "missing-bpm.mp3", "missing-timbre.mp3",
    }


def test_fast_explorer_job_releases_global_analysis_slot(monkeypatch):
    from app.services.analysis_coordinator import analysis_coordinator
    from app.services.ingestion_app_service import IngestionAppService

    service = IngestionAppService()

    async def finish_immediately(_targets, _force_update, _analysis_profile="auto"):
        service.update_state(type="complete")

    monkeypatch.setattr(service, "_run_ingestion", finish_immediately)

    async def run():
        assert await service.start_ingestion([], False)
        task = service.current_task
        assert task is not None
        await task

    asyncio.run(run())
    assert analysis_coordinator.owner is None


def test_explorer_job_waits_for_play_import_instead_of_being_rejected(monkeypatch):
    from app.services.analysis_coordinator import analysis_coordinator
    from app.services.ingestion_app_service import IngestionAppService

    service = IngestionAppService()
    ran = []

    async def record(_targets, _force_update, _analysis_profile="auto"):
        ran.append(analysis_coordinator.owner)
        service.update_state(type="complete")

    monkeypatch.setattr(service, "_run_ingestion", record)
    held = analysis_coordinator.acquire("Play取り込み")
    assert held is not None

    async def run():
        assert await service.start_ingestion(["/music"], False)
        await asyncio.sleep(0.05)
        assert service.state["details"]["stage"] == "Play取り込みの完了を待っています"
        assert service.state["details"]["queued"] is True
        assert ran == []
        analysis_coordinator.release(held)
        await asyncio.wait_for(service.current_task, 5)

    asyncio.run(run())
    assert ran == ["Explorer解析"]
    assert service.state["details"]["queued"] is False
    assert analysis_coordinator.owner is None


def test_cancelling_a_queued_explorer_job_never_takes_the_analysis_slot(monkeypatch):
    from app.services.analysis_coordinator import analysis_coordinator
    from app.services.ingestion_app_service import IngestionAppService

    service = IngestionAppService()

    async def unexpected(*_args):
        raise AssertionError("cancelled queued job must not run")

    monkeypatch.setattr(service, "_run_ingestion", unexpected)
    held = analysis_coordinator.acquire("Play取り込み")

    async def run():
        assert await service.start_ingestion(["/music"], False)
        await asyncio.sleep(0.05)
        await service.cancel_ingestion()

    try:
        asyncio.run(run())
        assert service.state["type"] == "cancelled"
        assert analysis_coordinator.owner == "Play取り込み"
    finally:
        analysis_coordinator.release(held)
    assert analysis_coordinator.owner is None


@pytest.mark.parametrize("result,message", [
    (None, "produced no result"),
    ({"title": "Partial", "artist": "DJ", "duration": 60, "bpm": 120}, "incomplete"),
])
def test_decoder_or_partial_analysis_is_an_error_instead_of_a_skip(tmp_path, monkeypatch, result, message):
    from domain.services import ingestion_domain_service as ingestion_module

    path = tmp_path / "broken.mp3"
    path.write_bytes(b"not audio")
    monkeypatch.setattr(ingestion_module, "analyze_track_file", lambda *_args: result)

    async def run():
        service = ingestion_module.IngestionDomainService()
        with pytest.raises(RuntimeError, match=message):
            await service.process_track_ingestion(
                str(path), True, asyncio.get_running_loop(), save_to_db=False,
            )

    asyncio.run(run())


def test_tagless_audio_is_registered_with_filename_fallback(library, tmp_path, monkeypatch):
    from domain.services import ingestion_domain_service as ingestion_module

    path = tmp_path / "Terrace Conga II.mp3"
    path.write_bytes(b"audio")
    monkeypatch.setattr(ingestion_module, "extract_metadata_smart", lambda *_args: {
        "title": "Terrace Conga II", "artist": "Unknown", "album": "Unknown",
        "genre": "Unknown", "year": None,
    })
    monkeypatch.setattr(ingestion_module, "analyze_track_file", lambda *_args: {
        "filepath": str(path), "title": "Terrace Conga II", "artist": "Unknown",
        "duration": 60, "bpm": 120, "analysis_level": "full",
        "energy": .6, "danceability": .7, "brightness": .3, "noisiness": .2,
        "features_extra": {"waveform_peaks": [.2, .7]},
    })

    async def run():
        return await ingestion_module.IngestionDomainService().process_track_ingestion(
            str(path), True, asyncio.get_running_loop(), save_to_db=True,
        )

    result = asyncio.run(run())
    assert result["title"] == "Terrace Conga II"
    assert result["artist"] == "Unknown"
    assert result["bpm"] == 120
    library.expire_all()
    saved = library.exec(select(Track).where(Track.filepath == str(path))).one()
    embedding = library.get(TrackEmbedding, saved.id)
    assert saved.title == "Terrace Conga II"
    assert saved.artist == "Unknown"
    from utils.ingestion import has_completed_analysis
    assert has_completed_analysis(saved, embedding, library.get(TrackAnalysis, saved.id))


def test_forced_retry_combines_fresh_audio_with_existing_metadata_and_embedding(library, tmp_path, monkeypatch):
    from domain.services import ingestion_domain_service as ingestion_module

    path = tmp_path / "plain.mp3"
    path.write_bytes(b"audio")
    existing = Track(
        filepath=str(path), title="Curated title", artist="Curated artist",
        genre="House", duration=60, bpm=None,
    )
    library.add(existing)
    library.flush()
    library.add(TrackEmbedding(track_id=existing.id, embedding_json=json.dumps([0.1] * 200)))
    library.commit()
    monkeypatch.setattr(ingestion_module, "analyze_track_file", lambda *_args: {
        "filepath": str(path), "title": "plain", "artist": "Unknown",
        "duration": 60, "bpm": 120,
        "energy": .6, "danceability": .7, "brightness": .3, "noisiness": .2,
        "features_extra": {"waveform_peaks": [.2, .7]},
    })

    async def run():
        return await ingestion_module.IngestionDomainService().process_track_ingestion(
            str(path), True, asyncio.get_running_loop(), save_to_db=False,
        )

    result = asyncio.run(run())
    assert result["artist"] == "Curated artist"
    assert result["title"] == "Curated title"
    assert result["bpm"] == 120


@pytest.mark.parametrize("state", ["probing", "analyzing"])
def test_interrupted_item_is_processed(library, tmp_path, state):
    song = track(library, tmp_path)
    batch = PlayImportService(library).create("retry", "collection", None, [song.filepath])
    library.exec(text("UPDATE import_items SET state=:state WHERE batch_id=:id"), params={"state": state, "id": batch["id"]})
    library.commit()
    process_batch(batch["id"])
    result = PlayImportService(library).get(batch["id"])
    assert result["state"] == "completed"
    assert result["items"][0]["state"] == "existing"


def test_removed_playlist_entry_can_be_imported_again(library, tmp_path):
    song = track(library, tmp_path)
    playlist = Setlist(name="Set")
    library.add(playlist); library.commit(); library.refresh(playlist)
    service = PlayImportService(library)
    first = service.create("first", "local_playlist", playlist.id, [song.filepath])
    process_batch(first["id"])
    library.exec(text("DELETE FROM setlist_tracks")); library.commit()
    second = service.create("second", "local_playlist", playlist.id, [song.filepath])
    process_batch(second["id"])
    assert library.exec(text("SELECT count(*) FROM setlist_tracks")).one()[0] == 1


def test_membership_and_completion_rollback_together(library, tmp_path, monkeypatch):
    song = track(library, tmp_path)
    playlist = Setlist(name="Set")
    library.add(playlist); library.commit(); library.refresh(playlist)
    batch = PlayImportService(library).create("atomic", "local_playlist", playlist.id, [song.filepath])
    original = Session.exec
    def fail(self, statement, *args, **kwargs):
        if "UPDATE import_items SET track_id=" in str(statement):
            raise RuntimeError("injected failure after membership insert")
        return original(self, statement, *args, **kwargs)
    with monkeypatch.context() as patch:
        patch.setattr(Session, "exec", fail)
        process_batch(batch["id"])
    assert library.exec(text("SELECT count(*) FROM setlist_tracks")).one()[0] == 0
    library.commit()
    process_batch(batch["id"])
    assert library.exec(text("SELECT count(*) FROM setlist_tracks")).one()[0] == 1


def test_sqlite_snapshot_includes_live_wal(tmp_path):
    source, saved = tmp_path / "queue", tmp_path / "copy"
    con = sqlite3.connect(source)
    try:
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("PRAGMA wal_autocheckpoint=0")
        con.execute("CREATE TABLE test (value TEXT)")
        con.execute("INSERT INTO test VALUES ('committed in WAL')"); con.commit()
        assert Path(str(source) + "-wal").stat().st_size > 0
        workflow._snapshot_sqlite(source, saved)
        with sqlite3.connect(saved) as snapshot:
            assert snapshot.execute("SELECT value FROM test").fetchone()[0] == "committed in WAL"
    finally:
        con.close()


def make_backup(library, tmp_path):
    path = tmp_path / "saved.plumdeck-backup"
    workflow.create_backup(library, str(path), False, False, {"plumdeck.deckCount": "4"})
    library.close()
    return path


def test_restore_absent_queue_does_not_keep_future_jobs(library, tmp_path):
    archive = make_backup(library, tmp_path)
    path = db.DB_PATH + ".analysis-jobs.sqlite3"
    AnalysisJobRepository(path).create({"features": []}, [])
    result = workflow.restore_backup(str(archive), True)
    assert result["restored"]
    assert not Path(path).exists()
    assert Path(result["rollback_path"] + ".analysis-jobs.sqlite3").exists()


def test_failed_restore_rolls_back_both_databases(library, tmp_path, monkeypatch):
    song = track(library, tmp_path)
    queue_path = db.DB_PATH + ".analysis-jobs.sqlite3"
    repo = AnalysisJobRepository(queue_path)
    repo.create({"generation": "old"}, [])
    archive = make_backup(library, tmp_path)
    library.exec(text("UPDATE tracks SET title='Live generation'")); library.commit(); library.close()
    latest = repo.create({"generation": "live"}, [])
    original = db.reopen_db
    calls = 0
    def fail_once(path):
        nonlocal calls
        calls += 1
        if calls == 1:
            raise RuntimeError("injected reopen failure")
        return original(path)
    monkeypatch.setattr(db, "reopen_db", fail_once)
    with pytest.raises(RuntimeError, match="injected"):
        workflow.restore_backup(str(archive), True)
    with Session(db.engine) as restored:
        assert restored.exec(text("SELECT title FROM tracks")).one()[0] == "Live generation"
    assert repo.get(latest)["config"]["generation"] == "live"


@pytest.mark.parametrize("mutate", ["id", "missing_digest", "extra"])
def test_archive_rejects_unsafe_or_unverified_payload(library, tmp_path, mutate):
    archive = make_backup(library, tmp_path)
    with zipfile.ZipFile(archive) as source:
        contents = {name: source.read(name) for name in source.namelist()}
    manifest = json.loads(contents["manifest.json"])
    if mutate == "id": manifest["snapshot_id"] = "../../escape"
    if mutate == "missing_digest": manifest["files"] = [f for f in manifest["files"] if f["path"] != "plumdeck.duckdb"]
    if mutate == "extra": contents["unverified.bin"] = b"unexpected"
    contents["manifest.json"] = json.dumps(manifest).encode()
    bad = tmp_path / "bad.zip"
    with zipfile.ZipFile(bad, "w") as output:
        for name, data in contents.items(): output.writestr(name, data)
    with pytest.raises(ValueError): workflow.inspect_backup(str(bad))


@pytest.mark.parametrize("level", ["automated_file_check", "automated_library_check"])
def test_client_cannot_claim_automatic_usb_verification(library, tmp_path, monkeypatch, level):
    service = workflow.UsbHandoffService(library)
    library.exec(text("INSERT INTO usb_exports (id,setlist_id,state,snapshot_hash,snapshot_json,handoff_path) VALUES ('test',1,'awaiting_rekordbox','hash','{}','/tmp')")); library.commit()
    monkeypatch.setattr(service, "_snapshot", lambda _: {"hash": "hash"})
    with pytest.raises(ValueError): service.mark_checked("test", {"level": level})
    assert library.exec(text("SELECT state FROM usb_exports")).one()[0] != "verified"


def test_restore_gate_rejects_active_connection(library):
    library.exec(text("SELECT 1"))
    with pytest.raises(ValueError):
        with db.exclusive_database(): pass
    library.close()
    with db.exclusive_database(): pass


def test_crash_between_database_replacements_recovers_original_pair(library, tmp_path):
    from infra.database.restore_recovery import begin_restore, recover_pending_restore, marker_path
    import shutil
    track(library, tmp_path)
    library.close()
    db.checkpoint_db(); db.close_db()
    db_path = Path(db.DB_PATH)
    rollback = db_path.with_name(db_path.name + ".pre-restore-crash-test")
    shutil.copy2(db_path, rollback)
    repo = AnalysisJobRepository(str(db_path) + ".analysis-jobs.sqlite3")
    job = repo.create({"generation": "original"}, [])
    workflow._snapshot_sqlite(Path(repo.path), Path(str(rollback) + ".analysis-jobs.sqlite3"))
    begin_restore(db_path, rollback, True)
    db_path.write_bytes(b"interrupted replacement")
    Path(repo.path).write_bytes(b"interrupted queue replacement")
    assert recover_pending_restore(db_path)
    db.reopen_db(str(db_path))
    with Session(db.engine) as session:
        assert session.exec(text("SELECT title FROM tracks")).one()[0] == "Song"
    assert repo.get(job)["config"]["generation"] == "original"
    assert not marker_path(db_path).exists()


def test_cancel_during_analysis_prevents_membership_until_explicit_retry(library, tmp_path, monkeypatch):
    from domain.services.ingestion_domain_service import IngestionDomainService
    path = tmp_path / "new.wav"
    path.write_bytes(b"new audio content")
    playlist = Setlist(name="Target")
    library.add(playlist); library.commit(); library.refresh(playlist)
    batch = PlayImportService(library).create("cancel", "local_playlist", playlist.id, [str(path)])
    async def analyze(self, *args, **kwargs):
        assert kwargs["write_source_metadata"] is False
        assert kwargs["executor"] is not None
        with Session(db.engine) as other:
            PlayImportService(other).set_state(batch["id"], "cancel")
        return {
            "filepath": str(path), "title": "New", "artist": "DJ", "duration": 60,
            "bpm": 120, "energy": .6, "danceability": .7, "brightness": .3, "noisiness": .2,
            "features_extra": {"waveform_peaks": [.2, .7]},
        }
    monkeypatch.setattr(IngestionDomainService, "process_track_ingestion", analyze)
    library.commit()  # End the request snapshot before background work.
    process_batch(batch["id"])
    result = PlayImportService(library).get(batch["id"])
    assert result["state"] == "canceled"
    assert library.exec(text("SELECT count(*) FROM setlist_tracks")).one()[0] == 0
    library.commit()
    PlayImportService(library).set_state(batch["id"], "retry")
    library.commit()
    process_batch(batch["id"])
    assert library.exec(text("SELECT count(*) FROM setlist_tracks")).one()[0] == 1


def test_timeline_keys_are_scoped_to_recording_and_quality_is_estimated(library):
    ids = []
    for key in ("first", "second"):
        ids.append(library.exec(text("INSERT INTO recordings (recording_key,filepath,started_at) VALUES (:key,'/tmp/audio.wav',now()) RETURNING id"), params={"key": key}).one()[0])
    library.commit()
    service = workflow.RecordingTimelineService(library)
    segment = {"eventKey": "same-wire-key", "startFrame": 0, "endFrame": 44100, "title": "Song"}
    for recording_id in ids:
        service.upsert_engine(recording_id, 44100, 88200, 0, [segment])
        result = service.list(recording_id)
        assert len(result) == 1
        assert result[0]["confidence"] == "estimated"
    assert library.exec(text("SELECT DISTINCT timeline_quality FROM recordings")).one()[0] == "engine_sampled"
