"""Read-only aggregates of a plumdeck library through Python's duckdb.

    python tools/parity/db_stats.py FILE > python.json
    plumdeck-parity db-stats FILE > rust.json

Prints the same JSON as `plumdeck-parity db-stats` (plan R2: the Rust
engine must read a file and its WAL exactly as the Python backend does):
per table the row count and, per column, the non-NULL count and
bit_xor(hash(column)); per selected column a SHA-256 over the values as the
client decodes them, in primary-key order. Only aggregates are printed.

Opens FILE read-only and imports nothing from the backend. Run it on a copy
(e.g. an APFS clone), never on the live library.
"""
from __future__ import annotations

import datetime
import hashlib
import json
import struct
import sys

import duckdb

# Keep in sync with CLIENT_COLUMNS in plumdeck-parity/src/dbstats.rs.
CLIENT_COLUMNS = [
    ("tracks", "id", "id"),
    ("tracks", "id", "filepath"),
    ("tracks", "id", "bpm"),
    ("tracks", "id", "duration"),
    ("tracks", "id", "is_genre_verified"),
    ("tracks", "id", "created_at"),
    ("track_analyses", "track_id", "beats_f32"),
    ("track_analyses", "track_id", "waveform_u8"),
    ("track_analyses", "track_id", "features_extra_json"),
    ("lyrics", "track_id", "content"),
    ("lyrics", "track_id", "updated_at"),
    ("setlists", "id", "target_duration"),
    ("setlists", "id", "updated_at"),
    ("setlist_tracks", "id", "in_ms"),
    ("settings", "key", "value"),
]


def quote(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def canonical(value) -> str:
    if value is None:
        return "\\N"
    if isinstance(value, bool):
        return "1" if value else "0"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return struct.pack(">d", value).hex()
    if isinstance(value, str):
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).hex()
    if isinstance(value, datetime.datetime):
        return value.isoformat()
    return f"<{type(value).__name__}>"


def stats(path: str) -> dict:
    conn = duckdb.connect(path, read_only=True)
    try:
        version = conn.execute("SELECT version()").fetchone()[0]
        tables = [row[0] for row in conn.execute(
            "SELECT table_name FROM duckdb_tables() WHERE schema_name = 'main' AND NOT temporary "
            "ORDER BY table_name").fetchall()]

        def columns(table: str) -> list[str]:
            return [row[0] for row in conn.execute(
                "SELECT column_name FROM duckdb_columns() WHERE schema_name = 'main' AND table_name = ? "
                "ORDER BY column_index", [table]).fetchall()]

        out_tables = {}
        for table in tables:
            cols = columns(table)
            select = ["CAST(count(*) AS VARCHAR)"]
            for col in cols:
                select.append(f"CAST(count({quote(col)}) AS VARCHAR)")
                select.append(f"CAST(bit_xor(hash({quote(col)})) AS VARCHAR)")
            values = conn.execute(f"SELECT {', '.join(select)} FROM {quote(table)}").fetchone()
            out_tables[table] = {
                "rows": values[0],
                "columns": {col: [values[1 + 2 * i], values[2 + 2 * i]] for i, col in enumerate(cols)},
            }
        client = {}
        for table, order, column in CLIENT_COLUMNS:
            if table not in tables or column not in columns(table):
                continue
            digest = hashlib.sha256()
            count = 0
            cursor = conn.execute(f"SELECT {quote(column)} FROM {quote(table)} ORDER BY {quote(order)}")
            while True:
                batch = cursor.fetchmany(10000)
                if not batch:
                    break
                for (value,) in batch:
                    digest.update(canonical(value).encode("utf-8", "surrogatepass"))
                    digest.update(b"\n")
                    count += 1
            client[f"{table}.{column}"] = [count, digest.hexdigest()]
        return {"engine": version, "tables": out_tables, "client": client}
    finally:
        conn.close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: db_stats.py FILE")
    print(json.dumps(stats(sys.argv[1]), indent=2, ensure_ascii=False))
