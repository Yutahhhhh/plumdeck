"""Explicit imports retry missing embeddings without duplicating queued work."""
import threading

import pytest
from app.services.analysis_job_service import AnalysisJobService
from domain.models.track import Track, TrackAnalysis


def make_service(session, tmp_path):
    track = Track(filepath="/synthetic/track.mp3", title="Track", artist="Artist", genre="House",
                  bpm=128, duration=180, energy=.7, danceability=.6, brightness=.4, noisiness=.2,
                  analysis_level="full")
    session.add(track)
    session.flush()
    analysis = TrackAnalysis(track_id=track.id)
    analysis.waveform_peaks = [.2, .7]
    session.add(analysis)
    session.commit()
    track_id = track.id
    session.rollback()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: None)
    service._shutdown = True  # inspect queue state without starting native workers
    return service, track_id


def add_job(service, track_id, status, item_status="pending", path="/synthetic/track.mp3", background=True):
    config = {"background": background, "features": ["embedding"], "workers": 1, "only_outdated": True}
    job_id = service.repository.create(config, [{"id": track_id, "filepath": path}], status=status)
    if item_status != "pending":
        service.repository.finish(job_id, track_id, item_status)
    return job_id


@pytest.mark.parametrize("status,item_status", [("queued", "pending"), ("running", "running"),
                                                 ("pausing", "running")])
def test_existing_background_work_is_not_duplicated(session, tmp_path, status, item_status):
    service, track_id = make_service(session, tmp_path)
    existing = add_job(service, track_id, status, item_status)
    assert service.enqueue_embedding(track_ids=[track_id]) == {"status": "idle"}
    assert service.repository.get(existing)["total"] == 1
    with service.repository.connect() as queue:
        assert queue.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1


def test_paused_background_work_does_not_strand_tracks(session, tmp_path):
    # Only an MCP pause or a restore leaves an automatic job paused; nothing
    # in the app resumes it, so a later import queues the track again.
    service, track_id = make_service(session, tmp_path)
    add_job(service, track_id, "paused")
    assert service.enqueue_embedding(track_ids=[track_id])["status"] == "queued"


@pytest.mark.parametrize("failed_job_status", ["running", "completed_with_errors"])
def test_failed_items_are_retryable_and_moved_paths_do_not_block(session, tmp_path, failed_job_status):
    service, track_id = make_service(session, tmp_path)
    add_job(service, track_id, failed_job_status, "failed")
    add_job(service, track_id, "queued", path="/synthetic/old.mp3")
    new = service.enqueue_embedding(track_ids=[track_id])
    assert new["status"] == "queued" and new["total"] == 1
    assert service.enqueue_embedding(filepaths=["/synthetic/track.mp3"]) == {"status": "idle"}


def test_simultaneous_enqueues_create_one_job(session, tmp_path):
    service, track_id = make_service(session, tmp_path)
    barrier = threading.Barrier(2)
    results = []
    def enqueue():
        barrier.wait(timeout=5)
        results.append(service.enqueue_embedding(track_ids=[track_id]))
    threads = [threading.Thread(target=enqueue) for _ in range(2)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(10)
    assert all(not thread.is_alive() for thread in threads)
    assert sorted(job["status"] for job in results) == ["idle", "queued"]
    with service.repository.connect() as queue:
        assert queue.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1
