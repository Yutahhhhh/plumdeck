#!/usr/bin/env python3
"""Bring existing tracks' BPM (and, with --fields bpm,key, key) in line with rekordbox.

New saves already apply manual > rekordbox > own analysis. This script applies
the same rule once to tracks saved before, in two steps so nothing changes
before the plan has been read:

    backend/.venv/bin/python scripts/sync_rekordbox_values.py plan --db LIBRARY.duckdb --out plan.json [--fields bpm,key]
    backend/.venv/bin/python scripts/sync_rekordbox_values.py apply --db LIBRARY.duckdb plan.json

``plan`` opens both libraries read-only. It compares every track with its entry
in the rekordbox collection (djmdContent, not the playlist mirror) and lists the
tracks whose stored value differs. Manual values (CSV metadata import) stay.
Keys are planned only while the rekordbox_key_source setting is on, as saves
otherwise bring plumdeck's own key back.

``apply`` needs the app stopped (DuckDB allows one writer). It copies the library
first unless --no-backup, checks that every planned track still has the value
the plan saw, writes all changes in one transaction and records the source in
features_extra exactly like a save does.
"""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import shutil
import sys

import duckdb

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "backend"))

PLAN_VERSION = 1
BPM_TOLERANCE = 0.005


def octave_apart(old: float | None, new: float) -> bool:
    return bool(old) and any(abs(old * factor - new) / new < 0.01 for factor in (2.0, 0.5))


def same_bpm(left: float | None, right: float | None) -> bool:
    if left is None or right is None:
        return left is right
    return abs(left - right) < BPM_TOLERANCE


def _read_extra(raw_extra):
    """Read provenance without treating corrupt metadata as an unowned value."""
    try:
        extra = json.loads(raw_extra) if raw_extra else {}
    except (TypeError, ValueError) as exc:
        raise ValueError("Unreadable analysis provenance") from exc
    if not isinstance(extra, dict):
        raise ValueError("Unreadable analysis provenance")
    components = extra.get("analysis_components")
    if components is not None and not isinstance(components, dict):
        raise ValueError("Unreadable analysis provenance")
    sources = components.get("sources") if components else None
    if sources is not None and not isinstance(sources, dict):
        raise ValueError("Unreadable analysis provenance")
    return extra


def _validate_plan(payload, database):
    """Reject an invalid or misdirected plan before opening a writable library."""
    from domain.services import value_sources
    from utils.audio_math import key_name

    if not isinstance(payload, dict) or payload.get("version") != PLAN_VERSION:
        raise ValueError("Unsupported plan file")
    planned_database = payload.get("database")
    if not isinstance(planned_database, str) or Path(planned_database).expanduser().resolve() != database:
        raise ValueError("The plan belongs to a different library")
    fields = payload.get("fields")
    if (not isinstance(fields, list) or not fields
            or any(not isinstance(field, str) or field not in value_sources.FIELDS for field in fields)
            or len(fields) != len(set(fields))):
        raise ValueError("Invalid plan fields")
    changes = payload.get("changes")
    if not isinstance(changes, list):
        raise ValueError("Invalid plan changes")
    seen = set()
    for change in changes:
        if not isinstance(change, dict) or set(change) - {"track_id", "filepath", "bpm", "key"}:
            raise ValueError("Invalid planned track")
        track_id, filepath = change.get("track_id"), change.get("filepath")
        if (not isinstance(track_id, int) or isinstance(track_id, bool) or track_id <= 0
                or not isinstance(filepath, str) or not filepath or track_id in seen):
            raise ValueError("Invalid or duplicate planned track")
        seen.add(track_id)
        changed_fields = set(change) & set(value_sources.FIELDS)
        if not changed_fields or changed_fields - set(fields):
            raise ValueError("Planned values do not match plan fields")
        for field in changed_fields:
            pair = change[field]
            if not isinstance(pair, list) or len(pair) != 2:
                raise ValueError("Invalid planned value pair")
            old, new = pair
            if field == "bpm":
                if (old is not None and (not isinstance(old, (int, float)) or isinstance(old, bool)
                                         or not math.isfinite(old))) or not value_sources.valid(field, new):
                    raise ValueError("Invalid planned BPM")
            elif (old is not None and not isinstance(old, str)) or not isinstance(new, str) or key_name(new) != new:
                raise ValueError("Invalid planned key")
    return changes


def build_plan(rows, index, fields):
    """Changes to ``fields`` for ``rows`` of (id, filepath, bpm, key, features_extra_json).

    The summary counts differences for both fields even when one is not planned.
    """
    from domain.services import value_sources
    from utils.audio_math import key_name, normalize_key

    counts = Counter()
    changes = []
    for track_id, filepath, bpm, key, raw_extra in rows:
        counts["tracks"] += 1
        entry = index.get(filepath) if filepath else None
        if entry is None:
            counts["not_in_rekordbox"] += 1
            continue
        counts["in_rekordbox"] += 1
        try:
            extra = _read_extra(raw_extra)
        except ValueError:
            counts["unreadable_provenance_kept"] += 1
            continue
        sources = value_sources.stored_sources(extra)
        rekordbox = value_sources.RekordboxValues(
            True, entry.bpm if value_sources.valid("bpm", entry.bpm) else None, key_name(entry.key))
        change = {}
        if rekordbox.bpm is None:
            counts["rekordbox_without_bpm"] += 1
        elif sources.get("bpm") == value_sources.MANUAL:
            counts["bpm_manual_kept"] += 1
        elif not same_bpm(bpm, rekordbox.bpm):
            counts["bpm_changes"] += 1
            counts["bpm_changes_over_half_percent"] += not bpm or abs(bpm - rekordbox.bpm) / rekordbox.bpm > 0.005
            counts["bpm_octave_changes"] += octave_apart(bpm, rekordbox.bpm)
            if "bpm" in fields:
                change["bpm"] = [bpm, rekordbox.bpm]
        if rekordbox.key is None:
            counts["rekordbox_without_key"] += 1
        elif sources.get("key") == value_sources.MANUAL:
            counts["key_manual_kept"] += 1
        elif (key or "") != rekordbox.key:
            same = normalize_key(key) is not None and normalize_key(key) == normalize_key(rekordbox.key)
            counts["key_respelled" if same else "key_changes"] += 1
            if not same and normalize_key(key) and normalize_key(key)[-1] != normalize_key(rekordbox.key)[-1]:
                counts["key_mode_flips"] += 1
            if "key" in fields:
                change["key"] = [key, rekordbox.key]
        if change:
            changes.append({"track_id": track_id, "filepath": filepath, **change})
    return changes, dict(counts)


def _key_source_enabled(connection) -> bool:
    from domain.services import value_sources

    try:
        row = connection.execute("SELECT value FROM settings WHERE key = ?",
                                 [value_sources.KEY_SOURCE_SETTING]).fetchone()
    except duckdb.Error:
        row = None
    return value_sources.key_source_value(row[0] if row else None)


def plan(args) -> None:
    from domain.services import value_sources
    from infra import rekordbox_grid, rekordbox_library

    if args.rekordbox_db:
        os.environ["PLUMDECK_REKORDBOX_DB"] = str(Path(args.rekordbox_db).expanduser().resolve())
    index = rekordbox_library.collection_index()
    connection = duckdb.connect(str(args.db), read_only=True)
    try:
        if "key" in args.fields and not _key_source_enabled(connection):
            # Saves would put plumdeck's own keys back while the setting is off.
            raise SystemExit(f"Turn the '{value_sources.KEY_SOURCE_SETTING}' setting on before syncing keys")
        rows = connection.execute(
            "SELECT t.id, t.filepath, t.bpm, t.key, a.features_extra_json FROM tracks t "
            "LEFT JOIN track_analyses a ON a.track_id = t.id ORDER BY t.id").fetchall()
    finally:
        connection.close()
    changes, summary = build_plan(rows, index, args.fields)
    payload = {
        "version": PLAN_VERSION, "created_at": datetime.now(timezone.utc).isoformat(),
        "database": str(Path(args.db).expanduser().resolve()), "rekordbox_database": str(rekordbox_grid.master_db_path()),
        "fields": args.fields, "summary": summary, "changes": changes,
    }
    Path(args.out).write_text(json.dumps(payload, ensure_ascii=False, indent=1), encoding="utf-8")
    print(json.dumps({"plan": str(args.out), **summary}, ensure_ascii=False, indent=2))


def _current(connection, change):
    return connection.execute(
        "SELECT t.bpm, t.key, a.track_id, a.features_extra_json FROM tracks t "
        "LEFT JOIN track_analyses a ON a.track_id = t.id WHERE t.id = ? AND t.filepath = ?",
        [change["track_id"], change["filepath"]]).fetchone()


def _updated_extra(raw_extra, change):
    from domain.services import value_sources

    extra = _read_extra(raw_extra)
    previous = value_sources.stored_sources(extra)
    if any(previous.get(field) == value_sources.MANUAL for field in value_sources.FIELDS if field in change):
        raise RuntimeError(f"Track {change['track_id']} became manual since planning")
    updated = value_sources.with_sources(extra, {field: value_sources.REKORDBOX for field in ("bpm", "key")
                                                 if field in change})
    old_bpm = change.get("bpm", [None])[0]
    if "bpm" in change and previous.get("bpm") in (None, value_sources.OWN) and value_sources.valid("bpm", old_bpm):
        updated["analysis_bpm"] = old_bpm  # the replaced own estimate, as a save keeps it
    return json.dumps(updated)


def apply(args) -> None:
    from domain.services import value_sources

    payload = json.loads(Path(args.plan).read_text(encoding="utf-8"))
    database = Path(args.db).expanduser().resolve()
    changes = _validate_plan(payload, database)
    if not database.is_file():
        raise ValueError("Library does not exist")
    connection = duckdb.connect(str(database))  # fails while the app holds the library
    backup = None
    try:
        connection.execute("CHECKPOINT")
        if not args.no_backup:
            stamp = datetime.now().strftime("%Y%m%d-%H%M%S-%f")
            backup = database.with_name(f"{database.name}.before-rekordbox-sync-{stamp}")
            created = False
            try:
                with database.open("rb") as source, backup.open("xb") as target:
                    created = True
                    shutil.copyfileobj(source, target)
                shutil.copystat(database, backup)
            except BaseException:
                # A failed copy (for example a full disk) is not a restorable
                # backup. Never remove a pre-existing file after an xb clash.
                if created:
                    backup.unlink(missing_ok=True)
                raise
        connection.execute("BEGIN TRANSACTION")
        for change in changes:
            row = _current(connection, change)
            if row is None:
                raise RuntimeError(f"Track {change['track_id']} disappeared or moved since planning")
            bpm, key, analysis_id, raw_extra = row
            if "bpm" in change and not same_bpm(bpm, change["bpm"][0]):
                raise RuntimeError(f"BPM of track {change['track_id']} changed since planning")
            if "key" in change and (key or "") != (change["key"][0] or ""):
                raise RuntimeError(f"Key of track {change['track_id']} changed since planning")
            # Recheck provenance as well as the old value: a manual edit can
            # deliberately confirm the same number after a plan was created.
            updated = _updated_extra(raw_extra, change)
            if "bpm" in change:
                connection.execute("UPDATE tracks SET bpm = ? WHERE id = ?", [change["bpm"][1], change["track_id"]])
            if "key" in change:
                connection.execute("UPDATE tracks SET key = ?, scale = ? WHERE id = ?",
                                   [change["key"][1], value_sources.scale_of(change["key"][1]), change["track_id"]])
            if analysis_id is None:
                connection.execute("INSERT INTO track_analyses (track_id, features_extra_json) VALUES (?, ?)",
                                   [change["track_id"], updated])
            else:
                connection.execute("UPDATE track_analyses SET features_extra_json = ? WHERE track_id = ?",
                                   [updated, change["track_id"]])
        mismatched = 0
        for change in changes:
            bpm, key, _, _ = _current(connection, change)
            mismatched += "bpm" in change and not same_bpm(bpm, change["bpm"][1])
            mismatched += "key" in change and key != change["key"][1]
        if mismatched:
            raise RuntimeError(f"Verification found {mismatched} mismatched values")
        connection.execute("COMMIT")
        connection.execute("CHECKPOINT")
    except Exception:
        try:
            connection.execute("ROLLBACK")
        except duckdb.Error:
            pass
        raise
    finally:
        connection.close()
    print(json.dumps({"applied": len(changes), "fields": payload.get("fields"),
                      "backup": str(backup) if backup else None}, ensure_ascii=False))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    planner = commands.add_parser("plan", help="list the changes (read-only)")
    planner.add_argument("--db", required=True, type=Path, help="plumdeck.duckdb to read")
    planner.add_argument("--rekordbox-db", help="rekordbox master.db (default: rekordbox's own)")
    planner.add_argument("--out", required=True, help="plan file to write")
    planner.add_argument("--fields", default="bpm", type=lambda value: [f for f in value.split(",") if f],
                         help="bpm, key or bpm,key (keys need the rekordbox_key_source setting on)")
    applier = commands.add_parser("apply", help="write a reviewed plan (app stopped)")
    applier.add_argument("--db", required=True, type=Path, help="plumdeck.duckdb to change")
    applier.add_argument("plan")
    applier.add_argument("--no-backup", action="store_true", help="skip the copy of the library")
    args = parser.parse_args()
    if args.command == "plan" and (not args.fields or set(args.fields) - {"bpm", "key"}):
        parser.error("--fields takes bpm, key or bpm,key")
    (plan if args.command == "plan" else apply)(args)


if __name__ == "__main__":
    main()
