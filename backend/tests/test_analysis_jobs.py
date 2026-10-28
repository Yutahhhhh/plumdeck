import json
import threading

import numpy as np
import pytest

from app.services.analysis_job_service import AnalysisJobService
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.models.lyrics import Lyrics
from domain.services.analysis.analyzer import AudioAnalyzer
from domain.services.analysis.constants import EMBEDDING_MODEL, EMBEDDING_PIPELINE_VERSION
from infra.repositories.analysis_job_repository import AnalysisJobRepository
from infra.repositories.track_repository import TrackRepository
from app.services.recommendation_app_service import RecommendationAppService
from utils.embedding import cosine_similarity


def result():
    return {
        "embedding": [0.1] * 200, "embedding_model": EMBEDDING_MODEL,
        "features_extra": {"embedding_pipeline_version": EMBEDDING_PIPELINE_VERSION,
                           "analysis_components": {"embedding": EMBEDDING_PIPELINE_VERSION}},
    }, 0.01


def add_track(session, number, **kwargs):
    values = dict(filepath=f"/test/{number}.mp3", title=f"Track {number}", artist="Artist",
                  genre="House", bpm=128, key="8A", duration=180)
    values.update(kwargs)
    track = Track(**values)
    session.add(track)
    session.commit()
    session.refresh(track)
    return track


def finish(service):
    service._thread.join(timeout=5)
    assert not service.is_running


def test_embedding_job_preserves_metadata_lyrics_and_other_analysis(session, tmp_path):
    track = add_track(session, 1, is_genre_verified=True, subgenre="Deep House", energy=0.7)
    analysis = TrackAnalysis(track_id=track.id, features_extra_json='{"bpm_confidence":0.9}')
    analysis.beat_positions = [0.5, 1.0]
    analysis.waveform_peaks = [0.2, 0.8]
    session.add(analysis)
    session.add(Lyrics(track_id=track.id, content="Keep these lyrics"))
    session.commit()
    session.refresh(track)
    before = track.model_dump()
    beats, waveform = analysis.beats_f32, analysis.waveform_u8
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: result())
    job = service.start(workers=1)
    finish(service)
    session.rollback()
    session.expire_all()
    assert session.get(Track, track.id).model_dump() == before
    assert session.get(Lyrics, track.id).content == "Keep these lyrics"
    updated = session.get(TrackAnalysis, track.id)
    assert updated.beats_f32 == beats and updated.waveform_u8 == waveform
    assert updated.features_extra["bpm_confidence"] == 0.9
    assert session.get(TrackEmbedding, track.id).model_name == EMBEDDING_MODEL
    assert service.status(job["id"])["counts"]["completed"] == 1
    assert service.plan()["selected_tracks"] == 0


def test_pause_resume_saves_completed_work_and_avoids_repeating_it(session, tmp_path):
    first = add_track(session, 1)
    second = add_track(session, 2)
    entered, release = threading.Event(), threading.Event()
    calls = []
    def analyze(path, features):
        calls.append(path)
        entered.set()
        assert release.wait(3)
        return result()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    job = service.start(workers=1)
    assert entered.wait(3)
    assert service.pause(job["id"])["status"] == "pausing"
    release.set()
    finish(service)
    assert service.status(job["id"])["status"] == "paused"
    assert service.status(job["id"])["counts"]["completed"] == 1
    service.resume(job["id"])
    finish(service)
    assert service.status(job["id"])["status"] == "completed"
    assert calls == [first.filepath, second.filepath]


def test_bad_embeddings_fail_without_overwriting_and_can_be_retried(session, tmp_path):
    track = add_track(session, 1)
    session.add(TrackEmbedding(track_id=track.id, embedding_json=json.dumps([0.2] * 200)))
    session.commit()
    bad, seconds = result()
    bad["embedding"] = [float("nan")] * 200
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: (bad, seconds))
    job = service.start(workers=1)
    finish(service)
    assert service.status(job["id"])["status"] == "completed_with_errors"
    session.rollback()
    assert session.get(TrackEmbedding, track.id).model_name == "musicnn"
    service.analyze_fn = lambda *args: result()
    service.resume(job["id"], retry_failed=True)
    finish(service)
    assert service.status(job["id"])["counts"]["failed"] == 0


def test_queue_recovers_interrupted_items_without_losing_completed_ones(tmp_path):
    path = str(tmp_path / "jobs.sqlite3")
    repo = AnalysisJobRepository(path)
    job_id = repo.create({"features": ["embedding"], "workers": 1}, [{"id": 1, "filepath": "/one"}, {"id": 2, "filepath": "/two"}])
    repo.claim(job_id)
    repo.finish(job_id, 1, "completed")
    repo.claim(job_id)
    repo.update_job(job_id, "running")
    reopened = AnalysisJobRepository(path)
    reopened.recover()
    assert reopened.get(job_id)["status"] == "paused"
    assert reopened.get(job_id)["counts"] == dict(pending=1, running=0, completed=1, skipped=0, failed=0)


def test_embedding_only_does_not_call_other_analysis_or_metadata(mocker):
    analyzer = AudioAnalyzer.__new__(AudioAnalyzer)
    mocker.patch.object(analyzer, "_load_audio", return_value=np.ones(44100, dtype=np.float32))
    mocker.patch.object(analyzer, "_extract_embedding", return_value=result()[0])
    basic = mocker.patch.object(analyzer, "_extract_features")
    timbre = mocker.patch.object(analyzer, "_extract_timbre_features")
    metadata = mocker.patch.object(analyzer, "_extract_metadata")
    waveform = mocker.patch.object(analyzer, "_compute_waveform_peaks")
    value = analyzer.analyze_selected("/test", ["embedding"])
    assert "bpm" not in value and "title" not in value
    for algorithm in (basic, timbre, metadata, waveform):
        algorithm.assert_not_called()


def test_plan_filters_scope_and_current_versions(session, tmp_path):
    first = add_track(session, 1)
    add_track(session, 2, genre="Techno")
    session.add(TrackEmbedding(track_id=first.id, model_name=EMBEDDING_MODEL, embedding_json=json.dumps([0.1] * 200)))
    session.commit()
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    assert service.plan(genres=["House"])["selected_tracks"] == 0
    assert service.plan(only_outdated=False)["selected_tracks"] == 2
    assert service.plan(track_ids=[])["selected_tracks"] == 0
    with pytest.raises(ValueError):
        service.start(features=["lyrics"])
    with pytest.raises(ValueError):
        service.start(workers=8)


def test_similarity_and_genre_suggestions_never_mix_pipeline_versions(session):
    current = add_track(session, 1)
    old = add_track(session, 2, genre="Techno", is_genre_verified=True)
    compatible = add_track(session, 3, is_genre_verified=True)
    for t, model in ((current, EMBEDDING_MODEL), (old, "musicnn"), (compatible, EMBEDDING_MODEL)):
        session.add(TrackEmbedding(track_id=t.id, model_name=model, embedding_json=json.dumps([0.1] * 200)))
    session.commit()
    assert [t["id"] for t in TrackRepository(session).get_similar_tracks(current.id)] == [compatible.id]
    assert RecommendationAppService(session).suggest_genre(current.id)["suggested_genre"] == "House"
    assert cosine_similarity(np.ones(200), np.ones(200), "musicnn", EMBEDDING_MODEL) is None
    assert cosine_similarity(np.ones(200), np.ones(200), EMBEDDING_MODEL, EMBEDDING_MODEL) == pytest.approx(1)
