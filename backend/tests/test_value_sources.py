"""BPM/key sources: manual > rekordbox > own, with rekordbox read at save time."""
import asyncio
import json
import sqlite3
import unicodedata

import pytest
from sqlmodel import select

from app.services.analysis_job_service import AnalysisJobService
from app.services.csv_app_service import CsvAppService
from api.schemas.settings import CsvImportRow, ImportExecuteRequest, MetadataImportExecuteRequest
from domain.models.setting import Setting
from domain.models.track import Track, TrackAnalysis
from domain.services import value_sources
from domain.services.analysis.constants import EMBEDDING_MODEL
from domain.services.ingestion_domain_service import IngestionDomainService
from infra import rekordbox_library
from infra.rekordbox_library import CollectionIndex, CollectionValues, RekordboxLibraryUnavailable
from infra.repositories.ingestion_repository import IngestionRepository
from infra.repositories.track_repository import CAMELOT_ORDER, apply_sort
from utils.audio_math import CAMELOT_TO_KEY, key_name

PATH = "/lib/registered.mp3"
GRID = {"bpm": 64.0, "first_beat_ms": 250.0, "beat_times_ms": None, "source": "analysis"}

# The four rekordbox answers a save can get.
CASES = {
    "bpm": CollectionValues(128.0, "Fm", None),
    "bpm_zero": CollectionValues(None, "Fm", None),
    "unregistered": None,
    "error": RekordboxLibraryUnavailable("master.db is locked"),
}


def use_rekordbox(mocker, case, path=PATH):
    answer = CASES[case] if isinstance(case, str) else case
    if isinstance(answer, Exception):
        return mocker.patch.object(rekordbox_library, "collection_index", side_effect=answer)
    entries = {} if answer is None else {path: answer}
    return mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex(entries))


def use_rekordbox_keys(session, enabled=True):
    session.merge(Setting(key=value_sources.KEY_SOURCE_SETTING, value="true" if enabled else "false"))
    session.commit()


def analysis_result(filepath=PATH, **values):
    result = {
        "filepath": filepath, "title": "Song", "artist": "Artist", "duration": 180.0,
        "bpm": 64.0, "key": "A minor", "scale": "minor", "energy": 0.4, "danceability": 0.5,
        "brightness": 0.3, "noisiness": 0.2, "analysis_level": "full",
        "embedding": [0.1] * 200, "embedding_model": EMBEDDING_MODEL,
        "features_extra": {
            "analysis_components": {"rhythm": "rhythm-attacks-v4", "key": "key-v1", "timbre": "timbre-v1"},
            "playback_grid": GRID, "playback_grid_version": 2, "beat_positions": [0.25, 1.19, 2.13],
        },
    }
    result.update(values)
    return result


def saved(session, track_id):
    session.rollback()  # end the read snapshot so other sessions' commits are visible
    session.expire_all()
    return session.get(Track, track_id), session.get(TrackAnalysis, track_id).features_extra


@pytest.mark.parametrize("raw, stored", [
    ("Fm", "F minor"), ("F#m", "F# minor"), ("Dbm", "C# minor"), ("G#m", "Ab minor"),
    ("Abm", "Ab minor"), ("Db", "C# major"), ("Gb", "F# major"), ("B", "B major"),
    ("4A", "F minor"), ("11b", "A major"), ("F minor", "F minor"), ("Eb Major", "Eb major"),
    ("www.FabricaMP3.net", None), ("", None), (None, None),
])
def test_key_name_spells_rekordbox_keys_as_stored(raw, stored):
    assert key_name(raw) == stored


def test_stored_key_spelling_sorts_in_camelot_order(session):
    for code, name in CAMELOT_TO_KEY.items():
        assert CAMELOT_ORDER[name.lower()] == CAMELOT_ORDER[code.lower()]
    for index, raw in enumerate(["Am", "4B", "Fm", "Dbm"]):
        session.add(Track(filepath=f"/lib/{index}.mp3", title=raw, artist="A", key=key_name(raw)))
    session.commit()
    ordered = [track.key for track in session.exec(apply_sort(select(Track), "key", "asc")).all()]
    assert ordered == ["F minor", "Ab major", "A minor", "C# minor"]  # 4A, 4B, 8A, 12A


@pytest.mark.parametrize("case, keys, expected", [
    ("bpm", True, value_sources.RekordboxValues(True, 128.0, "F minor")),
    ("bpm", False, value_sources.RekordboxValues(True, 128.0, None, keys=False)),
    ("bpm_zero", True, value_sources.RekordboxValues(True, None, "F minor")),
    ("unregistered", True, value_sources.RekordboxValues(True, None, None)),
    ("error", True, value_sources.RekordboxValues(False, None, None)),
])
def test_lookup_answers_for_every_rekordbox_state(mocker, case, keys, expected):
    use_rekordbox(mocker, case)
    assert value_sources.lookup(PATH, keys=keys) == expected


def test_choose_applies_manual_then_rekordbox_then_own():
    choose, values = value_sources.choose, value_sources.RekordboxValues
    readable, offline = values(True, 128.0, "F minor"), values(False)
    assert choose("bpm", 64.0, "own", 125.0, "manual", readable) == (125.0, "manual")
    assert choose("bpm", 64.0, "own", 120.0, "own", readable) == (128.0, "rekordbox")
    assert choose("key", "A minor", "own", None, None, readable) == ("F minor", "rekordbox")
    # A value the caller took from rekordbox stays rekordbox even if master.db went away.
    assert choose("bpm", 128.0, "rekordbox", None, None, offline) == (128.0, "rekordbox")
    assert choose("bpm", 64.0, "own", 128.0, "rekordbox", offline) == (128.0, "rekordbox")
    # Readable but rekordbox no longer has a tempo (left the collection, another
    # path spelling, BPM 0): the synced value still outranks an estimate.
    assert choose("bpm", 64.0, "own", 128.0, "rekordbox", values(True)) == (128.0, "rekordbox")
    assert choose("bpm", 64.0, "own", 120.0, "own", values(True)) == (64.0, "own")
    assert choose("bpm", 0.0, "own", 120.0, "own", values(True)) == (120.0, "own")
    assert choose("bpm", 0.0, "own", None, None, offline) == (None, None)


@pytest.mark.parametrize("case, bpm, source", [
    ("bpm", 128.0, "rekordbox"), ("bpm_zero", 64.0, "own"),
    ("unregistered", 64.0, "own"), ("error", 64.0, "own"),
])
def test_save_takes_bpm_from_rekordbox_when_it_has_one(session, mocker, case, bpm, source):
    use_rekordbox(mocker, case)
    result = analysis_result()
    track_id = IngestionRepository()._prepare_track_models(session, result)
    session.commit()

    track, extra = saved(session, track_id)
    assert track.bpm == bpm
    assert extra["analysis_components"]["sources"]["bpm"] == source
    assert extra.get("analysis_bpm") == (64.0 if source == "rekordbox" else None)
    assert result["bpm"] == 64.0  # the caller's result is not modified
    # The key flag is off by default: own detection stays.
    assert (track.key, extra["analysis_components"]["sources"]["key"]) == ("A minor", "own")


def test_save_keeps_own_key_when_the_key_flag_is_off(session, mocker):
    use_rekordbox_keys(session, enabled=False)
    use_rekordbox(mocker, "bpm")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.bpm, track.key, extra["analysis_components"]["sources"]["key"]) == (128.0, "A minor", "own")


@pytest.mark.parametrize("case, key, scale, source", [
    ("bpm", "F minor", "minor", "rekordbox"), ("bpm_zero", "F minor", "minor", "rekordbox"),
    ("unregistered", "A minor", "minor", "own"), ("error", "A minor", "minor", "own"),
])
def test_save_takes_key_from_rekordbox_only_when_enabled(session, mocker, case, key, scale, source):
    use_rekordbox_keys(session)
    use_rekordbox(mocker, case)
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.key, track.scale, extra["analysis_components"]["sources"]["key"]) == (key, scale, source)
    # The replaced own key is kept, like analysis_bpm.
    assert extra.get("analysis_key") == ("A minor" if source == "rekordbox" else None)


def test_choose_restores_own_key_only_when_the_key_source_is_off():
    choose, values = value_sources.choose, value_sources.RekordboxValues
    off, offline = values(True, 128.0, None, keys=False), values(False)
    assert choose("key", "A minor", "own", "F minor", "rekordbox", off, "E minor") == ("A minor", "own")
    assert choose("key", "", "own", "F minor", "rekordbox", off, "E minor") == ("E minor", "own")
    # Enabled but master.db is offline: the synced key stays.
    assert choose("key", "A minor", "own", "F minor", "rekordbox", offline, "E minor") == ("F minor", "rekordbox")
    # BPM has no off switch; manual still wins over everything.
    assert choose("bpm", 64.0, "own", 128.0, "rekordbox", values(False, keys=False), 63.0) == (128.0, "rekordbox")
    assert choose("key", "A minor", "own", "G minor", "manual", off, "E minor") == ("G minor", "manual")


@pytest.mark.parametrize("case", ["bpm", "error"])
def test_turning_the_key_source_off_restores_the_own_key(session, mocker, case):
    use_rekordbox_keys(session)
    use_rekordbox(mocker, "bpm")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    assert saved(session, track_id)[0].key == "F minor"

    use_rekordbox_keys(session, enabled=False)
    use_rekordbox(mocker, case)
    IngestionRepository()._prepare_track_models(session, analysis_result(key="D minor", scale="minor"))
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.key, extra["analysis_components"]["sources"]["key"]) == ("D minor", "own")
    assert "analysis_key" not in extra


def test_key_source_off_restores_the_kept_key_when_the_save_has_none(session, mocker):
    use_rekordbox_keys(session)
    use_rekordbox(mocker, "bpm")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    use_rekordbox_keys(session, enabled=False)
    IngestionRepository()._prepare_track_models(session, analysis_result(key=""))
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.key, track.scale, extra["analysis_components"]["sources"]["key"]) == ("A minor", "minor", "own")


def test_import_without_key_detection_keeps_the_rows_own_key(session, mocker):
    use_rekordbox_keys(session)
    use_rekordbox(mocker, "bpm")
    session.add(Track(filepath=PATH, title="Song", artist="Artist", bpm=120.0, key="E minor", duration=180.0))
    session.commit()
    # Registered track: the import skipped key detection and filled rekordbox's key.
    result = analysis_result(key="F minor")
    result["features_extra"]["analysis_components"]["sources"] = {"key": "rekordbox"}
    track_id = IngestionRepository()._prepare_track_models(session, result)
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.key, extra["analysis_key"]) == ("F minor", "E minor")
    # A later save keeps it while rekordbox's key still wins.
    IngestionRepository()._prepare_track_models(session, result)
    session.commit()
    assert saved(session, track_id)[1]["analysis_key"] == "E minor"


def test_rekordbox_bpm_survives_an_offline_save_and_follows_rekordbox_later(session, mocker):
    use_rekordbox(mocker, "bpm")
    repository = IngestionRepository()
    track_id = repository._prepare_track_models(session, analysis_result())
    session.commit()

    use_rekordbox(mocker, "error")
    repository._prepare_track_models(session, analysis_result(bpm=63.0))
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.bpm, extra["analysis_components"]["sources"]["bpm"]) == (128.0, "rekordbox")
    assert extra["analysis_bpm"] == 63.0

    use_rekordbox(mocker, CollectionValues(127.5, None, None))
    repository._prepare_track_models(session, analysis_result(bpm=63.0))
    session.commit()
    assert saved(session, track_id)[0].bpm == 127.5


def test_metadata_only_save_does_not_resolve_values(session, mocker):
    track = Track(filepath=PATH, title="Song", artist="Artist", bpm=120.0, key="A minor", duration=180.0)
    session.add(track)
    session.commit()
    lookup = use_rekordbox(mocker, "bpm")
    IngestionRepository()._prepare_track_models(session, {
        "filepath": PATH, "title": "Renamed", "bpm": 120.0, "key": "A minor", "features_extra": {}})
    session.commit()
    assert saved(session, track.id)[0].bpm == 120.0
    lookup.assert_not_called()


def test_manual_csv_values_outrank_rekordbox_and_reanalysis(session, mocker):
    use_rekordbox_keys(session)
    use_rekordbox(mocker, "bpm")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    updated = CsvAppService(session).execute_metadata_import(MetadataImportExecuteRequest(updates=[
        {"data": {"filepath": PATH, "bpm": 125.0, "key": "G minor"}}]))
    assert updated == 1
    assert saved(session, track_id)[1]["analysis_components"]["sources"] == {"bpm": "manual", "key": "manual"}

    IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    track, extra = saved(session, track_id)
    assert (track.bpm, track.key) == (125.0, "G minor")
    assert extra["analysis_components"]["sources"] == {"bpm": "manual", "key": "manual"}
    assert extra["analysis_bpm"] == 64.0


def test_library_csv_corrections_are_manual_for_moved_and_new_tracks(session, mocker):
    use_rekordbox(mocker, "bpm", path="/old.mp3")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result(filepath="/old.mp3"))
    session.commit()
    assert saved(session, track_id)[1]["analysis_components"]["sources"]["bpm"] == "rekordbox"
    CsvAppService(session).execute_import(ImportExecuteRequest(
        new_tracks=[CsvImportRow(filepath="/lib/new.mp3", title="New", artist="Artist", bpm=125.0, key="G minor")],
        path_updates=[{"old_path": "/old.mp3", "new_path": PATH, "track": {"bpm": 130.0}}]))
    assert saved(session, track_id)[1]["analysis_components"]["sources"]["bpm"] == "manual"
    new_id = session.exec(select(Track.id).where(Track.filepath == "/lib/new.mp3")).one()
    assert saved(session, new_id)[1]["analysis_components"]["sources"] == {"bpm": "manual", "key": "manual"}

    # rekordbox's 128 and the re-analysed 64 no longer replace the correction.
    use_rekordbox(mocker, "bpm")
    IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    assert saved(session, track_id)[0].bpm == 130.0


def test_metadata_csv_previews_and_imports_bpm_and_key_as_manual(session, mocker):
    use_rekordbox(mocker, "bpm")
    track_id = IngestionRepository()._prepare_track_models(session, analysis_result())
    session.commit()
    service = CsvAppService(session)
    exported = service.export_metadata_csv().splitlines()[0].split(",")
    assert {"bpm", "key"} <= set(exported)
    preview = service.analyze_metadata_import(f"filepath,bpm,key\n{PATH},126.5,G minor\n{PATH}x,abc,\n")
    assert len(preview.updates) == 1 and len(preview.not_found) == 1
    row = preview.updates[0]["new"]
    assert (row.bpm, row.key) == (126.5, "G minor")
    assert preview.not_found[0].bpm is None
    service.execute_metadata_import(MetadataImportExecuteRequest(updates=[{"new": row.model_dump()}]))
    track, extra = saved(session, track_id)
    assert (track.bpm, track.key) == (126.5, "G minor")
    assert extra["analysis_components"]["sources"] == {"bpm": "manual", "key": "manual"}


def _job_save(session, tmp_path, features, result):
    track_id = session.exec(select(Track.id).where(Track.filepath == PATH)).one()
    session.rollback()
    AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"))._save(
        {"track_id": track_id, "filepath": PATH}, result, features)
    return saved(session, track_id)


@pytest.mark.parametrize("stored_bpm, source, case, expected", [
    (None, None, "bpm", (128.0, "rekordbox")), (0.0, None, "bpm", (128.0, "rekordbox")),
    (125.0, "manual", "bpm", (125.0, "manual")), (120.0, "own", "unregistered", (64.0, "own")),
])
def test_grid_only_job_resolves_bpm_like_other_saves(session, mocker, tmp_path, stored_bpm, source, case, expected):
    use_rekordbox(mocker, case)
    track = Track(filepath=PATH, title="Song", artist="Artist", bpm=stored_bpm, duration=180.0)
    session.add(track)
    session.flush()
    extra = value_sources.with_sources({}, {"bpm": source}) if source else {}
    session.add(TrackAnalysis(track_id=track.id, features_extra_json=json.dumps(extra)))
    session.commit()

    track, extra = _job_save(session, tmp_path, ["rhythm"], analysis_result())
    assert (track.bpm, value_sources.stored_sources(extra).get("bpm")) == expected
    assert extra.get("analysis_bpm") == (None if expected[1] == "own" else 64.0)
    assert session.get(TrackAnalysis, track.id).beat_positions


def test_analysis_job_save_applies_the_same_rule(session, mocker, tmp_path):
    use_rekordbox_keys(session)
    session.add(Track(filepath=PATH, title="Song", artist="Artist", bpm=120.0, key="E minor", duration=180.0))
    session.commit()
    use_rekordbox(mocker, "bpm")
    result = {"bpm": 64.0, "key": "A minor", "scale": "minor", "features_extra": {
        "beat_positions": [0.5, 1.0, 1.5], "playback_grid": GRID, "playback_grid_version": 2,
        "analysis_components": {"rhythm": "rhythm-attacks-v4", "key": "key-v1"}}}

    track, extra = _job_save(session, tmp_path, ["rhythm", "key"], result)
    assert (track.bpm, track.key, track.scale) == (128.0, "F minor", "minor")
    assert extra["analysis_components"]["sources"] == {"bpm": "rekordbox", "key": "rekordbox"}
    assert (extra["analysis_bpm"], extra["analysis_key"]) == (64.0, "A minor")

    use_rekordbox(mocker, "unregistered")
    track, extra = _job_save(session, tmp_path, ["rhythm"], result)
    assert (track.bpm, extra["analysis_components"]["sources"]["bpm"]) == (128.0, "rekordbox")
    assert extra["analysis_bpm"] == 64.0


def _ingest(mocker, tmp_path, case, keys=False):
    audio = tmp_path / "registered.mp3"
    audio.write_bytes(b"audio")
    use_rekordbox(mocker, case, str(audio))
    for name in ("Session", "db_connection", "select"):
        mocker.patch(f"domain.services.ingestion_domain_service.{name}")
    mocker.patch.object(value_sources, "key_source_enabled", return_value=keys)
    calls = []

    def analyze(*args):
        calls.append(args)
        skipped = args[6] if len(args) > 6 else []
        return {"filepath": str(audio), "title": "Song", "artist": "Artist", "duration": 180.0,
                "bpm": 0.0 if "rhythm" in skipped else 64.0, "key": "" if "key" in skipped else "A minor",
                "scale": "" if "key" in skipped else "minor", "analysis_level": "full",
                "energy": .7, "danceability": .6, "brightness": .4, "noisiness": .2,
                "features_extra": {"analysis_components": {"timbre": "timbre-v1"}, "waveform_peaks": [.2, .7]}}

    mocker.patch("domain.services.ingestion_domain_service.analyze_track_file", side_effect=analyze)
    result = run_ingestion(str(audio))
    return calls[0], result


def run_ingestion(path, **options):
    async def ingest():
        return await IngestionDomainService().process_track_ingestion(
            path, True, asyncio.get_running_loop(), save_to_db=False, write_source_metadata=False, **options)
    return asyncio.run(ingest())


@pytest.mark.parametrize("case, keys, skipped, bpm, key", [
    ("bpm", True, ["rhythm", "key"], 128.0, "F minor"),
    ("bpm", False, ["rhythm"], 128.0, "A minor"),
    ("bpm_zero", True, ["key"], 64.0, "F minor"),
    ("unregistered", True, [], 64.0, "A minor"),
    ("error", True, [], 64.0, "A minor"),
])
def test_import_skips_rhythm_and_key_that_rekordbox_has(mocker, tmp_path, case, keys, skipped, bpm, key):
    args, result = _ingest(mocker, tmp_path, case, keys)
    assert (list(args[6]) if len(args) > 6 else []) == skipped
    if not skipped:
        assert len(args) == 4  # the established worker arity is unchanged
    assert (result["bpm"], result["key"]) == (bpm, key)
    sources = result["features_extra"]["analysis_components"].get("sources", {})
    assert sources == {field: "rekordbox" for field, name in (("bpm", "rhythm"), ("key", "key")) if name in skipped}


def test_light_import_keeps_its_own_grid(mocker, tmp_path):
    lookup = mocker.patch.object(value_sources, "lookup")
    audio = tmp_path / "light.mp3"
    audio.write_bytes(b"audio")
    for name in ("Session", "db_connection", "select"):
        mocker.patch(f"domain.services.ingestion_domain_service.{name}")
    mocker.patch("domain.services.ingestion_domain_service.has_completed_analysis_result", return_value=True)
    worker = mocker.patch("domain.services.ingestion_domain_service.analyze_track_file",
                          return_value={"filepath": str(audio), "bpm": 120.0, "duration": 60.0})
    run_ingestion(str(audio), analysis_profile="light")
    lookup.assert_not_called()
    assert worker.call_args.args[5] == "light" and len(worker.call_args.args) == 6


def test_collection_index_reads_bpm_key_and_analysis_path(mocker, tmp_path):
    connection = sqlite3.connect(":memory:")
    connection.executescript(
        "CREATE TABLE djmdKey (ID TEXT, ScaleName TEXT);"
        "CREATE TABLE djmdContent (FolderPath TEXT, BPM INTEGER, KeyID TEXT, AnalysisDataPath TEXT,"
        " rb_local_deleted INTEGER);")
    connection.executemany("INSERT INTO djmdKey VALUES (?, ?)", [("k1", "Fm"), ("k2", "4A")])
    decomposed = unicodedata.normalize("NFD", "/lib/café.mp3")
    connection.executemany("INSERT INTO djmdContent VALUES (?, ?, ?, ?, ?)", [
        (decomposed, 12800, "k1", "/PIONEER/USBANLZ/a/ANLZ0000.DAT", 0),
        ("/lib/unanalysed.mp3", 0, None, None, 0),
        ("/lib/deleted.mp3", 12000, "k1", None, 1),
        ("/lib/twice.mp3", 12000, "k1", None, 0), ("/lib/twice.mp3", 6000, "k2", None, 0),
    ])
    mocker.patch.object(rekordbox_library, "_connect", return_value=connection)
    mocker.patch.object(rekordbox_library.os.path, "samefile", return_value=True)
    index = rekordbox_library._CollectionValuesCache._load(tmp_path / "master.db")

    assert index.get(unicodedata.normalize("NFC", "/lib/café.mp3")) == CollectionValues(
        128.0, "Fm", "/PIONEER/USBANLZ/a/ANLZ0000.DAT")
    assert index.get("/lib/unanalysed.mp3") == CollectionValues(None, None, None)
    assert index.get("/lib/deleted.mp3") is None
    assert index.get("/lib/twice.mp3") == CollectionValues(None, None, None)
    assert index.get("/lib/other.mp3") is None
