"""What analysis saves: allow-listed features_extra, protected rekordbox BPM.

rekordbox itself is unreadable here (conftest), as on a machine without it.
"""
import copy
import json

import pytest

from app.services.analysis_job_service import AnalysisJobService
from app.services.csv_app_service import CsvAppService
from api.schemas.settings import CsvImportRow, ImportExecuteRequest
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.services.analysis.constants import EMBEDDING_MODEL
from infra.repositories.ingestion_repository import IngestionRepository, file_sha256

AUTHORITY = {"source": "rekordbox", "rekordbox_id": "42", "synced_at": "2026-09-07T05:43:13+00:00",
             "match_method": "path"}
GRID = {"bpm": 128.0, "first_beat_ms": 250.0, "beat_times_ms": None, "source": "analysis", "confidence": 3.1}


def analysis_result(filepath, **values):
    """Shape of a full analyzer result, including the keys that must not be persisted."""
    result = {
        "filepath": filepath, "title": "Song", "artist": "Artist", "duration": 180.0,
        "bpm": 64.0, "key": "F minor", "scale": "minor", "energy": 0.4, "danceability": 0.5,
        "brightness": 0.3, "noisiness": 0.2, "analysis_level": "full",
        "embedding": [0.1] * 200, "embedding_model": EMBEDDING_MODEL,
        "features_extra": {
            "analysis_components": {"rhythm": "rhythm-attacks-v4", "key": "key-v1", "timbre": "timbre-v1"},
            "playback_grid": GRID, "playback_grid_version": 2,
            "beat_positions": [0.25, 0.72, 1.19], "waveform_peaks": [0.1, 0.9, 0.4],
            "bpm_confidence": 3.1, "key_strength": 0.8, "analysis_level": "full",
            "analysis_profile": "full", "embedding_sample_rate": 16000,
            "embedding_pipeline_version": "musicnn-16khz-v1",
        },
    }
    result.update(values)
    return result


def add_synced_track(session, filepath="/lib/synced.mp3", bpm=128.0):
    track = Track(filepath=filepath, title="Song", artist="Artist", genre="House", bpm=bpm,
                  key="F minor", duration=180.0, analysis_level="full")
    session.add(track)
    session.flush()
    analysis = TrackAnalysis(track_id=track.id, features_extra_json=json.dumps({
        "analysis_components": {"rhythm": "rhythm-attacks-v4"}, "playback_grid": GRID,
        "playback_grid_version": 2, "bpm_authority": AUTHORITY}))
    analysis.beat_positions = [0.25, 0.72, 1.19]
    analysis.waveform_peaks = [0.1, 0.9, 0.4]
    session.add(analysis)
    session.add(TrackEmbedding(track_id=track.id, model_name=EMBEDDING_MODEL, embedding_json=json.dumps([0.1] * 200)))
    session.commit()
    session.refresh(track)
    return track


def test_ingestion_persists_only_allow_listed_features_extra(session):
    result = analysis_result("/lib/new.mp3")
    original = copy.deepcopy(result)
    track_id = IngestionRepository()._prepare_track_models(session, result)
    session.commit()

    # Diagnostics (e.g. _has_light_playback_data) still read the full result.
    assert result == original
    analysis = session.get(TrackAnalysis, track_id)
    assert analysis.features_extra == {
        "analysis_components": {**original["features_extra"]["analysis_components"],
                                "sources": {"bpm": "own", "key": "own"}},
        "playback_grid": GRID, "playback_grid_version": 2,
        "bpm_confidence": 3.1, "key_strength": 0.8,
    }
    # Arrays live only in the BLOB columns.
    assert analysis.beat_positions == pytest.approx([0.25, 0.72, 1.19])
    assert len(analysis.waveform_peaks) == 3
    track = session.get(Track, track_id)
    assert track.loudness == -60.0 and track.contrast == 0.0 and track.spectral_rolloff == 0.0


def test_forced_reingest_keeps_synced_bpm_while_rekordbox_is_offline(session):
    track = add_synced_track(session)
    IngestionRepository()._prepare_track_models(session, analysis_result(track.filepath, energy=0.9))
    session.commit()
    session.refresh(track)

    assert track.bpm == 128.0
    assert track.energy == pytest.approx(0.9)
    extra = session.get(TrackAnalysis, track.id).features_extra
    # The one-off sync marker becomes the recorded source.
    assert extra["analysis_components"]["sources"]["bpm"] == "rekordbox"
    assert "bpm_authority" not in extra
    assert extra["analysis_bpm"] == 64.0
    assert "beat_positions" not in extra
    # CSV export reads it, so the allow-list keeps it.
    assert extra["key_strength"] == 0.8


def test_reingest_without_marker_still_updates_bpm(session):
    track = Track(filepath="/lib/plain.mp3", title="Song", artist="Artist", bpm=120.0, duration=180.0)
    session.add(track)
    session.commit()
    IngestionRepository()._prepare_track_models(session, analysis_result(track.filepath, bpm=124.5))
    session.commit()
    session.refresh(track)
    assert track.bpm == 124.5
    assert "analysis_bpm" not in session.get(TrackAnalysis, track.id).features_extra


def _run_job(session, tmp_path, features, result):
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), lambda *args: (result, 0.01))
    job = service.start(features=features, only_outdated=False, workers=1)
    service._thread.join(timeout=10)
    assert not service.is_running
    assert service.status(job["id"])["counts"]["completed"] == 1, service.status(job["id"])
    session.rollback()
    session.expire_all()


def test_rhythm_job_does_not_overwrite_rekordbox_bpm(session, tmp_path):
    track = add_synced_track(session)
    _run_job(session, tmp_path, ["rhythm"], {"bpm": 64.0, "features_extra": {
        "beat_positions": [0.5, 1.0, 1.5], "bpm_confidence": 2.0, "playback_grid": GRID,
        "playback_grid_version": 2, "analysis_components": {"rhythm": "rhythm-attacks-v4"},
        "analysis_source": {"size": 1, "mtime_ns": 1}}})

    assert session.get(Track, track.id).bpm == 128.0
    analysis = session.get(TrackAnalysis, track.id)
    assert analysis.beat_positions == [0.5, 1.0, 1.5]
    extra = analysis.features_extra
    # The recorded source replaces the legacy marker.
    assert extra["analysis_components"]["sources"]["bpm"] == "rekordbox"
    assert "bpm_authority" not in extra
    assert extra["analysis_bpm"] == 64.0
    assert not {"analysis_source", "beat_positions"} & extra.keys()
    assert extra["bpm_confidence"] == 2.0  # read by CSV export and the grid fallback


def test_grid_job_moves_an_own_bpm_with_the_new_beats(session, tmp_path):
    # A rekordbox BPM stays: test_rhythm_job_does_not_overwrite_rekordbox_bpm.
    track = Track(filepath="/lib/plain.mp3", title="Song", artist="Artist", bpm=120.0, duration=180.0)
    session.add(track)
    session.commit()
    rhythm = {"bpm": 124.0, "key": "A minor", "scale": "minor", "features_extra": {
        "beat_positions": [0.5, 1.0, 1.5], "playback_grid": GRID, "playback_grid_version": 2,
        "analysis_components": {"rhythm": "rhythm-attacks-v4"}}}
    _run_job(session, tmp_path, ["rhythm"], rhythm)
    assert session.get(Track, track.id).bpm == 124.0
    assert session.get(TrackAnalysis, track.id).beat_positions == [0.5, 1.0, 1.5]
    assert session.get(TrackAnalysis, track.id).features_extra["analysis_components"]["sources"]["bpm"] == "own"

    _run_job(session, tmp_path, ["rhythm", "key"], rhythm)
    saved = session.get(Track, track.id)
    assert (saved.bpm, saved.key, saved.scale) == (124.0, "A minor", "minor")
    extra = session.get(TrackAnalysis, track.id).features_extra
    assert "analysis_bpm" not in extra
    assert extra["analysis_components"]["sources"] == {"bpm": "own", "key": "own"}


def test_timbre_job_saves_the_four_descriptors(session, tmp_path):
    track = Track(filepath="/lib/plain.mp3", title="Song", artist="Artist", bpm=120.0, duration=180.0,
                  loudness=-9.0, contrast=0.4)
    session.add(track)
    session.commit()
    _run_job(session, tmp_path, ["timbre"], {
        "energy": 0.4, "danceability": 0.5, "brightness": 0.3, "noisiness": 0.2,
        "features_extra": {"analysis_components": {"timbre": "timbre-v1"}}})
    saved = session.get(Track, track.id)
    assert (saved.energy, saved.danceability, saved.brightness, saved.noisiness) == pytest.approx((0.4, 0.5, 0.3, 0.2))
    # Legacy values are left as they were; nothing reads them.
    assert (saved.loudness, saved.contrast) == pytest.approx((-9.0, 0.4))


def test_csv_path_update_merges_instead_of_replacing_features_extra(session):
    track = add_synced_track(session, filepath="/old/song.mp3")
    before = session.get(TrackAnalysis, track.id)
    extra_before, beats_before = before.features_extra, before.beats_f32
    row = CsvImportRow(filepath="/new/song.mp3", title="Song", artist="Artist", genre="Techno",
                       bpm_confidence=1.5, key_strength=0.7, bpm_raw=64.0)
    CsvAppService(session).execute_import(ImportExecuteRequest(new_tracks=[], path_updates=[
        {"old_path": "/old/song.mp3", "new_path": "/new/song.mp3", "track": row.model_dump()}]))
    session.expire_all()

    assert session.get(Track, track.id).filepath == "/new/song.mp3"
    analysis = session.get(TrackAnalysis, track.id)
    assert analysis.features_extra == extra_before
    assert analysis.beats_f32 == beats_before


def _skipped_rhythm(filepath, **values):
    """A registered track's import: rhythm skipped, BPM from rekordbox."""
    result = analysis_result(filepath, **values)
    del result["embedding"], result["embedding_model"]
    extra = result["features_extra"]
    for key in ("playback_grid", "playback_grid_version", "beat_positions"):
        del extra[key]
    extra["analysis_components"] = {"timbre": "timbre-v1", "sources": {"bpm": "rekordbox"}}
    return result


def _recorded_audio(session, track, path, content=b"same audio"):
    """Point the track at a real file whose sha256 the last analysis recorded."""
    path.write_bytes(content)
    track.filepath = str(path)
    analysis = session.get(TrackAnalysis, track.id)
    analysis.features_extra_json = json.dumps({**analysis.features_extra, "audio_sha256": file_sha256(str(path))})
    session.add_all([track, analysis])
    session.commit()
    return track


def test_skipped_rhythm_keeps_own_grid_and_embedding_for_the_same_audio(session, mocker, tmp_path):
    from domain.services.value_sources import RekordboxValues
    mocker.patch("domain.services.value_sources.lookup", return_value=RekordboxValues(True, 128.0))
    track = _recorded_audio(session, add_synced_track(session), tmp_path / "song.mp3")
    IngestionRepository()._prepare_track_models(session, _skipped_rhythm(track.filepath, duration=180.3))
    session.commit()
    session.expire_all()
    analysis = session.get(TrackAnalysis, track.id)
    extra = analysis.features_extra
    assert extra["playback_grid"] == GRID and extra["playback_grid_version"] == 2
    assert extra["analysis_components"]["rhythm"] == "rhythm-attacks-v4"
    assert analysis.beat_positions == pytest.approx([0.25, 0.72, 1.19])
    assert session.get(TrackEmbedding, track.id) is not None


@pytest.mark.parametrize("replacement", ["longer", "same_length", "unrecorded", "unreadable"])
def test_skipped_rhythm_on_replaced_audio_clears_the_old_beats(session, mocker, tmp_path, replacement):
    from domain.services.value_sources import RekordboxValues
    mocker.patch("domain.services.value_sources.lookup", return_value=RekordboxValues(True, 128.0))
    path = tmp_path / "song.mp3"
    track = _recorded_audio(session, add_synced_track(session), path)
    duration = 241.0 if replacement == "longer" else 180.0
    if replacement == "same_length":
        path.write_bytes(b"other audio of the same length")
    elif replacement == "unrecorded":
        analysis = session.get(TrackAnalysis, track.id)
        analysis.features_extra_json = json.dumps({k: v for k, v in analysis.features_extra.items()
                                                   if k != "audio_sha256"})
        session.add(analysis)
        session.commit()
    elif replacement == "unreadable":
        path.unlink()
    IngestionRepository()._prepare_track_models(session, _skipped_rhythm(track.filepath, duration=duration))
    session.commit()
    session.expire_all()
    analysis = session.get(TrackAnalysis, track.id)
    assert not analysis.beats_f32
    assert "playback_grid" not in analysis.features_extra
    assert "rhythm" not in analysis.features_extra["analysis_components"]
    assert session.get(TrackEmbedding, track.id) is None
    assert session.get(Track, track.id).bpm == 128.0


def test_media_verification_hash_also_proves_the_same_audio(session, mocker, tmp_path):
    from sqlalchemy import text
    from domain.services.value_sources import RekordboxValues
    mocker.patch("domain.services.value_sources.lookup", return_value=RekordboxValues(True, 128.0))
    path = tmp_path / "song.mp3"
    path.write_bytes(b"verified audio")
    track = add_synced_track(session, filepath=str(path))
    session.exec(text("INSERT INTO track_media (track_id, sha256) VALUES (:id, :hash)"),
                 params={"id": track.id, "hash": file_sha256(str(path))})
    session.commit()
    IngestionRepository()._prepare_track_models(session, _skipped_rhythm(track.filepath))
    session.commit()
    session.expire_all()
    extra = session.get(TrackAnalysis, track.id).features_extra
    assert extra["playback_grid"] == GRID and extra["audio_sha256"] == file_sha256(str(path))
    assert session.get(TrackEmbedding, track.id) is not None
