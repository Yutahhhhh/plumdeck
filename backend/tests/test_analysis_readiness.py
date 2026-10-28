"""One "analyzed" rule for Dashboard, Library, Explorer and Play; embeddings are optional."""
import json
from types import SimpleNamespace

from sqlmodel import select

from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from utils.ingestion import (
    analyzed_track_ids, completed_analysis_condition, has_completed_analysis,
    has_completed_analysis_result,
)

TIMBRE = ("energy", "danceability", "brightness", "noisiness")
LIGHT_COMPONENTS = {"rhythm": "numpy-attacks-v2", "timbre": "bounded-summary-v2"}


def ready(**changes):
    return {**dict(bpm=128.0, duration=180.0, energy=.7, danceability=.6,
                   brightness=.4, noisiness=.2, analysis_level="full"), **changes}


def stored(components=None, beats=True, waveform=True, raw=None):
    analysis = TrackAnalysis(track_id=0, features_extra_json=raw if raw is not None else json.dumps(
        {"analysis_components": components or {}}))
    if beats:
        analysis.beat_positions = [.25, .75, 1.25]
    if waveform:
        analysis.waveform_peaks = [.2, .8]
    return analysis


def result(**changes):
    return {**ready(), "features_extra": {"waveform_peaks": [.2, .8]}, **changes}


def test_completion_ignores_pending_and_invalid_embeddings():
    track = SimpleNamespace(**ready())
    for embedding in (None, [], [0] * 200, [float("nan")] * 200):
        assert has_completed_analysis(track, embedding, stored())
        assert has_completed_analysis_result(result(embedding=embedding))


def test_default_timbre_missing_waveform_and_bad_values_are_incomplete():
    # The 424 library rows with danceability/brightness/noisiness left at 0.0.
    defaults = dict.fromkeys(("danceability", "brightness", "noisiness"), 0.0)
    for changes in ({"bpm": float("nan")}, {"duration": 0}, {"energy": None}, {"energy": 1.2},
                    defaults, dict.fromkeys(TIMBRE, 0.0)):
        assert not has_completed_analysis(SimpleNamespace(**{**ready(), **changes}), None, stored())
        assert not has_completed_analysis_result(result(**changes))
    # One descriptor can measure 0.00 (brightness does on real tracks).
    assert has_completed_analysis(SimpleNamespace(**ready(brightness=0.0)), None, stored())
    assert not has_completed_analysis(SimpleNamespace(**ready()), None, stored(waveform=False))
    assert not has_completed_analysis(SimpleNamespace(**ready()), None, None)
    assert not has_completed_analysis_result(result(features_extra={}))
    # Clipped masters decode above 1.0; storage clamps the waveform.
    assert has_completed_analysis_result(result(features_extra={"waveform_peaks": [.2, 1.3]}))


def test_sql_and_python_readiness_agree_and_every_screen_uses_it(session, client):
    defaults = dict.fromkeys(("danceability", "brightness", "noisiness"), 0.0)
    cases = [
        ({}, {}, True),
        ({"brightness": 0.0}, {}, True),
        ({"bpm": 0}, {}, False),
        ({"duration": 0}, {}, False),
        (defaults, {}, False),
        ({}, {"waveform": False}, False),
        ({"analysis_level": "light"}, {}, False),  # old excerpt-only light row
        ({"analysis_level": "light"}, {"components": LIGHT_COMPONENTS}, True),
        ({"analysis_level": "light"}, {"components": LIGHT_COMPONENTS, "beats": False}, False),
        ({"analysis_level": "light"}, {"raw": "{not json"}, False),
        ({}, {"raw": "{not json"}, True),
    ]
    expected = []
    for n, (changes, analysis_args, complete) in enumerate(cases):
        track = Track(filepath=f"/ready/{n}.mp3", title=f"Ready {n}", artist="Artist", genre="House",
                      **{**ready(), **changes})
        session.add(track)
        session.flush()
        analysis = stored(**analysis_args)
        analysis.track_id = track.id
        session.add(analysis)
        if n == 0:
            session.add(TrackEmbedding(track_id=track.id, embedding_json=json.dumps([.1] * 200)))
        assert has_completed_analysis(track, None, analysis) == complete, n
        if complete:
            expected.append(track.id)
    session.commit()
    actual = session.exec(select(Track.id).where(completed_analysis_condition()).order_by(Track.id)).all()
    assert actual == expected
    assert analyzed_track_ids(session, [None, *range(1, 100)]) == set(expected)

    # Library: the status filter and the per-row flag.
    analyzed = client.get("/api/tracks", params={"status": "analyzed"}).json()
    assert sorted(row["id"] for row in analyzed) == expected
    assert all(row["is_analyzed"] for row in analyzed)
    unanalyzed = client.get("/api/tracks", params={"status": "unanalyzed"}).json()
    assert len(unanalyzed) == len(cases) - len(expected)
    assert not any(row["is_analyzed"] for row in unanalyzed)
    # Dashboard.
    stats = client.get("/api/dashboard").json()
    assert (stats["analyzed_tracks"], stats["unanalyzed_tracks"]) == (len(expected), len(cases) - len(expected))
