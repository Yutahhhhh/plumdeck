import json
import shutil
import math
import zipfile
from types import SimpleNamespace
from pathlib import Path

from fastapi.testclient import TestClient
from sqlmodel import Session

from models import Setlist, Track, TrackEmbedding
from app.services.play_import_service import PlayImportService, process_batch
from domain.services.set_duration import SetTimingError, calculate_set_duration


def _track(session: Session, path: Path, title: str, duration: float = 300, bpm: float | None = 126.25):
    path.write_bytes((title.encode("utf-8") + b"\0") * 20)
    track = Track(filepath=str(path), title=title, artist="DJ", album="", genre="House", bpm=bpm, duration=duration)
    session.add(track)
    session.flush()
    session.add(TrackEmbedding(track_id=track.id, embedding_json=json.dumps([0.1] * 200)))
    session.commit()
    session.refresh(track)
    return track


def test_setlist_entries_keep_identity_and_calculate_planned_duration(client: TestClient, session: Session, tmp_path: Path):
    a = _track(session, tmp_path / "a.wav", "A", 300)
    b = _track(session, tmp_path / "b.wav", "B", 240)
    playlist = Setlist(name="A B A", target_duration=600)
    session.add(playlist)
    session.commit()
    assert client.post(f"/api/setlists/{playlist.id}/tracks", json=[a.id, b.id, a.id]).status_code == 200
    rows = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    ids = [row["setlist_track_id"] for row in rows]
    payload = [
        {"id": a.id, "setlist_track_id": ids[2], "revision": rows[2]["revision"], "in_ms": 180000, "out_ms": 300000, "playback_rate": 1, "overlap_next_ms": 20_000},
        {"id": b.id, "setlist_track_id": ids[1], "revision": rows[1]["revision"], "in_ms": 0, "out_ms": 180000, "playback_rate": 1, "overlap_next_ms": 30_000},
        {"id": a.id, "setlist_track_id": ids[0], "revision": rows[0]["revision"], "in_ms": 0, "out_ms": 120000, "playback_rate": 1},
    ]
    assert client.post(f"/api/setlists/{playlist.id}/tracks", json=payload).status_code == 200
    updated = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    assert [row["setlist_track_id"] for row in updated] == list(reversed(ids))
    # Moving changes adjacency, so old overlap values are deliberately cleared.
    assert client.get(f"/api/setlists/{playlist.id}/duration").json()["planned_duration_ms"] == 420_000


def test_set_duration_specification_vectors():
    assert calculate_set_duration([])["planned_duration_ms"] == 0
    assert calculate_set_duration([{"duration": 60}])["planned_duration_ms"] == 60_000
    assert calculate_set_duration([{"duration": None}])["planned_duration_ms"] == 120_000
    assert calculate_set_duration([
        {"duration": 300, "overlap_next_ms": 30_000}, {"duration": 240},
    ])["planned_duration_ms"] == 210_000
    base = [
        {"duration": 300, "in_ms": 60_000, "out_ms": 300_000, "playback_rate": 1.25,
         "overlap_next_ms": 32_000},
        {"duration": 240, "in_ms": 0, "out_ms": 240_000, "playback_rate": 1},
    ]
    assert calculate_set_duration(base)["planned_duration_ms"] == 400_000
    base[0]["extra_duration_ms"] = 10_000
    assert calculate_set_duration(base)["planned_duration_ms"] == 410_000
    repeated = [
        {"duration": 300, "in_ms": 0, "out_ms": 120_000, "overlap_next_ms": 20_000},
        {"duration": 240, "in_ms": 0, "out_ms": 180_000, "overlap_next_ms": 30_000},
        {"duration": 300, "in_ms": 180_000, "out_ms": 300_000},
    ]
    assert calculate_set_duration(repeated)["planned_duration_ms"] == 370_000
    for invalid in (
        [{"duration": 10, "playback_rate": 0}],
        [{"duration": 10, "playback_rate": -1}],
        [{"duration": 10, "playback_rate": math.nan}],
        [{"duration": 10, "in_ms": 5000, "out_ms": 5000}],
        [{"duration": 10, "overlap_next_ms": 11_000}, {"duration": 12}],
    ):
        with __import__("pytest").raises(SetTimingError):
            calculate_set_duration(invalid)


def test_track_versions_and_single_entry_swap(client: TestClient, session: Session, tmp_path: Path):
    original = _track(session, tmp_path / "original.wav", "Song", 300)
    extended = _track(session, tmp_path / "extended.wav", "Song Extended", 420)
    playlist = Setlist(name="Versions")
    session.add(playlist)
    session.commit()
    client.post(f"/api/setlists/{playlist.id}/tracks", json=[original.id, original.id])
    created = client.post("/api/workflows/version-groups", json={"track_ids": [original.id, extended.id], "name": "Song"})
    assert created.status_code == 200
    rows = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    swapped = client.patch(f"/api/workflows/setlist-entries/{rows[1]['setlist_track_id']}/version",
                           json={"new_track_id": extended.id, "revision": rows[1]["revision"]})
    assert swapped.status_code == 200
    final = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    assert [row["id"] for row in final] == [original.id, extended.id]


def test_legacy_track_id_payload_reuses_each_existing_occurrence(client: TestClient, session: Session, tmp_path: Path):
    a = _track(session, tmp_path / "repeat.wav", "Repeat")
    b = _track(session, tmp_path / "middle.wav", "Middle")
    playlist = Setlist(name="Stable IDs")
    session.add(playlist); session.commit()
    client.post(f"/api/setlists/{playlist.id}/tracks", json=[a.id, b.id, a.id])
    before = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    response = client.post(f"/api/setlists/{playlist.id}/tracks", json=[a.id, a.id, b.id])
    assert response.status_code == 200, response.text
    after = client.get(f"/api/setlists/{playlist.id}/tracks").json()
    assert [row["setlist_track_id"] for row in after] == [before[0]["setlist_track_id"], before[2]["setlist_track_id"], before[1]["setlist_track_id"]]


def test_media_repair_uses_hash_and_keeps_track_id(client: TestClient, session: Session, tmp_path: Path):
    old = tmp_path / "old"
    new = tmp_path / "new"
    old.mkdir(); new.mkdir()
    source = old / "日本語 song.wav"
    track = _track(session, source, "Move")
    assert client.post("/api/workflows/media/diagnose", json={"track_ids": [track.id]}).status_code == 200
    destination = new / source.name
    shutil.move(source, destination)
    plan = client.post("/api/workflows/media/diagnose", json={"track_ids": [track.id], "old_root": str(old), "new_root": str(new)}).json()
    assert plan["items"][0]["candidate_state"] == "confirmed"
    applied = client.post(f"/api/workflows/media/repairs/{plan['id']}/apply", json={"selections": {str(track.id): str(destination)}})
    assert applied.status_code == 200
    session.expire_all()
    assert session.get(Track, track.id).filepath == str(destination)


def test_backup_manifest_and_hash_validation(client: TestClient, session: Session, tmp_path: Path):
    _track(session, tmp_path / "audio.wav", "Backup")
    sampler = tmp_path / "sampler.wav"
    sampler.write_bytes(b"sampler")
    destination = tmp_path / "complete.plumdeck-backup"
    response = client.post("/api/workflows/backups", json={
        "destination": str(destination), "include_media": True, "include_recordings": False,
        "ui_settings": {
            "plumdeck.deckCount": "4", "secret.token": "excluded",
            "plumdeck.sampler.paths": json.dumps([str(sampler)]),
        },
    })
    assert response.status_code == 200, response.text
    manifest = client.post("/api/workflows/restore/inspect", json={"path": str(destination)})
    assert manifest.status_code == 200, manifest.text
    data = manifest.json()
    assert data["schema_version"] == 5
    assert data["table_counts"]["track_version_groups"] == 0
    assert data["included"]["media"] is True
    assert any(item["category"] == "sampler" and item["source_path"] == str(sampler) for item in data["files"])


def test_backup_restore_round_trip_keeps_workflow_tables(client: TestClient, session: Session, tmp_path: Path):
    track = _track(session, tmp_path / "roundtrip.wav", "Before backup")
    track_id = track.id
    session.exec(__import__("sqlalchemy").text(
        "INSERT INTO audio_presets (id,name,config_json) VALUES ('route','Club',:config)"),
        params={"config": json.dumps({"sample_rate": 44100, "buffer_size": 256, "master": {"channels": [0, 1]}})},
    )
    session.commit()
    destination = tmp_path / "roundtrip.plumdeck-backup"
    made = client.post("/api/workflows/backups", json={
        "destination": str(destination), "include_media": False, "include_recordings": False,
        "ui_settings": {"plumdeck.deckCount": "4", "plumdeck.secret.token": "must-not-leak"},
    })
    assert made.status_code == 200, made.text
    with zipfile.ZipFile(destination) as archive:
        assert "must-not-leak" not in archive.read("ui-settings.json").decode()
    session.exec(__import__("sqlalchemy").text("UPDATE tracks SET title='After backup' WHERE id=:id"), params={"id": track.id})
    session.exec(__import__("sqlalchemy").text("DELETE FROM audio_presets WHERE id='route'"))
    session.commit(); session.close()
    restored = client.post("/api/workflows/restore/apply", json={"path": str(destination), "confirmed": True})
    assert restored.status_code == 200, restored.text
    assert restored.json()["ui_settings"] == {"plumdeck.deckCount": "4"}
    import infra.database.connection as db_connection
    with Session(db_connection.engine) as verification:
        assert verification.get(Track, track_id).title == "Before backup"
        assert verification.exec(__import__("sqlalchemy").text("SELECT name FROM audio_presets WHERE id='route'" )).one()[0] == "Club"


def test_rekordbox_handoff_preserves_bpm_uri_and_repeated_entry(client: TestClient, session: Session, tmp_path: Path):
    track = _track(session, tmp_path / "日本語 #%&.wav", "XML", bpm=126.25)
    playlist = Setlist(name="A & A")
    session.add(playlist); session.commit()
    client.post(f"/api/setlists/{playlist.id}/tracks", json=[track.id, track.id])
    result = client.post("/api/workflows/usb/handoffs", json={"setlist_id": playlist.id})
    assert result.status_code == 200, result.text
    xml = Path(result.json()["xml_path"]).read_text(encoding="utf-8")
    assert 'AverageBpm="126.25"' in xml
    assert "12625" not in xml
    assert xml.count("<TRACK Key=") == 2
    assert "%23" in xml and "%25" in xml


def test_usb_handoff_becomes_stale_and_cannot_be_verified(client: TestClient, session: Session, tmp_path: Path):
    audio = tmp_path / "stale.wav"
    track = _track(session, audio, "Original")
    playlist = Setlist(name="Snapshot")
    session.add(playlist); session.commit()
    client.post(f"/api/setlists/{playlist.id}/tracks", json=[track.id])
    created = client.post("/api/workflows/usb/handoffs", json={"setlist_id": playlist.id})
    assert created.status_code == 200, created.text
    export_id = created.json()["id"]
    audio.write_bytes(b"changed after export")

    listed = client.get("/api/workflows/usb/handoffs")
    assert listed.status_code == 200, listed.text
    item = next(row for row in listed.json() if row["id"] == export_id)
    assert item["state"] == "stale"
    assert item["stale"] is True
    verified = client.post(f"/api/workflows/usb/handoffs/{export_id}/verify", json={
        "level": "user_rekordbox_check", "checks": ["playlist imported"],
    })
    assert verified.status_code == 422


def test_usb_inventory_schema_matches_persistent_adapter(session: Session):
    columns = {row[0] for row in session.exec(__import__("sqlalchemy").text("DESCRIBE usb_devices")).all()}
    assert {
        "id", "device_identifier", "volume_uuid", "label", "mount_path", "filesystem",
        "capacity_bytes", "free_bytes", "read_only", "connected", "last_seen_at",
    } <= columns


def test_audio_profile_validation_and_ddj400_builtin(client: TestClient):
    invalid = client.put("/api/workflows/audio-presets", json={"name": "bad", "config": {"master": {"channels": [0, 0]}}})
    assert invalid.status_code == 409
    profiles = client.get("/api/workflows/controller-profiles").json()
    ddj = next(profile for profile in profiles if profile["adapterId"] == "ddj400")
    assert any(binding["actionId"] == "deck.play" for binding in ddj["bindings"])


def test_manual_recording_timeline_is_revisioned(client: TestClient, session: Session, tmp_path: Path):
    audio = tmp_path / "mix.wav"; audio.write_bytes(b"RIFF" + b"\0" * 100)
    row = session.exec(__import__("sqlalchemy").text("""
        INSERT INTO recordings (recording_key,filepath,started_at,duration_ms,status,source)
        VALUES ('mix',:path,CURRENT_TIMESTAMP,60000,'completed','external') RETURNING id
    """), params={"path": str(audio)}).one()
    session.commit()
    recording_id = int(row[0])
    saved = client.put(f"/api/workflows/recordings/{recording_id}/timeline", json={"revision": 1, "segments": [
        {"start_ms": 12000, "title_snapshot": "Song", "artist_snapshot": "Artist"}
    ]})
    assert saved.status_code == 200, saved.text
    text_result = client.get(f"/api/workflows/recordings/{recording_id}/tracklist.txt")
    assert "00:00:12 Artist - Song" in text_result.text


def test_external_recording_uses_probed_duration(client: TestClient, session: Session, tmp_path: Path, mocker):
    audio = tmp_path / "external.wav"
    audio.write_bytes(b"RIFF" + b"\0" * 100)
    mocker.patch("api.routers.workflows.MutagenFile", return_value=SimpleNamespace(info=SimpleNamespace(length=12.3456)))
    response = client.post("/api/workflows/recordings/external", json={
        "filepath": str(audio), "title": "Outside", "artist": "DJ", "duration_ms": 999,
    })
    assert response.status_code == 200, response.text
    assert response.json()["duration_ms"] == 12_346
    row = session.exec(__import__("sqlalchemy").text("SELECT duration_ms,source FROM recordings WHERE id=:id"),
                       params={"id": response.json()["id"]}).one()
    assert tuple(row) == (12_346, "external")


def test_play_import_is_idempotent_and_preserves_source(client: TestClient, session: Session, tmp_path: Path):
    audio = tmp_path / "already.wav"
    track = _track(session, audio, "Already")
    before = audio.read_bytes()
    playlist = Setlist(name="Drop target")
    session.add(playlist); session.commit()
    service = PlayImportService(session)
    created = service.create("same-request", "local_playlist", playlist.id, [str(audio)], "native_file_drop")
    assert created["origin"] == "native_file_drop"
    assert service.create("same-request", "local_playlist", playlist.id, [str(audio)])["id"] == created["id"]
    session.rollback()  # End this connection's read snapshot before the worker connection commits.
    process_batch(created["id"])
    session.expire_all()
    result = service.get(created["id"])
    assert result["state"] == "completed"
    assert result["items"][0]["track_id"] == track.id
    assert audio.read_bytes() == before
    count = session.exec(__import__("sqlalchemy").text(
        "SELECT count(*) FROM setlist_tracks WHERE setlist_id=:id"), params={"id": playlist.id}).one()[0]
    assert count == 1


def test_rekordbox_xml_writes_the_saved_beat_grid(tmp_path: Path):
    from xml.etree import ElementTree as ET
    from app.services.workflow_service import UsbHandoffService, _tempo_markers

    def entry(track_id, grid):
        return {"track_id": track_id, "title": f"T{track_id}", "artist": "A", "filepath": str(tmp_path / f"{track_id}.wav"),
                "bpm": 126.0, "duration": 300, "beat_grid_json": json.dumps(grid) if grid else None}

    constant = {"bpm": 128.0, "first_beat_ms": 250.0, "source": "analysis"}
    rekordbox = {"bpm": 120.0, "first_beat_ms": 100.0, "beat_times_ms": [100.0, 600.0, 1100.0],
                 "beat_numbers": [3, 4, 1], "source": "rekordbox"}
    xml, _ = UsbHandoffService(None)._xml({"setlist": {"name": "Grid"}, "entries": [
        entry(1, constant), entry(2, rekordbox), entry(3, None)]})
    tracks = {t.get("Name"): t.findall("TEMPO") for t in ET.fromstring(xml).find("COLLECTION")}

    assert [(t.get("Inizio"), t.get("Bpm"), t.get("Battito")) for t in tracks["T1"]] == [("0.250000", "128.00", "1")]
    assert [(t.get("Inizio"), t.get("Bpm"), t.get("Battito")) for t in tracks["T2"]] == [
        ("0.100000", "120.00", "3"), ("0.600000", "120.00", "4"), ("1.100000", "120.00", "1")]
    # Without a saved grid the track BPM is still handed over.
    assert [(t.get("Inizio"), t.get("Bpm")) for t in tracks["T3"]] == [("0.000000", "126.00")]
    # A constant grid that starts before 0 moves to its first audible beat.
    assert _tempo_markers({"bpm": 120.0, "first_beat_ms": -100.0}) == [(400.0, 120.0, 2, 4)]
    assert _tempo_markers({"bpm": "x", "first_beat_ms": 0}) == []
