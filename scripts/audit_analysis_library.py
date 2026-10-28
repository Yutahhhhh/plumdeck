#!/usr/bin/env python3
"""Read-only integrity fingerprints; run against a backup or a stopped library."""
import argparse
import json
import os
import duckdb


def audit(path, check_files=False):
    con = duckdb.connect(path, read_only=True)
    try:
        protected = {}
        for table in ("tracks", "lyrics", "setlists", "setlist_tracks"):
            # Hash columns in name order: a rebuilt file may order them differently.
            columns = sorted(row[0] for row in con.execute(
                "SELECT column_name FROM information_schema.columns WHERE table_schema = 'main' AND table_name = ?",
                [table]).fetchall())
            values = ", ".join('"' + column.replace('"', '""') + '"' for column in columns)
            count, fingerprint = con.execute(f"SELECT COUNT(*), BIT_XOR(HASH({values})) FROM {table}").fetchone()
            protected[table] = {"count": count, "fingerprint": str(fingerprint)}
        count, fingerprint = con.execute("SELECT COUNT(*), BIT_XOR(HASH(track_id, beats_f32, waveform_u8)) FROM track_analyses").fetchone()
        protected["beats_and_waveforms"] = {"count": count, "fingerprint": str(fingerprint)}
        models = dict(con.execute("SELECT model_name,COUNT(*) FROM track_embeddings GROUP BY model_name").fetchall())
        result = {"protected": protected, "embedding_models": models}
        if check_files:
            rows = con.execute("SELECT id, filepath FROM tracks ORDER BY id").fetchall()
            unavailable = [{"track_id": track_id, "filepath": filepath}
                           for track_id, filepath in rows if not os.path.isfile(filepath)]
            result["audio_files"] = {"total": len(rows), "available": len(rows) - len(unavailable),
                                     "unavailable_count": len(unavailable), "unavailable": unavailable}
        return result
    finally:
        con.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database")
    parser.add_argument("--reference")
    parser.add_argument("--check-files", action="store_true", help="Also report missing/non-file audio paths without modifying them")
    args = parser.parse_args()
    result = audit(args.database, args.check_files)
    if args.reference:
        result["protected_data_unchanged"] = result["protected"] == audit(args.reference)["protected"]
    print(json.dumps(result, ensure_ascii=False, indent=2))
    if args.reference and not result["protected_data_unchanged"]:
        raise SystemExit(1)
