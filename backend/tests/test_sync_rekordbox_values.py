"""scripts/sync_rekordbox_values.py: plan, then apply, the rekordbox BPM/key of existing tracks."""
import json

import pytest

from domain.models.setting import Setting
from domain.models.track import Track, TrackAnalysis
from domain.services import value_sources
from infra import rekordbox_library
from infra.rekordbox_library import CollectionIndex, CollectionValues


def _sync_script():
    import importlib.util
    from pathlib import Path
    path = Path(__file__).resolve().parents[2] / "scripts" / "sync_rekordbox_values.py"
    spec = importlib.util.spec_from_file_location("sync_rekordbox_values", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_sync_script_plans_then_applies_collection_values(mocker, tmp_path):
    from argparse import Namespace
    import duckdb
    from sqlmodel import Session, create_engine
    from infra.database.schema import init_raw_db

    sync = _sync_script()
    database = tmp_path / "library.duckdb"
    engine = create_engine(f"duckdb:///{database}")
    init_raw_db(engine)
    with Session(engine) as session:
        for path, bpm, key, extra in [
            ("/lib/octave.mp3", 64.0, "A minor", {"analysis_components": {"rhythm": "rhythm-attacks-v4"}}),
            ("/lib/manual.mp3", 125.0, "G minor", {"analysis_components": {"sources": {"bpm": "manual", "key": "manual"}}}),
            ("/lib/equal.mp3", 128.0, "F minor", {}),
            ("/lib/unregistered.mp3", 90.0, "C major", {}),
        ]:
            track = Track(filepath=path, title=path, artist="A", bpm=bpm, key=key, scale=key.split()[1])
            session.add(track)
            session.flush()
            session.add(TrackAnalysis(track_id=track.id, features_extra_json=json.dumps(extra)))
        session.add(Setting(key=value_sources.KEY_SOURCE_SETTING, value="true"))
        session.commit()
    engine.dispose()
    mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex({
        path: CollectionValues(128.0, "Fm", None) for path in ("/lib/octave.mp3", "/lib/manual.mp3", "/lib/equal.mp3")}))

    plan_path = tmp_path / "plan.json"
    sync.plan(Namespace(db=database, rekordbox_db=None, out=plan_path, fields=["bpm", "key"]))
    plan = json.loads(plan_path.read_text())
    assert [change["filepath"] for change in plan["changes"]] == ["/lib/octave.mp3"]
    assert plan["changes"][0]["bpm"] == [64.0, 128.0] and plan["changes"][0]["key"] == ["A minor", "F minor"]
    assert plan["summary"]["bpm_octave_changes"] == 1 and plan["summary"]["bpm_manual_kept"] == 1
    assert plan["summary"]["not_in_rekordbox"] == 1

    sync.apply(Namespace(db=database, plan=str(plan_path), no_backup=False))
    connection = duckdb.connect(str(database), read_only=True)
    try:
        rows = dict(connection.execute(
            "SELECT t.filepath, [t.bpm::VARCHAR, t.key, t.scale, a.features_extra_json] FROM tracks t "
            "JOIN track_analyses a ON a.track_id = t.id").fetchall())
    finally:
        connection.close()
    bpm, key, scale, extra = rows["/lib/octave.mp3"]
    assert (float(bpm), key, scale) == (128.0, "F minor", "minor")
    extra = json.loads(extra)
    assert extra["analysis_components"] == {"rhythm": "rhythm-attacks-v4",
                                            "sources": {"bpm": "rekordbox", "key": "rekordbox"}}
    assert extra["analysis_bpm"] == 64.0
    assert rows["/lib/manual.mp3"][:2] == ["125.0", "G minor"]
    assert len(list(tmp_path.glob("library.duckdb.before-rekordbox-sync-*"))) == 1
    # A plan whose old values no longer match is refused as a whole.
    with pytest.raises(RuntimeError, match="changed since planning"):
        sync.apply(Namespace(db=database, plan=str(plan_path), no_backup=True))


@pytest.mark.parametrize("stored", [None, "false"])
def test_plan_refuses_keys_while_the_key_source_is_off(mocker, tmp_path, monkeypatch, stored):
    from argparse import Namespace
    import duckdb

    monkeypatch.setattr(value_sources, "KEY_SOURCE_DEFAULT", False)
    sync = _sync_script()
    database = tmp_path / "library.duckdb"
    with duckdb.connect(str(database)) as connection:
        connection.execute("CREATE TABLE tracks (id INTEGER PRIMARY KEY, filepath VARCHAR, bpm DOUBLE, key VARCHAR)")
        connection.execute("CREATE TABLE track_analyses (track_id INTEGER PRIMARY KEY, features_extra_json VARCHAR)")
        connection.execute("CREATE TABLE settings (key VARCHAR PRIMARY KEY, value VARCHAR)")
        connection.execute("INSERT INTO tracks VALUES (1, '/one.mp3', 64.0, 'A minor')")
        if stored is not None:
            connection.execute("INSERT INTO settings VALUES (?, ?)", [value_sources.KEY_SOURCE_SETTING, stored])
    mocker.patch.object(rekordbox_library, "collection_index", return_value=CollectionIndex({
        "/one.mp3": CollectionValues(128.0, "Fm", None)}))
    plan_path = tmp_path / "plan.json"
    with pytest.raises(SystemExit, match="rekordbox_key_source"):
        sync.plan(Namespace(db=database, rekordbox_db=None, out=plan_path, fields=["bpm", "key"]))
    assert not plan_path.exists()
    sync.plan(Namespace(db=database, rekordbox_db=None, out=plan_path, fields=["bpm"]))
    assert json.loads(plan_path.read_text())["changes"] == [
        {"track_id": 1, "filepath": "/one.mp3", "bpm": [64.0, 128.0]}]


def _planned_library(tmp_path, *, analysis=True):
    from argparse import Namespace
    import duckdb

    sync = _sync_script()
    database = tmp_path / "fixture.duckdb"
    with duckdb.connect(str(database)) as connection:
        connection.execute("CREATE TABLE tracks (id INTEGER PRIMARY KEY, filepath VARCHAR, bpm DOUBLE, key VARCHAR, scale VARCHAR)")
        connection.execute("CREATE TABLE track_analyses (track_id INTEGER PRIMARY KEY, features_extra_json VARCHAR)")
        connection.executemany("INSERT INTO tracks VALUES (?, ?, ?, ?, ?)", [
            (1, "/one.mp3", 64.0, "A minor", "minor"),
            (2, "/two.mp3", 65.0, "G minor", "minor"),
        ])
        if analysis:
            connection.executemany("INSERT INTO track_analyses VALUES (?, ?)", [(1, "{}"), (2, "{}")])
        rows = connection.execute("SELECT t.id, t.filepath, t.bpm, t.key, a.features_extra_json "
                                  "FROM tracks t LEFT JOIN track_analyses a ON a.track_id=t.id ORDER BY t.id").fetchall()
    index = CollectionIndex({path: CollectionValues(128.0, "Fm", None) for path in ("/one.mp3", "/two.mp3")})
    changes, summary = sync.build_plan(rows, index, ["bpm", "key"])
    payload = {"version": sync.PLAN_VERSION, "database": str(database), "fields": ["bpm", "key"],
               "changes": changes, "summary": summary}
    plan = tmp_path / "plan.json"
    plan.write_text(json.dumps(payload))
    return sync, Namespace(db=database, plan=plan, no_backup=True), payload


def _values(database):
    import duckdb
    with duckdb.connect(str(database), read_only=True) as connection:
        return connection.execute("SELECT id, bpm, key FROM tracks ORDER BY id").fetchall()


def test_apply_rechecks_manual_source_and_rolls_back_earlier_tracks(tmp_path):
    import duckdb
    sync, args, _ = _planned_library(tmp_path)
    with duckdb.connect(str(args.db)) as connection:
        connection.execute("UPDATE track_analyses SET features_extra_json=? WHERE track_id=2",
                           [json.dumps({"analysis_components": {"sources": {"key": "manual"}}})])
    with pytest.raises(RuntimeError, match="became manual"):
        sync.apply(args)
    assert _values(args.db) == [(1, 64.0, "A minor"), (2, 65.0, "G minor")]
    with duckdb.connect(str(args.db), read_only=True) as connection:
        assert connection.execute("SELECT features_extra_json FROM track_analyses WHERE track_id=1").fetchone()[0] == "{}"


def test_apply_records_sources_for_tracks_without_analysis_rows(tmp_path):
    import duckdb
    sync, args, _ = _planned_library(tmp_path, analysis=False)
    sync.apply(args)
    with duckdb.connect(str(args.db), read_only=True) as connection:
        extras = connection.execute("SELECT features_extra_json FROM track_analyses ORDER BY track_id").fetchall()
    assert len(extras) == 2
    for index, (raw,) in enumerate(extras):
        extra = json.loads(raw)
        assert extra["analysis_components"]["sources"] == {"bpm": "rekordbox", "key": "rekordbox"}
        assert extra["analysis_bpm"] == 64.0 + index


@pytest.mark.parametrize("raw", ["{bad json", "[]", '{"analysis_components": []}',
                                  '{"analysis_components": {"sources": []}}'])
def test_sync_refuses_unreadable_provenance_at_plan_and_apply(tmp_path, raw):
    import duckdb
    sync, args, _ = _planned_library(tmp_path)
    index = CollectionIndex({"/one.mp3": CollectionValues(128.0, "Fm", None)})
    changes, summary = sync.build_plan([(1, "/one.mp3", 64.0, "A minor", raw)], index, ["bpm", "key"])
    assert changes == [] and summary["unreadable_provenance_kept"] == 1
    with duckdb.connect(str(args.db)) as connection:
        connection.execute("UPDATE track_analyses SET features_extra_json=? WHERE track_id=2", [raw])
    with pytest.raises(ValueError, match="provenance"):
        sync.apply(args)
    assert _values(args.db) == [(1, 64.0, "A minor"), (2, 65.0, "G minor")]


def test_apply_refuses_a_plan_for_another_library_before_creating_database(tmp_path):
    sync, args, _ = _planned_library(tmp_path)
    args.db = tmp_path / "other.duckdb"
    with pytest.raises(ValueError, match="different library"):
        sync.apply(args)
    assert not args.db.exists()


@pytest.mark.parametrize("mutation", [
    lambda plan: plan.update(fields=["bpm"]),
    lambda plan: plan.update(fields=["bpm", "key", "key"]),
    lambda plan: plan["changes"].append(dict(plan["changes"][0])),
    lambda plan: plan["changes"][0].update(bpm=[64.0, 0.0]),
    lambda plan: plan["changes"][0].update(bpm=[64.0, float("inf")]),
    lambda plan: plan["changes"][0].update(bpm=[64.0, float("nan")]),
    lambda plan: plan["changes"][0].update(bpm=[64.0, True]),
    lambda plan: plan["changes"][0].update(bpm=[64.0]),
    lambda plan: plan["changes"][0].update(key=["A minor", "not a key"]),
    lambda plan: plan["changes"][0].update(track_id=True),
])
def test_apply_rejects_invalid_plan_without_partial_writes(tmp_path, mutation):
    sync, args, plan = _planned_library(tmp_path)
    mutation(plan)
    args.plan.write_text(json.dumps(plan))
    with pytest.raises(ValueError):
        sync.apply(args)
    assert _values(args.db) == [(1, 64.0, "A minor"), (2, 65.0, "G minor")]


def test_failed_backup_leaves_library_unchanged_and_removes_partial_copy(tmp_path, monkeypatch):
    sync, args, _ = _planned_library(tmp_path)
    args.no_backup = False
    def full_disk(source, target):
        target.write(source.read(32))
        raise OSError("No space left on device")
    monkeypatch.setattr(sync.shutil, "copyfileobj", full_disk)
    with pytest.raises(OSError, match="No space"):
        sync.apply(args)
    assert not list(tmp_path.glob("fixture.duckdb.before-rekordbox-sync-*"))
    assert _values(args.db) == [(1, 64.0, "A minor"), (2, 65.0, "G minor")]
