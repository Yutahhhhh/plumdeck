"""Durable enrichment yields to foreground import between tracks."""
import json
import threading
import time

from sqlalchemy import text
from sqlmodel import select

from app.services.analysis_coordinator import analysis_coordinator
from app.services.analysis_job_service import AnalysisJobService
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.services.analysis.constants import EMBEDDING_MODEL, EMBEDDING_PIPELINE_VERSION
from infra.repositories.analysis_job_repository import AnalysisJobRepository


def result():
    return {"embedding": [.2] * 200, "embedding_model": EMBEDDING_MODEL,
            "features_extra": {"analysis_components": {"embedding": EMBEDDING_PIPELINE_VERSION}}}, .01


def add_tracks(session, count):
    tracks = [Track(filepath=f"/auto/{n}.mp3", title=f"Track {n}", artist="Artist", genre="House",
                    bpm=128, duration=180, energy=.7, danceability=.6, brightness=.4, noisiness=.2,
                    analysis_level="full") for n in range(count)]
    session.add_all(tracks)
    session.flush()
    for track in tracks:
        analysis = TrackAnalysis(track_id=track.id)
        analysis.waveform_peaks = [.2, .7, .4]
        session.add(analysis)
    session.commit()
    ids = [track.id for track in tracks]
    session.rollback()
    return ids


def test_background_job_waits_and_foreground_has_priority_between_tracks(session, tmp_path):
    ids = add_tracks(session, 2)
    entered, release, foreground_entered, release_foreground = [threading.Event() for _ in range(4)]
    calls = []
    def analyze(path, features):
        calls.append(path)
        if len(calls) == 1:
            entered.set()
            assert release.wait(5)
        return result()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    token = analysis_coordinator.acquire("Existing foreground")
    try:
        job = service.enqueue_embedding(track_ids=ids)
        assert job["config"]["background"] and job["config"]["workers"] == 1
        assert not entered.wait(.1)
    finally:
        analysis_coordinator.release(token)
    assert entered.wait(5)
    def foreground():
        with analysis_coordinator.foreground_request():
            foreground_entered.set()
            token = analysis_coordinator.acquire("Play import", wait=True)
        calls.append("foreground")
        release_foreground.wait(5)
        analysis_coordinator.release(token)
    thread = threading.Thread(target=foreground)
    thread.start()
    try:
        assert foreground_entered.wait(2)
        release.set()
        deadline = time.monotonic() + 5
        while "foreground" not in calls and time.monotonic() < deadline:
            time.sleep(.01)
        assert calls == ["/auto/0.mp3", "foreground"]
        release_foreground.set()
        thread.join(5)
        service._thread.join(5)
        assert service.status(job["id"])["counts"]["completed"] == 2
        session.expire_all()
        assert session.exec(select(TrackEmbedding)).all()
    finally:
        release.set()
        release_foreground.set()
        thread.join(5)
        service.shutdown()


def test_background_queue_recovers_after_shutdown_but_respects_explicit_pause(session, tmp_path):
    ids = add_tracks(session, 1)
    path = str(tmp_path / "jobs.sqlite3")
    token = analysis_coordinator.acquire("Import")
    service = AnalysisJobService(session.bind, path, lambda *args: result())
    try:
        job = service.enqueue_embedding(track_ids=ids)
        service.shutdown()
        assert service.status(job["id"])["status"] == "queued"
    finally:
        analysis_coordinator.release(token)
    recovered = AnalysisJobService(session.bind, path, lambda *args: result())
    recovered.startup()
    recovered._thread.join(5)
    assert recovered.status(job["id"])["status"] == "completed"
    recovered.shutdown()
    repo = AnalysisJobRepository(path)
    paused = repo.create({"background": True, "features": ["embedding"], "workers": 1, "only_outdated": True}, [{"id": ids[0], "filepath": "/auto/0.mp3"}])
    repo.recover()
    assert repo.get(paused)["status"] == "paused"
    assert repo.next_queued(background=True) is None


def test_play_import_is_load_ready_before_background_embedding_finishes(session, tmp_path, monkeypatch):
    from app.services.play_import_service import PlayImportService, process_batch
    import app.services.analysis_job_service as jobs
    from domain.services.ingestion_domain_service import IngestionDomainService
    path = tmp_path / "import.wav"
    path.write_bytes(b"synthetic audio contents")
    entered, release = threading.Event(), threading.Event()
    def analyze(*args):
        entered.set()
        assert release.wait(5)
        return result()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    monkeypatch.setattr(jobs, "analysis_job_service", service)
    async def import_audio(self, filepath, *args, **kwargs):
        return {"filepath": filepath, "title": "New", "artist": "Artist", "genre": "House",
                "bpm": 128, "duration": 180, "energy": .7, "danceability": .6,
                "brightness": .4, "noisiness": .2, "analysis_level": "full",
                "features_extra": {"analysis_components": {"timbre": "timbre-v1"}, "waveform_peaks": [.2, .7]}}
    monkeypatch.setattr(IngestionDomainService, "process_track_ingestion", import_audio)
    batch = PlayImportService(session).create("auto-embedding-test", "collection", None, [str(path)])
    session.commit()
    try:
        process_batch(batch["id"])
        assert entered.wait(5)
        session.expire_all()
        item = PlayImportService(session).get(batch["id"])["items"][0]
        assert item["load_ready"] and item["state"] == "completed"
        assert session.get(TrackEmbedding, item["track_id"]) is None
        session.rollback()
        release.set()
        service._thread.join(5)
        session.expire_all()
        assert session.get(TrackEmbedding, item["track_id"]).model_name == EMBEDDING_MODEL
    finally:
        release.set()
        service.shutdown()


def wait_for_jobs(service, ids):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if all(service.status(job_id)["status"] in {"completed", "completed_with_errors"} for job_id in ids):
            return
        time.sleep(.01)
    raise AssertionError([service.status(job_id) for job_id in ids])


def test_manual_analysis_runs_before_remaining_optional_embeddings(session, tmp_path):
    ids = add_tracks(session, 2)
    entered, release = threading.Event(), threading.Event()
    calls = []
    def analyze(path, features):
        calls.append((path, features))
        if len(calls) == 1:
            entered.set()
            assert release.wait(5)
        if features == ["timbre"]:
            return {"energy": .8, "danceability": .7, "brightness": .6, "noisiness": .3,
                    "features_extra": {"analysis_components": {"timbre": "timbre-v1"}}}, .01
        return result()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    try:
        background = service.enqueue_embedding(track_ids=ids)
        assert entered.wait(5)
        manual = service.start(track_ids=[ids[0]], features=["timbre"], only_outdated=False, workers=1)
        assert manual["status"] == "queued"
        assert service.status()["id"] == manual["id"]
        release.set()
        wait_for_jobs(service, [manual["id"], background["id"]])
        assert calls == [("/auto/0.mp3", ["embedding"]), ("/auto/0.mp3", ["timbre"]), ("/auto/1.mp3", ["embedding"])]
    finally:
        release.set()
        service.shutdown()


def test_enqueue_at_previous_worker_exit_is_not_stranded(session, tmp_path, monkeypatch):
    ids = add_tracks(session, 2)
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: result())
    scanned, exit_scan = threading.Event(), threading.Event()
    original = service.repository.next_queued
    intercepted = False
    def next_queued(**kwargs):
        nonlocal intercepted
        result = original(**kwargs)
        if threading.current_thread().name == "plumdeck-analysis-job" and result is None and not intercepted:
            intercepted = True
            scanned.set()
            assert exit_scan.wait(5)
        return result
    monkeypatch.setattr(service.repository, "next_queued", next_queued)
    first = service.enqueue_embedding(track_ids=[ids[0]])
    assert scanned.wait(5)
    second = []
    thread = threading.Thread(target=lambda: second.append(service.enqueue_embedding(track_ids=[ids[1]])))
    thread.start()
    try:
        exit_scan.set()
        thread.join(5)
        assert second
        wait_for_jobs(service, [first["id"], second[0]["id"]])
        assert service.status(second[0]["id"])["counts"]["completed"] == 1
    finally:
        exit_scan.set()
        thread.join(5)
        service.shutdown()


def test_new_foreground_analysis_invalidates_old_embedding_but_metadata_does_not(session, tmp_path):
    from infra.repositories.ingestion_repository import IngestionRepository
    ids = add_tracks(session, 1)
    old = TrackEmbedding(track_id=ids[0], embedding_json=json.dumps([.1] * 200), model_name=EMBEDDING_MODEL)
    session.add(old)
    session.commit()
    repository = IngestionRepository()
    repository.save_track_result(session, {"filepath": "/auto/0.mp3", "title": "Renamed"})
    assert session.get(TrackEmbedding, ids[0]) is not None
    repository.save_track_result(session, {"filepath": "/auto/0.mp3", "title": "Renamed", "bpm": 129,
        "duration": 180, "energy": .8, "danceability": .7, "brightness": .6, "noisiness": .3,
        "features_extra": {"analysis_components": {"timbre": "timbre-v1"}}})
    assert session.get(TrackEmbedding, ids[0]) is None
    session.rollback()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: result())
    try:
        job = service.enqueue_embedding(track_ids=ids)
        wait_for_jobs(service, [job["id"]])
        session.expire_all()
        assert json.loads(session.get(TrackEmbedding, ids[0]).embedding_json) == [.2] * 200
    finally:
        service.shutdown()


def blocking_first_run(runs, started):
    """run_isolated stand-in: the first track blocks until it is cancelled."""
    def run(function, args, timeout, cancel_event):
        runs.append(args[0])
        if len(runs) == 1:
            started.set()
            assert cancel_event.wait(5)
            raise RuntimeError("Analysis cancelled")
        return result()
    return run


def test_foreground_request_cancels_the_background_track_in_flight(session, tmp_path, monkeypatch):
    import app.services.analysis_job_service as jobs
    ids = add_tracks(session, 1)
    runs, started = [], threading.Event()
    monkeypatch.setattr(jobs, "run_isolated", blocking_first_run(runs, started))
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    try:
        job = service.enqueue_embedding(track_ids=ids)
        assert started.wait(5)
        began = time.monotonic()
        token = analysis_coordinator.acquire("Play取り込み", wait=True)
        try:
            # The Play import does not wait for the ~9 s embedding to finish.
            assert time.monotonic() - began < 2
            counts = service.status(job["id"])["counts"]
            assert counts["pending"] == 1 and counts["failed"] == 0
        finally:
            analysis_coordinator.release(token)
        wait_for_jobs(service, [job["id"]])
        assert runs == ["/auto/0.mp3", "/auto/0.mp3"]
        assert service.status(job["id"])["counts"]["completed"] == 1
    finally:
        service.shutdown()


def test_backup_pauses_the_background_job_and_resumes_it_afterwards(session, tmp_path, monkeypatch):
    import app.services.analysis_job_service as jobs
    import infra.database.connection as db
    ids = add_tracks(session, 1)
    runs, started = [], threading.Event()
    monkeypatch.setattr(jobs, "run_isolated", blocking_first_run(runs, started))
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    monkeypatch.setattr(jobs, "analysis_job_service", service)
    try:
        job = service.enqueue_embedding(track_ids=ids)
        assert started.wait(5)
        with db.background_analysis_paused():
            assert not service.is_running
            assert service.status(job["id"])["status"] == "queued"
            # Imports finishing meanwhile do not restart it before the backup ends.
            service._launch_next_queued()
            assert not service.is_running
        wait_for_jobs(service, [job["id"]])
        assert runs == ["/auto/0.mp3", "/auto/0.mp3"]
    finally:
        service.shutdown()
