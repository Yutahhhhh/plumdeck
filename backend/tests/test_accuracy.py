"""Regression cases for analysis fidelity and setlist constraint handling."""

from datetime import datetime
from unittest.mock import MagicMock

import numpy as np
import pytest

from app.services.setlist_app_service import SetlistAppService
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.services.analysis.analyzer import AudioAnalyzer
from domain.services.setlist_builder import SetlistBuilder
from infra.repositories.ingestion_repository import IngestionRepository
from infra.repositories.recommendation_repository import RecommendationRepository
from utils.audio_math import calculate_mixability_score, normalize_key


def make_track(number, **values):
    defaults = dict(
        id=number, filepath=f"/accuracy/{number}.mp3", title=f"Track {number}",
        artist=f"Artist {number}", genre="House", bpm=120.0, duration=180.0,
        energy=0.5, key="8A",
    )
    defaults.update(values)
    return Track(**defaults)


@pytest.mark.parametrize("key,expected", [
    ("C Major", "8B"), ("C Minor", "5A"), ("D# Minor", "2A"),
    ("Eb Major", "5B"), ("F# Major", "2B"), ("Bb Minor", "3A"),
    ("Db Minor", "12A"), ("Gb Minor", "11A"), ("Ab Minor", "1A"),
    ("Cb Major", "1B"), ("B# Minor", "5A"), ("E# Major", "7B"),
])
def test_key_names_are_case_insensitive_without_substring_collisions(key, expected):
    assert normalize_key(key.lower()) == expected


@pytest.mark.parametrize("key,expected", [
    ("D♭ major", "3B"), ("f♯ min", "11A"), ("Am", "8A"),
    ("C Maj", "8B"), ("CM", "8B"), (" eb ", "5B"), ("8a", "8A"),
    ("0A", None), ("13B", None), ("mix in C Major", None),
])
def test_common_key_notations_and_invalid_keys(key, expected):
    assert normalize_key(key) == expected


@pytest.mark.parametrize("first,second", [(120, 128), (70, 140), (75, 135), (80, 130)])
def test_tempo_compatibility_is_symmetric(first, second):
    assert calculate_mixability_score(first, "8A", second, "8A") == pytest.approx(
        calculate_mixability_score(second, "8A", first, "8A")
    )


@pytest.mark.parametrize("unknown", [0, None, float("nan"), float("inf"), -120])
def test_unknown_tempo_does_not_receive_a_120_bpm_match(unknown):
    unknown_score = calculate_mixability_score(120, "8A", unknown, "8A", 1.0)
    measured_score = calculate_mixability_score(120, "8A", 122, "8A", 1.0)
    assert np.isfinite(unknown_score)
    assert unknown_score < measured_score


def test_musicnn_receives_resampled_audio_with_preserved_pitch(mocker):
    # Use the real Essentia resampler; isolate file IO and model inference only.
    analyzer = AudioAnalyzer.__new__(AudioAnalyzer)
    audio = np.sin(2 * np.pi * 440 * np.arange(44100 * 4) / 44100).astype(np.float32)
    mocker.patch.object(analyzer, "_load_audio", return_value=audio)
    analyzer.embedding_algo = MagicMock(return_value=np.array([[1., 0.], [0., 1.]]))
    analyzer._embedding_initialized = True

    # Embeddings come from the background job's embedding-only analysis.
    result = analyzer.analyze_selected("/accuracy/example.wav", ["embedding"])

    received = analyzer.embedding_algo.call_args.args[0]
    assert len(received) == 16000 * 4
    frequencies = np.fft.rfftfreq(len(received), d=1 / 16000)
    assert frequencies[np.argmax(np.abs(np.fft.rfft(received)))] == pytest.approx(440)
    assert result["embedding"] == [0.5, 0.5]
    assert result["features_extra"]["embedding_sample_rate"] == 16000
    assert result["features_extra"]["embedding_pipeline_version"] == "musicnn-16khz-v1"


def test_bpm_output_preserves_hundredths():
    analyzer = AudioAnalyzer.__new__(AudioAnalyzer)
    tag = MagicMock(year=None, duration=180, title="T", artist="A", album="B", genre="G", extra={})
    features = dict(
        bpm=127.87, key="C", scale="major", bpm_confidence=1,
        beat_positions=[], energy=0.1, danceability=1, brightness=2000, noisiness=0.01,
    )
    assert analyzer._format_result("/test.wav", tag, features)["bpm"] == 127.87


def test_reanalysis_updates_all_audio_features_and_model_provenance(session):
    track = make_track(1, brightness=0.8, noisiness=0.6, danceability=0.7)
    session.add(track)
    session.commit()
    repo = IngestionRepository()
    result = dict(
        filepath=track.filepath, bpm=127.87, duration=190.5, energy=0.0,
        danceability=0.0, brightness=0.2, noisiness=0.0, contrast=0.1,
        loudness=-9.5, loudness_range=3.2, spectral_flux=0.4, spectral_rolloff=4100,
        embedding=[0.5, 0.5], embedding_model="msd-musicnn-1",
        features_extra={"embedding_pipeline_version": "musicnn-16khz-v1"},
    )
    repo._prepare_track_models(session, result)
    session.commit()
    session.refresh(track)
    for feature in ("bpm", "duration", "energy", "danceability", "brightness", "noisiness",
                    "contrast", "loudness", "loudness_range", "spectral_flux", "spectral_rolloff"):
        assert getattr(track, feature) == pytest.approx(result[feature])
    assert session.get(TrackEmbedding, track.id).model_name == "msd-musicnn-1"
    # Diagnostic keys stay in the worker result but are not persisted.
    assert session.get(TrackAnalysis, track.id).features_extra == {}

    # A later metadata-only edit must keep the newly measured features.
    repo._prepare_track_models(session, {"filepath": track.filepath, "title": "New title"})
    session.commit()
    session.refresh(track)
    assert track.title == "New title"
    assert track.brightness == pytest.approx(0.2)
    assert track.bpm == pytest.approx(127.87)
    assert track.loudness == pytest.approx(-9.5)


@pytest.mark.parametrize("target,expected", [
    ({"year_min": 1990, "year_max": 1999}, {2, 3}),
    ({"year_min": 1999}, {3, 4}), ({"year_max": 1990}, {1, 2}),
])
def test_candidate_pool_enforces_year_bounds(session, target, expected):
    session.add_all([make_track(i, year=year) for i, year in enumerate([1989, 1990, 1999, 2000, None], 1)])
    session.commit()
    pool = RecommendationRepository(session).fetch_candidates_pool(target)
    assert {node["id"] for node in pool} == expected


def test_candidate_pool_keeps_half_and_double_time_and_ranks_before_limit(session):
    session.add_all([
        make_track(1, bpm=70, created_at=datetime(2020, 1, 1)),
        make_track(2, bpm=140, created_at=datetime(2020, 1, 1)),
        make_track(3, bpm=100, created_at=datetime(2026, 1, 1)),
        make_track(4, bpm=0, created_at=datetime(2026, 1, 1)),
    ])
    session.commit()
    repo = RecommendationRepository(session)
    assert {node["id"] for node in repo.fetch_candidates_pool({"bpm": 70}, limit=2)} == {1, 2}
    assert {node["id"] for node in repo.fetch_candidates_pool({"bpm": 140}, limit=2)} == {1, 2}


@pytest.mark.parametrize("feature", ["brightness", "noisiness", "danceability"])
def test_candidate_pool_ranks_feature_targets_before_limit(session, feature):
    session.add_all([make_track(1, **{feature: 0.1}), make_track(2, **{feature: 0.9})])
    session.commit()
    pool = RecommendationRepository(session).fetch_candidates_pool({feature: 0.1}, limit=1)
    assert pool[0]["id"] == 1


def test_generation_preserves_seed_order_deduplicates_and_respects_length(session):
    session.add_all([make_track(1), make_track(2), make_track(3)])
    session.commit()
    result = SetlistAppService(session).generate_auto_setlist(limit=3, seed_track_ids=[3, 1, 3])
    assert [track["id"] for track in result] == [3, 1, 2]
    result = SetlistAppService(session).generate_auto_setlist(limit=1, seed_track_ids=[3, 1])
    assert [track["id"] for track in result] == [3]


def test_chain_start_is_reproducible_and_does_not_mutate_pool():
    tracks = [make_track(2, energy=0.2), make_track(1, energy=0.8)]
    pool = [{"id": t.id, "track": t, "vector": None} for t in tracks]
    builder = SetlistBuilder()
    for _ in range(20):
        assert [t.id for t in builder.build_chain(pool, [], 2, {"energy": 0.8})] == [1, 2]
    assert [node["id"] for node in pool] == [2, 1]
    assert builder.build_chain(pool, [], 0, {}) == []
