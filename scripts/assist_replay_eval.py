#!/usr/bin/env python3
"""Replay the DJ's rekordbox HISTORY against assist suggestions.

For every transition A -> B in sets played since --since, ask the real
AssistAppService for suggestions out of A, with history built only from the
sets played *before* that set and the tracks already played in it excluded,
and measure two things:

* hit@k / MRR  — how often the track the DJ actually played next is suggested.
* hard@5       — how many of the top five look hard to mix: more than 3% off
                 tempo, a half/double-time reading, or an energy jump over 0.2.

A good change keeps hit@k and lowers hard@5; a tuning that only raises hit@k
may just be copying old transitions that did not work.

Read-only: both libraries are copied first (to a temporary directory, or to
--copies, where they are reused on later runs) and only the copies are opened.
Constants can be tried without editing the code:

    backend/.venv/bin/python scripts/assist_replay_eval.py --presets keep,hype \\
        --set assist_recommendation.TEMPO_SIGMA_PERCENT=2.0
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from datetime import datetime
import importlib
import os
from pathlib import Path
import shutil
import sys
import tempfile

import platformdirs

ROOT = Path(__file__).resolve().parent.parent
HARD_TEMPO_PERCENT = 3.0
HARD_ENERGY = 0.2


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--since", default="2025-10-27", help="replay sets started on or after this date")
    parser.add_argument("--presets", default="keep,hype")
    parser.add_argument("--max-transitions", type=int, default=600,
                        help="evenly sample at most this many transitions (0 = all)")
    parser.add_argument("--backend", default=str(ROOT / "backend"), help="backend source to evaluate")
    parser.add_argument("--plumdeck-db", help="plumdeck.duckdb to copy (default: the app's)")
    parser.add_argument("--rekordbox-db", help="rekordbox master.db to copy (default: rekordbox's)")
    parser.add_argument("--copies", help="keep the copies here and reuse them on later runs")
    parser.add_argument("--set", action="append", default=[], metavar="MODULE.NAME=VALUE",
                        help="override a constant in domain.services.<MODULE>")
    return parser.parse_args()


def copy_database(source: Path, directory: Path) -> Path:
    """Copies a database with its journal files so the copy is consistent."""
    target = directory / source.name
    if target.exists():
        return target
    if not source.exists():
        raise SystemExit(f"{source} not found")
    for suffix in ("", "-wal", "-shm", ".wal"):
        path = Path(f"{source}{suffix}")
        if path.exists():
            shutil.copy2(path, Path(f"{target}{suffix}"))
    return target


def main():
    args = parse_args()
    sys.path.insert(0, args.backend)
    os.chdir(args.backend)
    if args.copies:
        Path(args.copies).mkdir(parents=True, exist_ok=True)
        run(args, Path(args.copies))
        return
    scratch = Path(tempfile.mkdtemp(prefix="assist-replay-"))
    try:
        run(args, scratch)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def run(args, copies: Path):
    # Point both libraries at the copies before any backend module reads its
    # settings, so nothing can open the live databases.
    plumdeck_db = Path(args.plumdeck_db or Path(platformdirs.user_data_dir("plumdeck", "plumdeck")) / "plumdeck.duckdb")
    rekordbox_db = Path(args.rekordbox_db or Path.home() / "Library/Pioneer/rekordbox/master.db")
    plumdeck_db = copy_database(plumdeck_db.expanduser(), copies)
    os.environ["DB_PATH"] = str(plumdeck_db)
    os.environ["PLUMDECK_REKORDBOX_DB"] = str(copy_database(rekordbox_db.expanduser(), copies))

    from sqlmodel import Session, create_engine
    from app.services.assist_app_service import AssistAppService, _history_path_key
    from domain.services import assist_recommendation as scoring
    from domain.services import play_history
    from infra import rekordbox_library

    for assignment in args.set:
        name, value = assignment.split("=", 1)
        module_name, constant = name.rsplit(".", 1)
        module = importlib.import_module(f"domain.services.{module_name}")
        setattr(module, constant, type(getattr(module, constant))(value))

    # Read-only, so several runs (say, two versions of the backend) can share one copy.
    session = Session(create_engine(f"duckdb:///{plumdeck_db}", connect_args={"read_only": True}))
    base = AssistAppService(session)
    identities = base._identities_by_path()
    ids_by_identity = base._identity_ids()

    sets = [
        (played.started_at, [identities.get(_history_path_key(path)) for path in played.filepaths])
        for played in rekordbox_library.play_sessions()
    ]
    since = datetime.fromisoformat(args.since)
    tests = [(started, tracks) for started, tracks in sets
             if started is not None and started >= since and len(tracks) >= play_history.MIN_SESSION_TRACKS]
    transitions = []
    for index, (started, tracks) in enumerate(tests):
        for position in range(1, len(tracks)):
            source, played = tracks[position - 1], tracks[position]
            if source and played and source != played and ids_by_identity.get(source):
                transitions.append((index, position))
    if args.max_transitions and len(transitions) > args.max_transitions:
        step = len(transitions) / args.max_transitions
        transitions = [transitions[int(i * step)] for i in range(args.max_transitions)]
    print(f"{len(tests)} sets since {args.since}, {len(transitions)} transitions replayed", flush=True)

    presets = [name.strip() for name in args.presets.split(",") if name.strip()]
    stats = {preset: defaultdict(float) for preset in presets}
    services: dict[int, AssistAppService] = {}
    for done, (index, position) in enumerate(transitions, 1):
        started, tracks = tests[index]
        if index not in services:
            earlier = [(when, played) for when, played in sets if when is not None and when < started]
            services.clear()
            services[index] = AssistAppService(session, history=play_history.PlayHistory(earlier, started))
        service = services[index]
        source, actual = tracks[position - 1], tracks[position]
        excluded = sorted({track_id for identity in tracks[:position] if identity
                           for track_id in ids_by_identity.get(identity, ())})
        for preset in presets:
            result = service.recommend(ids_by_identity[source][0], preset, limit=12,
                                       exclude_track_ids=excluded)
            measure(stats[preset], result, actual, base, scoring)
        if done % 100 == 0:
            print(f"  {done}/{len(transitions)}", flush=True)

    print(f"\n{'preset':<10} {'n':>5} {'hit@1':>6} {'hit@5':>6} {'hit@12':>6} {'MRR':>6} "
          f"{'hard@5':>7} {'>3%':>6} {'x2':>6} {'dE>.2':>6} {'genre':>6} {'empty':>6}")
    for preset, values in stats.items():
        n, picks = values["n"] or 1, values["picks"] or 1
        print(f"{preset:<10} {int(values['n']):>5} {values['hit1'] / n:>6.3f} {values['hit5'] / n:>6.3f} "
              f"{values['hit12'] / n:>6.3f} {values['rr'] / n:>6.3f} {values['hard'] / picks:>7.3f} "
              f"{values['off_tempo'] / picks:>6.3f} {values['octave'] / picks:>6.3f} "
              f"{values['energy_jump'] / picks:>6.3f} {values['genre_change'] / picks:>6.3f} "
              f"{values['empty'] / n:>6.3f}")


def measure(values, result, actual, service, scoring):
    values["n"] += 1
    candidates = result["candidates"]
    if not candidates:
        values["empty"] += 1
    found = [service._recording_identity(c.get("title"), c.get("artist")) for c in candidates]
    if actual in found:
        rank = found.index(actual) + 1
        values["rr"] += 1 / rank
        values["hit1"] += rank <= 1
        values["hit5"] += rank <= 5
        values["hit12"] += rank <= 12
    source = service.tracks.get_by_id(result["source_track_id"])
    for candidate in candidates[:5]:
        values["picks"] += 1
        matched = scoring.matched_tempo(source.bpm, candidate.get("bpm"))
        off = matched is None or abs(matched[1]) > HARD_TEMPO_PERCENT
        octave = matched is not None and matched[2] != 0
        energy = (abs(candidate["energy"] - source.energy) > HARD_ENERGY
                  if candidate.get("energy") is not None and source.energy is not None else False)
        values["off_tempo"] += off
        values["octave"] += octave
        values["energy_jump"] += energy
        values["hard"] += off or octave or energy
        values["genre_change"] += (
            (candidate.get("genre") or "").strip().casefold() != (source.genre or "").strip().casefold()
        )


if __name__ == "__main__":
    main()
