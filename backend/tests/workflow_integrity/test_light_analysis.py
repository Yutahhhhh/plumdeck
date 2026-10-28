import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace

import numpy as np
import pytest
from sqlalchemy import text
from sqlmodel import Session

import infra.database.connection as db
from infra.database.schema import init_raw_db
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.services.analysis.portable import PortableAudioAnalyzer
from domain.services.analysis import light_dsp
from utils.ingestion import (
    has_completed_analysis,
    has_completed_analysis_for_profile,
    has_completed_analysis_result,
)


def light_result(**overrides):
    return {
        "title": "Title", "artist": "DJ", "duration": 60, "bpm": 126,
        "analysis_level": "light", "energy": .6, "danceability": .7,
        "brightness": .3, "noisiness": .2,
        "features_extra": {"beat_positions": [.25, .726, 1.202], "waveform_peaks": [.1, .8, .3],
                           "analysis_components": {"rhythm": "numpy-attacks-v2", "timbre": "bounded-summary-v2"}},
        **overrides,
    }


def stored_light_analysis(track_id=1):
    result = light_result()
    analysis = TrackAnalysis(track_id=track_id, features_extra_json=json.dumps(result["features_extra"]))
    analysis.beat_positions = result["features_extra"]["beat_positions"]
    analysis.waveform_peaks = result["features_extra"]["waveform_peaks"]
    return analysis


@pytest.fixture(autouse=True)
def defer_optional_embeddings(monkeypatch):
    # These foreground-flow fixtures use fake audio; queue execution is tested
    # by the analysis-job suite rather than starting inference after teardown.
    from app.services.analysis_job_service import analysis_job_service
    calls = []
    monkeypatch.setattr(analysis_job_service, "enqueue_embedding", lambda **kwargs: calls.append(kwargs))
    return calls


def test_windows_analyzer_startup_does_not_import_essentia():
    script = """
import numpy
import sys
import tempfile
sys.platform = 'win32'
from domain.services.analysis.analyzer import AudioAnalyzer
analyzer = AudioAnalyzer()
assert type(analyzer).__name__ == 'PortableAudioAnalyzer'
assert 'essentia' not in sys.modules
"""
    environment = os.environ.copy()
    environment["PYTHONPATH"] = str(Path(__file__).resolve().parents[2])
    completed = subprocess.run(
        [sys.executable, "-c", script],
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stderr


def test_light_worker_deadline_allows_slow_windows_startup():
    assert light_dsp.WORKER_TIMEOUT >= 180


def test_portable_light_analysis_keeps_whole_track_grid_without_model_work(monkeypatch):
    analyzer = PortableAudioAnalyzer()
    analyzer._extract_metadata = lambda _: SimpleNamespace(
        duration=300, title="Title", artist="DJ", album="Album",
        genre="House", year="2026", extra={},
    )
    analyzer._load_grid_audio = lambda _: np.full(300 * light_dsp.SAMPLE_RATE, .1, dtype=np.float32)
    lengths = []
    def rhythm(audio):
        lengths.append(len(audio))
        return 126., .8, "C", "minor", .7
    monkeypatch.setattr(light_dsp, "rhythm_and_key", rhythm)
    from domain.services.analysis import light_grid
    monkeypatch.setattr(light_grid, "analyze", lambda audio, **_: (126., np.arange(.25, 300, 60 / 126), .8))
    def unexpected_embedding(*_args):
        raise AssertionError("Foreground light analysis must not compute an embedding")
    analyzer._extract_light_embedding = unexpected_embedding
    result = analyzer.analyze_light("/music/track.mp3")
    assert lengths == [30 * 11025]
    assert "embedding" not in result
    assert has_completed_analysis_result(result)
    assert result["features_extra"]["beat_positions"][-1] > 299
    assert len(result["features_extra"]["waveform_peaks"]) == 500
    assert result["features_extra"]["playback_grid"]["first_beat_ms"] == pytest.approx(250)
    assert "embedding_patch_count" not in result["features_extra"]


def test_windows_background_embedding_uses_light_windows_without_librosa(monkeypatch):
    from domain.services.analysis import constants
    analyzer = PortableAudioAnalyzer()
    def no_librosa(_self):
        raise AssertionError("background embedding must not import librosa")
    monkeypatch.setattr(PortableAudioAnalyzer, "librosa", property(no_librosa))
    analyzer._load_grid_audio = lambda _path: np.zeros(300 * light_dsp.SAMPLE_RATE, dtype=np.float32)
    # The same three ~3 s ONNX windows the light import used to compute.
    requests = []
    def load(path, start, duration, sample_rate):
        requests.append((start, duration, sample_rate))
        return np.full(duration * sample_rate, .1, dtype=np.float32)
    analyzer._load_audio_segment = load
    def predict(outputs, inputs):
        assert inputs["melspectrogram"].shape == (1, 187, 96)
        return [np.full((1, 200), .2, dtype=np.float32)]
    analyzer._embedding_session = SimpleNamespace(run=predict)
    result = analyzer.analyze_selected("/music/track.mp3", ["embedding"])
    assert requests == [(58.5, 3, 16000), (148.5, 3, 16000), (238.5, 3, 16000)]
    assert len(result["embedding"]) == 200 and result["embedding_model"] == constants.EMBEDDING_MODEL
    assert result["features_extra"]["analysis_components"] == {"embedding": constants.COMPONENT_VERSIONS["embedding"]}


def test_light_completion_requires_timbre_and_playback_but_embedding_is_optional():
    result = light_result()
    assert "embedding" not in result and has_completed_analysis_result(result)
    for optional_embedding in [None, [], [0] * 200]:
        assert has_completed_analysis_result(dict(result, embedding=optional_embedding))
    unmeasured = dict(result, features_extra={**result["features_extra"], "analysis_components": {
        key: value for key, value in result["features_extra"]["analysis_components"].items() if key != "timbre"}})
    for incomplete in [dict(result, features_extra={}), dict(result, bpm=float("nan")),
                       dict(result, energy=None), dict(result, energy=1.2),
                       dict(unmeasured, energy=0, danceability=0, brightness=0, noisiness=0),
                       dict(unmeasured, danceability=0, brightness=0, noisiness=0)]:
        assert not has_completed_analysis_result(incomplete)
    # Measured near-silent or sub-bass audio may sit at 0.00 and still imports.
    assert has_completed_analysis_result(dict(result, danceability=0, brightness=0, noisiness=0))
    track = SimpleNamespace(title="Title", artist="DJ", duration=60, bpm=126, analysis_level="light",
                            energy=.6, danceability=.7, brightness=.3, noisiness=.2)
    analysis = stored_light_analysis()
    assert not has_completed_analysis(track, None)  # old excerpt-only light must be upgraded
    assert has_completed_analysis(track, None, analysis)
    assert not has_completed_analysis_for_profile(track, None, "full", analysis)
    assert has_completed_analysis_for_profile(track, None, "auto", analysis)


def test_audio_analysis_completion_is_independent_of_tag_metadata():
    from utils.metadata import has_valid_metadata

    result = light_result(title="Terrace Conga II", artist="Unknown")
    track = SimpleNamespace(
        title=result["title"], artist=result["artist"], duration=60,
        bpm=126, analysis_level="light", energy=.6, danceability=.7, brightness=.3, noisiness=.2,
    )
    assert not has_valid_metadata(track)
    assert has_completed_analysis_result(result)
    assert has_completed_analysis(track, None, stored_light_analysis())


def test_light_data_reaches_playback_recommendations_and_preserves_manual_cues(tmp_path, monkeypatch):
    from infra.repositories.ingestion_repository import IngestionRepository
    from infra.repositories.track_repository import TrackRepository
    from app.services.performance_metadata_app_service import PerformanceMetadataAppService
    from app.services.recommendation_app_service import RecommendationAppService
    from app.services.grid_candidate_service import GridCandidateService
    from api.schemas.performance_metadata import PerformanceMetadataWrite

    engine = db.create_library_engine(f"duckdb:///{tmp_path / 'features.duckdb'}")
    init_raw_db(engine)
    monkeypatch.setattr(GridCandidateService, "rekordbox", lambda *args, **kwargs: None)
    try:
        with Session(engine) as session:
            repo = IngestionRepository()
            original = light_result(filepath=str(tmp_path / "light.mp3"),
                                    embedding=[.2] * 200, embedding_model="musicnn")
            light_id = repo.save_track_result(session, original)["track_id"]
            full_id = repo.save_track_result(session, light_result(
                filepath=str(tmp_path / "full.mp3"), analysis_level="full", genre="House",
                embedding=[.2] * 200, embedding_model="musicnn",
            ))["track_id"]
            full = session.get(Track, full_id)
            full.is_genre_verified = True
            session.add(full)
            session.commit()
            metadata = PerformanceMetadataAppService(session)
            grid = metadata.get(light_id)["beat_grid"]
            assert grid["first_beat_ms"] == 250
            assert grid["beat_times_ms"] == pytest.approx([250, 726, 1202], abs=.001)
            assert TrackRepository(session).get_similar_tracks(light_id)[0]["id"] == full_id
            assert RecommendationAppService(session).suggest_genre(light_id) == {"suggested_genre": "House", "reason": None}
            saved = metadata.replace(light_id, PerformanceMetadataWrite(
                revision=0, cue_points=[{"slot": 0, "position_ms": 1000}],
                loops=[{"id": "loop", "start_ms": 1000, "end_ms": 3000}],
                beat_grid={"bpm": 125, "first_beat_ms": 255, "source": "manual"},
            ))
            repo.save_track_result(session, original)
            after = metadata.get(light_id)
            for key in ("cue_points", "loops", "beat_grid", "revision"):
                assert after[key] == saved[key]
    finally:
        engine.dispose()


@pytest.mark.parametrize("has_embedding", [True, False])
def test_light_save_does_not_downgrade_an_existing_full_analysis(tmp_path, has_embedding):
    from infra.repositories.ingestion_repository import IngestionRepository

    engine = db.create_library_engine(f"duckdb:///{tmp_path / 'library.duckdb'}")
    init_raw_db(engine)
    try:
        with Session(engine) as session:
            track = Track(
                filepath=str(tmp_path / "track.mp3"), title="Title", artist="DJ",
                genre="House", duration=180, bpm=126, analysis_level="full",
                energy=.8, danceability=.7,
            )
            session.add(track)
            session.flush()
            if has_embedding:
                session.add(TrackEmbedding(
                    track_id=track.id, embedding_json=json.dumps([.1] * 200),
                    model_name="musicnn",
                ))
            analysis = TrackAnalysis(
                track_id=track.id, features_extra_json=json.dumps({"analysis_level": "full"}),
            )
            analysis.waveform_peaks = [1]
            session.add(analysis)
            session.commit()

            IngestionRepository().save_track_result(session, {
                "filepath": track.filepath, "title": "Title", "artist": "DJ",
                "genre": "House", "duration": 180, "bpm": 124,
                "analysis_level": "light", "energy": .2, "danceability": .1,
                "features_extra": {"analysis_level": "light", "beat_positions": [1., 2.], "waveform_peaks": [.2]},
                "embedding": [.9] * 200, "embedding_model": "msd-musicnn-1:musicnn-16khz-v1",
            })
            saved = session.get(Track, track.id)
            analysis = session.get(TrackAnalysis, track.id)
            assert saved.analysis_level == "full"
            assert saved.bpm == 126
            assert saved.energy == pytest.approx(.8)
            assert json.loads(analysis.features_extra_json)["analysis_level"] == "full"
            embedding = session.get(TrackEmbedding, track.id)
            if has_embedding:
                assert json.loads(embedding.embedding_json) == [.1] * 200
                assert embedding.model_name == "musicnn"
            else:
                assert embedding is None
    finally:
        engine.dispose()


def test_analyze_track_file_keeps_full_path_when_light_is_not_supported(monkeypatch):
    import ingest

    called = []
    analyzer = SimpleNamespace(
        analyze=lambda filepath, **kwargs: called.append((filepath, kwargs)) or {"ok": True}
    )
    monkeypatch.setattr(ingest, "get_analyzer", lambda: analyzer)

    assert ingest.analyze_track_file("track.mp3", analysis_profile="light") == {"ok": True}
    assert called == [("track.mp3", {
        "skip_basic": False, "skip_waveform": False, "external_lyrics": None,
    })]


def test_explicit_full_play_import_upgrades_a_light_track(tmp_path, monkeypatch):
    from app.services import play_import_service as play_module
    from domain.services import ingestion_domain_service as ingestion_module

    engine = db.create_library_engine(f"duckdb:///{tmp_path / 'upgrade.duckdb'}")
    monkeypatch.setattr(db, "engine", engine)
    init_raw_db(engine)
    path = tmp_path / "light.mp3"
    path.write_bytes(b"audio")
    calls = []

    class ImmediateExecutor:
        def __init__(self, *_, task_timeout=None, **__):
            self.task_timeout = task_timeout
        def __enter__(self):
            return self
        def __exit__(self, *_):
            return False

    class FullDomain:
        async def process_track_ingestion(self, filepath, _force, _loop, **kwargs):
            calls.append(kwargs["analysis_profile"])
            return {
                "filepath": filepath, "title": "Title", "artist": "DJ",
                "genre": "House", "duration": 60, "bpm": 126,
                "analysis_level": "full", "features_extra": {"analysis_level": "full", "waveform_peaks": [.2, .7]},
                "energy": .6, "danceability": .7, "brightness": .3, "noisiness": .2,
            }

    monkeypatch.setattr(play_module, "AnalysisExecutor", ImmediateExecutor)
    monkeypatch.setattr(ingestion_module, "IngestionDomainService", FullDomain)
    try:
        with Session(engine) as session:
            light = Track(
                filepath=str(path), title="Title", artist="DJ", genre="House",
                duration=60, bpm=126, analysis_level="light",
            )
            session.add(light)
            session.commit()
            batch = play_module.PlayImportService(session).create(
                "upgrade-light", "collection", None, [str(path)],
                analysis_profile="full",
            )
            session.rollback()
        play_module.process_batch(batch["id"])
        with Session(engine) as session:
            saved_batch = play_module.PlayImportService(session).get(batch["id"])
            upgraded = session.exec(text("SELECT analysis_level FROM tracks")).one()[0]
            assert saved_batch["items"][0]["state"] == "completed"
            assert saved_batch["items"][0]["analysis_level"] == "full"
            assert upgraded == "full"
            assert session.exec(text("SELECT count(*) FROM track_embeddings")).one()[0] == 0
        assert calls == ["full"]
    finally:
        engine.dispose()


def test_windows_auto_play_import_reuses_an_existing_light_track(tmp_path, monkeypatch):
    from app.services import play_import_service as play_module

    engine = db.create_library_engine(f"duckdb:///{tmp_path / 'reuse-light.duckdb'}")
    monkeypatch.setattr(db, "engine", engine)
    init_raw_db(engine)
    path = tmp_path / "light.mp3"
    path.write_bytes(b"audio")

    class ImmediateExecutor:
        def __init__(self, *_, task_timeout=None, **__):
            self.task_timeout = task_timeout
        def __enter__(self):
            return self
        def __exit__(self, *_):
            return False

    monkeypatch.setattr(play_module.sys, "platform", "win32")
    monkeypatch.setattr(play_module, "AnalysisExecutor", ImmediateExecutor)
    try:
        with Session(engine) as session:
            track = Track(
                filepath=str(path), title="Title", artist="DJ", genre="House",
                duration=60, bpm=126, analysis_level="light",
                energy=.6, danceability=.7, brightness=.3, noisiness=.2,
            )
            session.add(track)
            session.flush()
            session.add(stored_light_analysis(track.id))
            session.commit()
            batch = play_module.PlayImportService(session).create(
                "reuse-auto-light", "collection", None, [str(path)],
                analysis_profile="auto",
            )
            session.rollback()

        play_module.process_batch(batch["id"])

        with Session(engine) as session:
            saved = play_module.PlayImportService(session).get(batch["id"])
            assert saved["state"] == "completed"
            assert saved["items"][0]["state"] == "existing"
            assert saved["items"][0]["analysis_level"] == "light"
            assert session.exec(text("SELECT count(*) FROM track_embeddings")).one()[0] == 0
    finally:
        engine.dispose()


@pytest.mark.parametrize("requested_profile", ["auto", "light"])
def test_windows_play_import_uses_light_and_bounds_worker(tmp_path, monkeypatch, requested_profile):
    from app.services import play_import_service as play_module
    from domain.services import ingestion_domain_service as ingestion_module

    engine = db.create_library_engine(f"duckdb:///{tmp_path / 'queue.duckdb'}")
    monkeypatch.setattr(db, "engine", engine)
    init_raw_db(engine)
    first = tmp_path / "first.mp3"
    second = tmp_path / "second.mp3"
    first.write_bytes(b"first")
    second.write_bytes(b"second")
    calls = []

    class ImmediateExecutor:
        def __init__(self, *_, task_timeout=None, **__):
            self.task_timeout = task_timeout
        def __enter__(self):
            return self
        def __exit__(self, *_):
            return False

    class FakeDomain:
        async def process_track_ingestion(self, filepath, _force, _loop, **kwargs):
            profile = kwargs["analysis_profile"]
            calls.append((Path(filepath).name, profile))
            if profile == "full":
                raise RuntimeError("wrapped") from TimeoutError("Analysis timed out after 180 seconds")
            assert kwargs["executor"].task_timeout == light_dsp.WORKER_TIMEOUT
            assert kwargs["timeout"] == light_dsp.WORKER_TIMEOUT + 30
            return {
                **light_result(),
                "filepath": filepath, "title": Path(filepath).stem,
                "artist": "DJ", "genre": "House", "duration": 60,
                "bpm": 126, "key": "C minor", "scale": "minor",
                "analysis_level": "light",
            }

    monkeypatch.setattr(play_module.sys, "platform", "win32")
    monkeypatch.setattr(play_module, "AnalysisExecutor", ImmediateExecutor)
    monkeypatch.setattr(ingestion_module, "IngestionDomainService", FakeDomain)
    try:
        with Session(engine) as session:
            batch = play_module.PlayImportService(session).create(
                "auto-light", "collection", None, [str(first), str(second)],
                analysis_profile=requested_profile,
            )
            session.rollback()
        play_module.process_batch(batch["id"])
        with Session(engine) as session:
            saved = play_module.PlayImportService(session).get(batch["id"])
            assert saved["state"] == "completed"
            if requested_profile == "auto":
                assert saved["effective_analysis_profile"] == "light"
            assert [item["analysis_level"] for item in saved["items"]] == ["light", "light"]
            assert session.exec(text("SELECT count(*) FROM track_embeddings")).one()[0] == 0
        expected = [("first.mp3", "full")] if requested_profile == "auto" else []
        assert calls == expected + [("first.mp3", "light"), ("second.mp3", "light")]
    finally:
        engine.dispose()


@pytest.mark.parametrize("requested_profile", ["auto", "light"])
def test_windows_explorer_uses_light_and_bounds_worker(
    tmp_path, monkeypatch, requested_profile,
):
    from app.services import ingestion_app_service as app_module

    first = tmp_path / "first.mp3"
    second = tmp_path / "second.mp3"
    first.touch()
    second.touch()
    calls = []

    class ImmediateExecutor:
        def __init__(self, *_, task_timeout=None, **__):
            self.task_timeout = task_timeout
        def shutdown(self, *_, **__):
            return None

    class FakeDomain:
        async def process_track_ingestion(self, filepath, _force, _loop, *_args, **kwargs):
            profile = kwargs["analysis_profile"]
            calls.append((Path(filepath).name, profile))
            if profile == "full":
                raise RuntimeError("wrapped") from TimeoutError("Analysis timed out")
            assert _args[0].task_timeout == light_dsp.WORKER_TIMEOUT
            assert _args[1] == light_dsp.WORKER_TIMEOUT + 30
            return {"analysis_level": "light"}

    monkeypatch.setattr(app_module.sys, "platform", "win32")
    monkeypatch.setattr(app_module, "AnalysisExecutor", ImmediateExecutor)
    monkeypatch.setattr(app_module, "expand_targets", lambda targets: targets)
    monkeypatch.setattr(
        app_module, "filter_and_prioritize_files",
        lambda targets, _force, _profile, _skipped: (targets, 0),
    )
    service = app_module.IngestionAppService()
    service.domain_service = FakeDomain()

    asyncio.run(service._run_ingestion([str(first), str(second)], False, requested_profile))

    expected = [("first.mp3", "full")] if requested_profile == "auto" else []
    assert calls == expected + [("first.mp3", "light"), ("second.mp3", "light")]
    assert service.state["type"] == "complete"
    assert service.state["processed"] == 2
    assert service.state["details"]["effective_analysis_profile"] == "light"


def test_cancelling_explorer_before_first_step_allows_immediate_retry(monkeypatch):
    from app.services.analysis_coordinator import analysis_coordinator
    from app.services.ingestion_app_service import IngestionAppService

    service = IngestionAppService()
    calls = []

    async def finish(*_args):
        calls.append(analysis_coordinator.owner)
        service.update_state(type="complete")

    monkeypatch.setattr(service, "_run_ingestion", finish)

    async def run():
        assert await service.start_ingestion([], analysis_profile="light")
        await service.cancel_ingestion()  # no event-loop yield before cancel
        assert not service.is_running
        assert service.current_task is None
        assert analysis_coordinator.owner is None
        assert await service.start_ingestion([], analysis_profile="light")
        await asyncio.wait_for(service.current_task, 2)

    asyncio.run(run())
    assert calls == ["Explorer解析"]
    assert analysis_coordinator.owner is None
