"""Legacy stored beats remain usable when the catalog BPM is missing."""
import pytest

from domain.models.track import Track, TrackAnalysis
from infra import rekordbox_grid


@pytest.mark.parametrize("stored_bpm, expected_bpm", [(None, 120.0), (0.0, 120.0), (128.0, 128.0)])
def test_metadata_get_uses_stored_beats_with_optional_bpm(client, session, monkeypatch, stored_bpm, expected_bpm):
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = Track(filepath="/synthetic/missing-audio.mp3", title="Track", artist="Artist", genre="House",
                  bpm=stored_bpm, duration=180.0)
    session.add(track)
    session.flush()
    analysis = TrackAnalysis(track_id=track.id)
    analysis.beat_positions = [0.2, 0.7, 1.2]
    session.add(analysis)
    session.commit()

    response = client.get(f"/api/tracks/{track.id}/performance-metadata")
    assert response.status_code == 200
    grid = response.json()["beat_grid"]
    assert grid["source"] == "analysis"
    assert grid["bpm"] == pytest.approx(expected_bpm)
    assert grid["beat_times_ms"] == pytest.approx([200, 700, 1200])
    session.refresh(track)
    assert track.bpm == stored_bpm
