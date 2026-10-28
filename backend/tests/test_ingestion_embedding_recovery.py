"""Explorer re-import repairs optional work without redoing playback analysis."""
import asyncio
import time

from app.services.analysis_job_service import AnalysisJobService
from app.services.ingestion_app_service import IngestionAppService
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.services.analysis.constants import EMBEDDING_MODEL


def add_waveforms(session, tracks):
    session.flush()
    for track in tracks:
        analysis = TrackAnalysis(track_id=track.id)
        analysis.waveform_peaks = [.2, .7]
        session.add(analysis)


def test_explicit_reimport_recovers_missing_embedding_after_enqueue_and_job_failures(
    session, tmp_path, monkeypatch,
):
    import app.services.analysis_job_service as jobs
    import utils.ingestion as ingestion

    path = tmp_path / "synthetic.mp3"
    path.write_bytes(b"synthetic fixture")
    track = Track(filepath=str(path), title="Ready", artist="Artist", genre="House",
                  bpm=128, duration=180, energy=.7, danceability=.6,
                  brightness=.4, noisiness=.2, analysis_level="full")
    unrelated = Track(filepath=str(tmp_path / "unrelated.mp3"), title="Unrelated",
                      bpm=128, duration=180, energy=.7, danceability=.6,
                      brightness=.4, noisiness=.2, analysis_level="full")
    session.add_all([track, unrelated])
    add_waveforms(session, [track, unrelated])
    session.commit()
    track_id, unrelated_id = track.id, unrelated.id
    session.rollback()
    monkeypatch.setattr(ingestion, "check_metadata_changed", lambda *_: False)

    calls = []
    def analyze(filepath, features):
        calls.append(filepath)
        if len(calls) == 1:
            raise RuntimeError("synthetic background failure")
        return ({"embedding": [.2] * 200, "embedding_model": EMBEDDING_MODEL,
                 "features_extra": {}}, .01)

    queue = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    monkeypatch.setattr(jobs, "analysis_job_service", queue)
    app = IngestionAppService()
    async def unexpected_foreground(*args, **kwargs):
        raise AssertionError("playback-ready track must not be analyzed again")
    monkeypatch.setattr(app.domain_service, "process_track_ingestion", unexpected_foreground)

    def run_import():
        asyncio.run(app._run_ingestion([str(path)], False))
        assert app.state["type"] == "complete"

    try:
        # A registration failure leaves playback ready; the next re-import
        # must recover it even though the foreground filter skips the track.
        original_enqueue = queue.enqueue_embedding
        def unavailable(**kwargs):
            raise RuntimeError("synthetic enqueue failure")
        monkeypatch.setattr(queue, "enqueue_embedding", unavailable)
        run_import()
        assert calls == []
        monkeypatch.setattr(queue, "enqueue_embedding", original_enqueue)
        run_import()
        queue._thread.join(5)
        assert queue.status()["status"] == "completed_with_errors"
        assert calls == [str(path)]
        queue.startup()
        time.sleep(.05)
        assert calls == [str(path)]  # failed work does not retry on its own
        run_import()
        queue._thread.join(5)
        assert queue.status()["status"] == "completed"
        assert calls == [str(path), str(path)]
        session.expire_all()
        assert session.get(TrackEmbedding, track_id).model_name == EMBEDDING_MODEL
        assert session.get(TrackEmbedding, unrelated_id) is None
        session.rollback()
        run_import()
        assert calls == [str(path), str(path)]
    finally:
        queue.shutdown()


def test_reimport_bounds_queue_batches_and_continues_after_one_enqueue_failure(monkeypatch):
    import app.services.analysis_job_service as jobs
    import app.services.ingestion_app_service as ingestion_app

    paths = [f"/synthetic/{number}.mp3" for number in range(501)]
    calls = []
    class Queue:
        def enqueue_embedding(self, *, filepaths):
            calls.append(filepaths)
            if len(calls) == 2:
                raise RuntimeError("synthetic enqueue failure")
    monkeypatch.setattr(jobs, "analysis_job_service", Queue())
    monkeypatch.setattr(ingestion_app, "expand_targets", lambda targets: paths + paths[:1])
    def skip_all(_targets, _force, _profile, skipped):
        skipped.extend(paths + paths[:1])
        return [], len(paths)
    monkeypatch.setattr(ingestion_app, "filter_and_prioritize_files", skip_all)
    app = IngestionAppService()
    asyncio.run(app._run_ingestion(["/synthetic"], False))
    assert app.state["type"] == "complete"
    assert [len(batch) for batch in calls] == [200, 200, 101]
    assert [path for batch in calls for path in batch] == paths


def test_reimport_queues_saved_path_for_canonically_equivalent_unicode(session, tmp_path, monkeypatch):
    import app.services.analysis_job_service as jobs
    import unicodedata
    import utils.ingestion as ingestion

    # Neither the entire NFC nor entire NFD spelling equals the DB spelling.
    # This models a path assembled from differently normalized components.
    path = tmp_path / "café-e\u0301.mp3"
    path.write_bytes(b"synthetic fixture")
    saved_path = str(tmp_path / "cafe\u0301-é.mp3")
    assert saved_path != str(path)
    assert unicodedata.normalize("NFC", saved_path) == unicodedata.normalize("NFC", str(path))
    saved = Track(filepath=saved_path, title="Ready", artist="Artist", genre="House",
                  bpm=128, duration=180, energy=.7, danceability=.6,
                  brightness=.4, noisiness=.2, analysis_level="full")
    session.add(saved)
    add_waveforms(session, [saved])
    session.commit()
    session.rollback()
    monkeypatch.setattr(ingestion, "check_metadata_changed", lambda *_: False)
    calls = []
    def analyze(filepath, features):
        calls.append(filepath)
        return ({"embedding": [.2] * 200, "embedding_model": EMBEDDING_MODEL,
                 "features_extra": {}}, .01)
    queue = AnalysisJobService(session.bind, str(tmp_path / "unicode-jobs.sqlite3"), analyze)
    monkeypatch.setattr(jobs, "analysis_job_service", queue)
    try:
        app = IngestionAppService()
        asyncio.run(app._run_ingestion([str(path)], False))
        assert app.state["type"] == "complete"
        queue._thread.join(5)
        assert queue.status()["status"] == "completed"
        assert calls == [saved_path]
    finally:
        queue.shutdown()
