import json
from pathlib import Path
import struct
from concurrent.futures import ThreadPoolExecutor
import threading

import pytest
from pydantic import ValidationError
from sqlalchemy import text
from sqlmodel import Session

from api.schemas.performance_metadata import BeatGrid
from app.services.grid_candidate_service import GridCandidateService, analysis_candidate
from domain.models.track import TrackAnalysis
from domain.services.analysis.rhythm_grid import GridAnalysisError
from infra import rekordbox_grid, rekordbox_timing
from infra.database import connection as db_connection
from tests.test_performance_metadata import _track


def anlz(entries):
    body = b"".join(struct.pack(">HHI", *e) for e in entries)
    pqtz = struct.pack(">4sIIIII", b"PQTZ", 24, 24 + len(body), 0, 0x80000, len(entries)) + body
    return struct.pack(">4sII", b"PMAI", 28, 28 + len(pqtz)) + bytes(16) + pqtz


def test_pqtz_preserves_variable_tempo_phase_and_bar_position():
    entries = [(3, 12000, 137), (4, 12000, 637), (1, 10000, 1237), (2, 9000, 1904)]
    grid = rekordbox_grid.parse_pqtz(anlz(entries))
    assert grid.beat_times_ms == [137, 637, 1237, 1904]
    assert grid.beat_numbers == [3, 4, 1, 2]
    assert grid.first_beat_ms == 137
    assert grid.source == "rekordbox"
    assert grid.confidence is None


@pytest.mark.parametrize("entries", [
    [(1, 12000, 100), (2, 12000, 100)],
    [(1, 12000, 200), (2, 12000, 100)],
    [(0, 12000, 100), (2, 12000, 600)],
])
def test_invalid_source_beats_are_not_silently_fixed(entries):
    with pytest.raises(rekordbox_grid.RekordboxGridError):
        rekordbox_grid.parse_pqtz(anlz(entries))


@pytest.mark.parametrize("mutation", [
    lambda data: data[:-1],
    lambda data: b"BAD!" + data[4:],
    lambda data: data[:36] + struct.pack(">I", 0) + data[40:],
    lambda data: data[:48] + struct.pack(">I", 100001) + data[52:],
])
def test_malformed_source_is_rejected(mutation):
    with pytest.raises(rekordbox_grid.RekordboxGridError):
        rekordbox_grid.parse_pqtz(mutation(anlz([(1, 12000, 150), (2, 12000, 650)])))


@pytest.mark.parametrize("extra", [
    {"beat_times_ms": [100]},
    {"beat_times_ms": [100, 100]},
    {"beat_times_ms": [100, float("inf")]},
    {"beat_times_ms": [200, 700]},
    {"beat_times_ms": [100, 600], "beat_numbers": [1]},
    {"beat_times_ms": [100, 600], "beat_numbers": [1, 17]},
    {"beat_numbers": [1, 2]}, {"confidence": float("nan")}, {"source": "guess"},
])
def test_grid_schema_validates_array_contract(extra):
    with pytest.raises(ValidationError):
        BeatGrid(bpm=120, first_beat_ms=100, **extra)


def test_analysis_preserves_nonzero_phase_without_inventing_downbeat():
    grid = analysis_candidate([0.137, 0.637, 1.237, 1.904], 110, 4.9)
    assert grid.beat_times_ms == pytest.approx([137, 637, 1237, 1904])
    assert grid.beat_numbers is None
    assert grid.confidence == 4.9
    assert grid.source == "analysis"
    assert analysis_candidate([], 120) is None


def test_constant_analysis_uses_prepared_grid_and_preserves_raw_beats(client, session, monkeypatch):
    import math
    from domain.services.analysis.beat_grid import playback_grid, VERSION
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = _track(session)
    ticks = [.137 + i * .5 + .007 * math.sin(i) for i in range(20)]
    row = TrackAnalysis(track_id=track.id)
    row.beat_positions = ticks
    prepared = playback_grid(ticks, 120, 4)
    row.features_extra_json = json.dumps({"playback_grid": prepared, "playback_grid_version": VERSION})
    session.add(row)
    session.commit()
    response = client.get(f"/api/tracks/{track.id}/performance-metadata")
    assert response.status_code == 200
    grid = response.json()["beat_grid"]
    assert grid["beat_times_ms"] is None
    assert grid["bpm"] == prepared["bpm"]
    assert grid["first_beat_ms"] == prepared["first_beat_ms"]
    assert session.get(TrackAnalysis, track.id).beat_positions == pytest.approx(ticks)


@pytest.fixture
def source_file(tmp_path, monkeypatch):
    path = tmp_path / "ANLZ0000.DAT"
    path.write_bytes(anlz([(3, 12000, 137), (4, 12000, 637), (1, 10000, 1237)]))
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: (path, "external-123"))
    # The catalog track has no audio file to read MP3 headers from; the decoder
    # timing offset is covered by workflow_integrity/test_rekordbox_timing.py.
    monkeypatch.setattr(rekordbox_timing, "timing_offset_ms", lambda _: 0.0)
    return path


def test_get_imports_candidate_and_saved_grid_always_wins(client, session, source_file):
    track = _track(session)
    url = f"/api/tracks/{track.id}/performance-metadata"
    imported = client.get(url).json()
    assert imported["revision"] == 0
    assert imported["beat_grid"]["source"] == "rekordbox"
    assert imported["beat_grid"]["beat_times_ms"] == [137, 637, 1237]
    assert session.exec(text("SELECT count(*) FROM track_performance_metadata")).one()[0] == 0
    provenance = json.loads(session.exec(text("SELECT provenance_json FROM track_grid_candidates")).one()[0])
    assert provenance["read_only"] is True
    assert provenance["external_track_id"] == "external-123"
    saved = client.put(url, json={"revision": 0,
        "cue_points": [{"slot": 0, "position_ms": 200}],
        "loops": [{"id": "keep", "start_ms": 200, "end_ms": 1200}],
        "beat_grid": {"bpm": 121, "first_beat_ms": 222, "source": "manual"}}).json()
    assert client.get(url).json() == saved
    restored = client.post(f"/api/tracks/{track.id}/grid-rekordbox")
    assert restored.status_code == 200
    assert restored.json()["first_beat_ms"] == 137
    assert client.get(url).json() == saved
    conflict = client.put(url, json={"revision": 0, "beat_grid": restored.json()})
    assert conflict.status_code == 409
    assert client.get(url).json() == saved


def test_source_refresh_bypasses_cache(client, session, source_file):
    track = _track(session)
    url = f"/api/tracks/{track.id}/grid-rekordbox"
    assert client.post(url).json()["first_beat_ms"] == 137
    source_file.write_bytes(anlz([(1, 10000, 200), (2, 10000, 800)]))
    assert client.post(url).json()["beat_times_ms"] == [200, 800]


def test_analysis_reuse_force_failure_and_no_saved_metadata_changes(client, session, monkeypatch, tmp_path):
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = _track(session)
    audio = tmp_path / "track.wav"
    audio.write_bytes(b"fixture")
    track.filepath = str(audio)
    old = TrackAnalysis(track_id=track.id)
    old.beat_positions = [0.25, 0.75, 1.25]
    old.features_extra_json = '{"bpm_confidence": 3.2}'
    session.add(track)
    session.add(old)
    session.commit()
    import app.services.grid_candidate_service as service
    calls = []
    def extract(filepath):
        calls.append(filepath)
        return {"bpm": 119, "ticks": [0.3, 0.8, 1.4], "confidence": 4.7}
    monkeypatch.setattr(service, "analyze_grid", extract)
    url = f"/api/tracks/{track.id}/grid-analysis"
    reused = client.post(url, json={"force": False})
    assert reused.status_code == 200
    assert reused.json()["first_beat_ms"] == 250
    assert not calls
    forced = client.post(url, json={"force": True})
    assert forced.status_code == 200
    assert forced.json()["beat_times_ms"] == [300, 800, 1400]
    assert len(calls) == 1
    assert client.post(url, json={"force": False}).json() == forced.json()
    assert len(calls) == 1
    session.refresh(old)
    assert old.beat_positions == [0.25, 0.75, 1.25]
    assert session.exec(text("SELECT count(*) FROM track_performance_metadata")).one()[0] == 0
    def fail(_):
        raise GridAnalysisError("No usable beats")
    monkeypatch.setattr(service, "analyze_grid", fail)
    failed = client.post(url, json={"force": True})
    assert failed.status_code == 422
    assert failed.json()["detail"] == "No usable beats"
    assert client.post(url, json={"force": False}).json() == forced.json()


def test_unsourced_track_never_gets_zero_fallback(client, session, monkeypatch):
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = _track(session)
    assert client.get(f"/api/tracks/{track.id}/performance-metadata").json()["beat_grid"] is None
    assert client.post(f"/api/tracks/{track.id}/grid-rekordbox").status_code == 404
    assert client.post(f"/api/tracks/{track.id}/grid-analysis", json={"force": True}).status_code == 404


def test_rekordbox_connection_is_readonly_and_parameterized(tmp_path, monkeypatch):
    database = tmp_path / "master.db"
    database.touch()
    calls = []
    class Connection:
        def execute(self, statement, params):
            calls.append((statement, params))
            return self
        def fetchall(self):
            return []
        def close(self):
            calls.append("closed")
    monkeypatch.setattr(rekordbox_grid, "connect_readonly", lambda _: Connection())
    filepath = "/music/quote' UNION SELECT danger.mp3"
    assert rekordbox_grid.find_analysis(filepath, database) is None
    assert calls[0][1] == (filepath,)
    assert filepath not in calls[0][0]
    assert calls[-1] == "closed"


def test_sqlcipher_connection_enforces_readonly_uri_and_query_only(tmp_path, monkeypatch):
    from sqlcipher3 import dbapi2
    calls = []
    class Connection:
        def execute(self, statement):
            # Do not retain/log the encryption PRAGMA value.
            calls.append("key" if statement.startswith("PRAGMA key=") else statement)
        def close(self):
            pass
    def connect(database_uri, **kwargs):
        assert database_uri.endswith("master.db?mode=ro")
        assert kwargs["uri"] is True
        assert "immutable" not in database_uri  # The current WAL must remain visible.
        return Connection()
    monkeypatch.setattr(dbapi2, "connect", connect)
    rekordbox_grid.connect_readonly(tmp_path / "master.db")
    assert calls == ["key", "PRAGMA query_only=ON", "BEGIN"]


def test_concurrent_candidate_imports_do_not_conflict_or_create_manual_rows(session, source_file):
    track = _track(session)
    track_id = track.id
    session.rollback()
    barrier = threading.Barrier(2)
    def get_candidate(_):
        from domain.models.track import Track
        with Session(db_connection.engine) as worker_session:
            selected = worker_session.get(Track, track_id)
            barrier.wait(timeout=10)
            return GridCandidateService(worker_session).rekordbox(selected).model_dump()
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(get_candidate, [0, 1]))
    assert results[0] == results[1]
    assert session.exec(text("SELECT count(*) FROM track_grid_candidates")).one()[0] == 1
    assert session.exec(text("SELECT count(*) FROM track_performance_metadata")).one()[0] == 0


@pytest.mark.parametrize("last_ms", [10000, 10001])
def test_save_rejects_tail_outside_duration_without_writing(client, session, monkeypatch, last_ms):
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = _track(session, duration=10)
    url = f"/api/tracks/{track.id}/performance-metadata"
    before = client.get(url).json()
    response = client.put(url, json={"revision": 0, "beat_grid": {
        "bpm": 120, "first_beat_ms": 100, "beat_times_ms": [100, 600, last_ms],
    }})
    assert response.status_code == 422
    assert client.get(url).json() == before


def test_invalid_source_tail_reports_error_without_caching(client, session, source_file):
    track = _track(session, duration=1)
    response = client.post(f"/api/tracks/{track.id}/grid-rekordbox")
    assert response.status_code == 422
    assert "duration" in response.json()["detail"]
    metadata = client.get(f"/api/tracks/{track.id}/performance-metadata")
    assert metadata.status_code == 200
    assert metadata.json()["beat_grid"] is None
    assert "duration" in metadata.json()["grid_warning"]
    assert session.exec(text("SELECT count(*) FROM track_grid_candidates")).one()[0] == 0
    assert session.exec(text("SELECT count(*) FROM track_performance_metadata")).one()[0] == 0


@pytest.mark.parametrize("rekordbox", ["missing", "unreadable"])
def test_get_analyzes_on_demand_without_rekordbox_grid_or_stored_beats(client, session, monkeypatch, tmp_path, rekordbox):
    # Registered tracks skip rhythm analysis at import; if PQTZ cannot be read
    # at load time the deck must still get a grid instead of none.
    def find(_):
        if rekordbox == "unreadable":
            raise rekordbox_grid.RekordboxGridError("Could not read the local rekordbox library")
        return None
    monkeypatch.setattr(rekordbox_grid, "find_analysis", find)
    track = _track(session)
    audio = tmp_path / "track.wav"
    audio.write_bytes(b"fixture")
    track.filepath = str(audio)
    session.add(track)
    session.commit()
    import app.services.grid_candidate_service as service
    calls = []
    monkeypatch.setattr(service, "analyze_grid", lambda path: calls.append(path) or {
        "bpm": 120.0, "ticks": [0.3, 0.8, 1.3, 1.8], "confidence": 4.0})
    url = f"/api/tracks/{track.id}/performance-metadata"
    first = client.get(url).json()
    assert first["beat_grid"]["source"] == "analysis" and calls == [str(audio)]
    assert (first["grid_warning"] is not None) == (rekordbox == "unreadable")
    # The result is cached like an explicit analysis; the next load is immediate.
    assert client.get(url).json()["beat_grid"] == first["beat_grid"]
    assert len(calls) == 1
    assert session.exec(text("SELECT count(*) FROM track_performance_metadata")).one()[0] == 0


def test_grid_plan_leaves_out_tracks_with_a_rekordbox_grid(session, mocker, tmp_path, monkeypatch):
    from app.services.analysis_job_service import AnalysisJobService
    from domain.models.track import Track
    from infra import rekordbox_library
    from infra.rekordbox_library import CollectionIndex, CollectionValues, RekordboxLibraryUnavailable

    analysis_file = tmp_path / "ANLZ0000.DAT"
    analysis_file.write_bytes(anlz([(1, 12000, 100), (2, 12000, 600)]))
    monkeypatch.setenv("PLUMDECK_REKORDBOX_DB", str(tmp_path / "master.db"))
    for path in ("/lib/registered.mp3", "/lib/no-anlz.mp3", "/lib/unregistered.mp3"):
        session.add(Track(filepath=path, title="Song", artist="Artist", bpm=120.0, duration=180.0))
    session.commit()
    mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex({
        "/lib/registered.mp3": CollectionValues(120.0, "Am", str(analysis_file)),
        "/lib/no-anlz.mp3": CollectionValues(120.0, "Am", str(tmp_path / "missing.DAT")),
    }))
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    session.rollback()
    plan = service.plan(features=["rhythm"])
    assert (plan["rekordbox_grid_tracks"], plan["selected_tracks"]) == (1, 2)
    assert "/lib/registered.mp3" not in {item["filepath"] for item in plan["sample"]}
    # Unreadable rekordbox: nothing is left out.
    mocker.patch.object(rekordbox_library, "collection_index", side_effect=RekordboxLibraryUnavailable("x"))
    assert service.plan(features=["rhythm"])["selected_tracks"] == 3


def test_analysis_file_resolves_share_relative_paths(tmp_path):
    database = tmp_path / "master.db"
    stored = tmp_path / "share" / "PIONEER" / "USBANLZ" / "ANLZ0000.DAT"
    stored.parent.mkdir(parents=True)
    stored.write_bytes(b"PMAI")
    assert rekordbox_grid.analysis_file("/PIONEER/USBANLZ/ANLZ0000.DAT", database) == stored
    assert rekordbox_grid.analysis_file("/PIONEER/USBANLZ/missing.DAT", database) is None


@pytest.mark.parametrize("contents", [b"PMAI", anlz([]), struct.pack(">4sII", b"PMAI", 12, 12)])
def test_grid_plan_includes_tracks_when_anlz_has_no_usable_pqtz(session, mocker, tmp_path, monkeypatch, contents):
    from app.services.analysis_job_service import AnalysisJobService
    from domain.models.track import Track
    from infra import rekordbox_library
    from infra.rekordbox_library import CollectionIndex, CollectionValues

    path = tmp_path / "ANLZ0000.DAT"
    path.write_bytes(contents)
    database = tmp_path / "master.db"
    monkeypatch.setenv("PLUMDECK_REKORDBOX_DB", str(database))
    session.add(Track(filepath="/lib/registered.mp3", title="Song", artist="Artist", bpm=120.0, duration=180.0))
    session.commit()
    mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex({
        "/lib/registered.mp3": CollectionValues(120.0, "Am", str(path)),
    }))
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    session.rollback()
    plan = service.plan(features=["rhythm"])
    assert (plan["rekordbox_grid_tracks"], plan["selected_tracks"]) == (0, 1)
    assert not rekordbox_grid.has_usable_grid(str(path), database)


def test_grid_presence_handles_file_becoming_unreadable(tmp_path, monkeypatch):
    path = tmp_path / "ANLZ0000.DAT"
    path.write_bytes(anlz([(1, 12000, 100), (2, 12000, 600)]))
    assert rekordbox_grid.has_usable_grid(str(path), tmp_path / "master.db")
    def unreadable(_):
        raise PermissionError("offline")
    monkeypatch.setattr(rekordbox_grid, "_read_anlz", unreadable)
    assert not rekordbox_grid.has_usable_grid(str(path), tmp_path / "master.db")


def test_grid_presence_reads_structure_without_full_parse(tmp_path):
    # A library-wide plan only checks that PQTZ holds two beats or more; the
    # values are validated when the deck loads the grid.
    path = tmp_path / "ANLZ0000.DAT"
    path.write_bytes(anlz([(1, 12000, 600), (2, 12000, 100)]))
    assert rekordbox_grid.has_usable_grid(str(path), tmp_path / "master.db")
    with pytest.raises(rekordbox_grid.RekordboxGridError):
        rekordbox_grid.read_grid(path)
    path.write_bytes(anlz([(1, 12000, 100)]))
    assert not rekordbox_grid.has_usable_grid(str(path), tmp_path / "master.db")
    assert rekordbox_grid.read_grid(path) is None


def test_invalid_stored_playback_grid_is_reported_without_failing_metadata_get(client, session, monkeypatch):
    from domain.services.analysis.beat_grid import VERSION
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    track = _track(session)
    row = TrackAnalysis(track_id=track.id, features_extra_json=json.dumps({
        "playback_grid_version": VERSION, "playback_grid": {"bpm": -1, "first_beat_ms": 100},
    }))
    session.add(row)
    session.commit()
    response = client.get(f"/api/tracks/{track.id}/performance-metadata")
    assert response.status_code == 200
    assert response.json()["beat_grid"] is None
    assert "invalid playback grid" in response.json()["grid_warning"]


def test_failed_on_demand_analysis_is_not_repeated_until_the_file_changes(client, session, monkeypatch, tmp_path):
    import os
    import app.services.grid_candidate_service as service
    from app.services.analysis_coordinator import analysis_coordinator
    from domain.services.analysis.rhythm_grid import GridAnalysisError
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    monkeypatch.setattr(service, "_failed_on_demand", {})
    track = _track(session)
    audio = tmp_path / "undecodable.wav"
    audio.write_bytes(b"not audio")
    track.filepath = str(audio)
    session.add(track)
    session.commit()
    calls = []

    def fail(path):
        calls.append(path)
        raise GridAnalysisError("Grid analysis failed: cannot decode")
    monkeypatch.setattr(service, "analyze_grid", fail)
    url = f"/api/tracks/{track.id}/performance-metadata"
    for _ in range(3):  # deck load, then the editor's re-GETs after cue saves
        body = client.get(url).json()
        assert body["beat_grid"] is None and "cannot decode" in body["grid_warning"]
    assert len(calls) == 1
    assert analysis_coordinator.owner is None  # the slot was released

    stat = audio.stat()
    os.utime(audio, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000_000))
    client.get(url)
    assert len(calls) == 2


def test_on_demand_analysis_waits_for_the_analysis_slot(client, session, monkeypatch, tmp_path):
    import app.services.grid_candidate_service as service
    from app.services.analysis_coordinator import analysis_coordinator
    monkeypatch.setattr(rekordbox_grid, "find_analysis", lambda _: None)
    monkeypatch.setattr(service, "_failed_on_demand", {})
    monkeypatch.setattr(service, "ON_DEMAND_WAIT_SECONDS", 0.1)
    track = _track(session)
    audio = tmp_path / "track.wav"
    audio.write_bytes(b"fixture")
    track.filepath = str(audio)
    session.add(track)
    session.commit()
    calls = []
    monkeypatch.setattr(service, "analyze_grid", lambda path: calls.append(path) or {
        "bpm": 120.0, "ticks": [0.3, 0.8, 1.3, 1.8], "confidence": 4.0})
    url = f"/api/tracks/{track.id}/performance-metadata"
    token = analysis_coordinator.acquire("Import")
    try:
        body = client.get(url).json()
    finally:
        analysis_coordinator.release(token)
    # Busy is not a failure: no analysis ran and the next load analyzes.
    assert body["beat_grid"] is None and "Another analysis" in body["grid_warning"] and not calls
    assert client.get(url).json()["beat_grid"]["source"] == "analysis" and len(calls) == 1


def test_selected_jobs_skip_components_rekordbox_replaces(session, mocker, tmp_path, monkeypatch):
    from app.services.analysis_job_service import AnalysisJobService
    from domain.models.setting import Setting
    from domain.models.track import Track
    from infra import rekordbox_library
    from infra.rekordbox_library import CollectionIndex, CollectionValues

    analysis_file = tmp_path / "ANLZ0000.DAT"
    analysis_file.write_bytes(anlz([(1, 12000, 100), (2, 12000, 600)]))
    monkeypatch.setenv("PLUMDECK_REKORDBOX_DB", str(tmp_path / "master.db"))
    for path in ("/lib/pqtz.mp3", "/lib/bpm-only.mp3", "/lib/unregistered.mp3"):
        session.add(Track(filepath=path, title="Song", artist="Artist", bpm=120.0, duration=180.0))
    session.add(Setting(key="rekordbox_key_source", value="true"))
    session.commit()
    mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex({
        "/lib/pqtz.mp3": CollectionValues(120.0, "Am", str(analysis_file)),
        "/lib/bpm-only.mp3": CollectionValues(120.0, None, None),
    }))
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))
    session.rollback()
    _, candidates, selected, _ = service._select(None, None, ["rhythm", "key"], False, None)
    needed = {item["filepath"]: item["features"] for item in candidates}
    # PQTZ + BPM: rhythm has no reader; rekordbox key with the source on: key neither.
    assert needed == {"/lib/pqtz.mp3": [], "/lib/bpm-only.mp3": ["rhythm", "key"],
                      "/lib/unregistered.mp3": ["rhythm", "key"]}
    assert "/lib/pqtz.mp3" not in {item["filepath"] for item in selected}
    # The same rule applies when a queued item runs.
    track_id = session.exec(text("SELECT id FROM tracks WHERE filepath='/lib/pqtz.mp3'")).one()[0]
    session.rollback()
    config = {"features": ["rhythm", "key", "timbre"], "only_outdated": False}
    assert service._prepare_item({"track_id": track_id, "filepath": "/lib/pqtz.mp3"}, config) == ["timbre"]
