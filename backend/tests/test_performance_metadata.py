from datetime import datetime
from concurrent.futures import ThreadPoolExecutor
import threading

import pytest
from sqlmodel import Session
from api.schemas.performance_metadata import BeatGrid, PerformanceMetadataWrite
from app.services.performance_metadata_app_service import (
    PerformanceMetadataAppService,
    PerformanceMetadataConflictError,
)
from infra.database import connection as db_connection

from domain.models.track import Track


def _track(session: Session, duration: float = 180.0) -> Track:
    track = Track(
        filepath="/performance/metadata.mp3",
        title="Metadata",
        artist="plumdeck",
        genre="House",
        bpm=128.0,
        key="8A",
        duration=duration,
        created_at=datetime.now(),
    )
    session.add(track)
    session.commit()
    session.refresh(track)
    return track


def test_get_returns_unpersisted_empty_metadata(client, session: Session):
    track = _track(session)

    response = client.get(f"/api/tracks/{track.id}/performance-metadata")

    assert response.status_code == 200
    assert response.json() == {
        "track_id": track.id,
        "revision": 0,
        "cue_points": [],
        "loops": [],
        "beat_grid": None,
        "created_at": None,
        "updated_at": None,
        "grid_warning": None,
        "grid_pending": False,
    }


def test_replace_round_trips_and_increments_revision(client, session: Session):
    track = _track(session)
    payload = {
        "revision": 0,
        "cue_points": [{"slot": 0, "position_ms": 1250, "label": "Intro", "color": "cyan"}],
        "loops": [{"id": "intro-8", "start_ms": 1250, "end_ms": 5000, "label": "Intro loop"}],
        "beat_grid": {"bpm": 128, "first_beat_ms": 312.5, "beats_per_bar": 4},
    }

    saved = client.put(f"/api/tracks/{track.id}/performance-metadata", json=payload)

    assert saved.status_code == 200
    assert saved.json()["revision"] == 1
    assert saved.json()["cue_points"] == payload["cue_points"]
    assert saved.json()["loops"] == payload["loops"]
    assert saved.json()["beat_grid"] == BeatGrid(**payload["beat_grid"]).model_dump()
    assert client.get(f"/api/tracks/{track.id}/performance-metadata").json() == saved.json()

    # Grid-only UI edits still send a full replacement to preserve saved cues/loops.
    existing = saved.json()
    corrected_grid = {"bpm": 127.87, "first_beat_ms": 425.25, "beats_per_bar": 4}
    corrected = client.put(f"/api/tracks/{track.id}/performance-metadata", json={
        "revision": existing["revision"],
        "cue_points": existing["cue_points"],
        "loops": existing["loops"],
        "beat_grid": corrected_grid,
    })
    assert corrected.status_code == 200
    assert corrected.json()["revision"] == 2
    assert corrected.json()["cue_points"] == payload["cue_points"]
    assert corrected.json()["loops"] == payload["loops"]
    assert corrected.json()["beat_grid"] == BeatGrid(**corrected_grid).model_dump()
    assert client.get(f"/api/tracks/{track.id}/performance-metadata").json() == corrected.json()
    session.refresh(track)
    assert track.bpm == 128.0  # Performance override does not rewrite analyzed track metadata.


def test_replace_rejects_stale_revision_without_mutation(client, session: Session):
    track = _track(session)
    url = f"/api/tracks/{track.id}/performance-metadata"
    first = client.put(url, json={
        "revision": 0,
        "cue_points": [{"slot": 1, "position_ms": 1000, "label": "Keep cue", "color": "blue"}],
        "loops": [{"id": "keep-loop", "start_ms": 1000, "end_ms": 5000, "label": "Keep loop"}],
        "beat_grid": {"bpm": 128, "first_beat_ms": 312.5, "beats_per_bar": 4},
    })
    assert first.status_code == 200

    stale = client.put(
        url,
        json={"revision": 0, "cue_points": [], "loops": [],
              "beat_grid": {"bpm": 64, "first_beat_ms": 0, "beats_per_bar": 4}},
    )

    assert stale.status_code == 409
    assert stale.json()["detail"]["current_revision"] == 1
    assert client.get(url).json() == first.json()


@pytest.mark.parametrize("bpm,first_beat_ms,status", [
    (20, 0, 200),
    (300, 0, 200),
    (128, 9999.99, 200),
    (19.99, 0, 422),
    (300.01, 0, 422),
    (128, -0.01, 422),
    (128, 10000, 422),
    (128, 10000.01, 422),
])
def test_grid_bpm_and_offset_boundaries(client, session: Session, bpm, first_beat_ms, status):
    track = _track(session, duration=10)
    url = f"/api/tracks/{track.id}/performance-metadata"
    before = client.get(url).json()
    grid = {"bpm": bpm, "first_beat_ms": first_beat_ms, "beats_per_bar": 4}
    response = client.put(url, json={
        "revision": before["revision"], "cue_points": before["cue_points"],
        "loops": before["loops"], "beat_grid": grid,
    })
    assert response.status_code == status, response.text
    if status == 200:
        assert response.json()["beat_grid"] == BeatGrid(**grid).model_dump()
        assert response.json()["revision"] == 1
        assert client.get(url).json() == response.json()
    else:
        assert client.get(url).json() == before


def test_replace_validates_unique_slots_and_track_bounds(client, session: Session):
    track = _track(session, duration=10)
    url = f"/api/tracks/{track.id}/performance-metadata"

    duplicate = client.put(
        url,
        json={
            "revision": 0,
            "cue_points": [{"slot": 0, "position_ms": 100}, {"slot": 0, "position_ms": 200}],
            "loops": [],
        },
    )
    assert duplicate.status_code == 422

    beyond_duration = client.put(
        url,
        json={
            "revision": 0,
            "cue_points": [],
            "loops": [{"id": "too-long", "start_ms": 9000, "end_ms": 10001}],
        },
    )
    assert beyond_duration.status_code == 422


def test_missing_track_is_not_created(client):
    assert client.get("/api/tracks/999999/performance-metadata").status_code == 404
    response = client.put(
        "/api/tracks/999999/performance-metadata",
        json={"revision": 0, "cue_points": [], "loops": []},
    )
    assert response.status_code == 404


def test_concurrent_initial_replacements_resolve_to_success_and_conflict(session: Session):
    track = _track(session)
    barrier = threading.Barrier(2)

    def replace_once(slot: int):
        with Session(db_connection.engine) as worker_session:
            barrier.wait(timeout=10)
            try:
                saved = PerformanceMetadataAppService(worker_session).replace(
                    track.id,
                    PerformanceMetadataWrite(
                        revision=0,
                        cue_points=[{"slot": slot, "position_ms": 1000 + slot}],
                    ),
                )
                return ("saved", saved["revision"])
            except PerformanceMetadataConflictError as exc:
                return ("conflict", exc.current_revision)

    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(replace_once, [0, 1]))

    assert sorted(results) == [("conflict", 1), ("saved", 1)]


def test_cue_points_lists_only_stored_cues(client, session: Session):
    """一覧向けの一括取得。保存済みのスロットだけを返し、グリッド解析はしない。"""
    track = _track(session)
    other = Track(
        filepath="/performance/no-cues.mp3",
        title="No cues",
        artist="plumdeck",
        genre="House",
        bpm=128.0,
        key="8A",
        duration=180.0,
        created_at=datetime.now(),
    )
    session.add(other)
    session.commit()
    session.refresh(other)
    client.put(
        f"/api/tracks/{track.id}/performance-metadata",
        json={
            "revision": 0,
            "cue_points": [
                {"slot": 3, "position_ms": 1000.0, "label": "D", "color": None},
                {"slot": 0, "position_ms": 250.0, "label": "A", "color": None},
            ],
            "loops": [],
            "beat_grid": None,
        },
    )

    response = client.post(
        "/api/tracks/performance-metadata/cue-points",
        json={"track_ids": [track.id, other.id]},
    )

    assert response.status_code == 200
    # スロットは昇順で、キューの無いトラックは含まれない。
    # 8 スロットぶんの配列で、未設定は null。プレビュー波形にそのまま渡せる形。
    assert response.json() == {str(track.id): [250.0, None, None, 1000.0, None, None, None, None]}


def test_cue_points_accepts_an_empty_request(client):
    response = client.post(
        "/api/tracks/performance-metadata/cue-points", json={"track_ids": []}
    )

    assert response.status_code == 200
    assert response.json() == {}
