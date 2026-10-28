"""Read-only rekordbox master.db lookup and bounded, exact PQTZ decoding.

Format: https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html
Never instantiate Rekordbox6Database: its higher-level APIs can modify the library.
"""
from __future__ import annotations

import os
from pathlib import Path
import statistics
import struct
import sys

from api.schemas.performance_metadata import BeatGrid

MAX_ANLZ_BYTES = 32 * 1024 * 1024


class RekordboxGridError(ValueError):
    pass


def master_db_path() -> Path | None:
    explicit = os.environ.get("PLUMDECK_REKORDBOX_DB")
    if explicit:
        return Path(explicit).expanduser()
    if sys.platform == "darwin":
        return Path.home() / "Library/Pioneer/rekordbox/master.db"
    if sys.platform == "win32" and os.environ.get("APPDATA"):
        return Path(os.environ["APPDATA"]) / "Pioneer/rekordbox/master.db"
    return None


def connect_readonly(path: Path):
    try:
        from pyrekordbox.db6.database import BLOB, deobfuscate
        from sqlcipher3 import dbapi2
    except ImportError as exc:
        raise RekordboxGridError("rekordbox reading requires bundled pyrekordbox and SQLCipher") from exc
    connection = dbapi2.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=2)
    try:
        connection.execute("PRAGMA key='" + deobfuscate(BLOB).replace("'", "''") + "'")
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        return connection
    except Exception:
        connection.close()
        raise


def find_analysis(filepath: str, database: Path | None = None) -> tuple[Path, str] | None:
    database = database or master_db_path()
    if database is None or not database.is_file():
        return None
    try:
        connection = connect_readonly(database)
        try:
            # Exact path identity, never title/artist guessing; parameterized and WAL-aware.
            if sys.platform == "win32":
                from infra.rekordbox_library import same_path
                candidates = connection.execute(
                    "SELECT ID, AnalysisDataPath, FolderPath FROM djmdContent "
                    "WHERE replace(FolderPath, char(92), '/') COLLATE NOCASE = ? AND rb_local_deleted = 0",
                    (filepath.replace("\\", "/"),),
                ).fetchall()
                rows = [row[:2] for row in candidates if same_path(filepath, str(row[2]))]
            else:
                rows = connection.execute(
                    "SELECT ID, AnalysisDataPath FROM djmdContent "
                    "WHERE FolderPath = ? AND rb_local_deleted = 0", (filepath,),
                ).fetchall()
        finally:
            connection.close()
        if not rows:
            return None
        paths = {str(row[1]) for row in rows if row[1]}
        if len(paths) > 1:
            raise RekordboxGridError("Multiple rekordbox grids match this audio path")
        if not paths:
            return None
        analysis = analysis_file(next(iter(paths)), database)
        if analysis is None:
            return None
        return analysis, str(rows[0][0])
    except RekordboxGridError:
        raise
    except Exception as exc:
        raise RekordboxGridError("Could not read the local rekordbox library") from exc


def analysis_file(analysis_data_path: str, database: Path) -> Path | None:
    """The existing ANLZ file for a djmdContent.AnalysisDataPath, or None."""
    raw = analysis_data_path.replace("\\", "/")
    analysis = Path(raw)
    if analysis.is_file():
        return analysis
    # Rekordbox stores /share/... paths relative to the library directory.
    candidates = [database.parent / "share" / raw.lstrip("/"), database.parent / raw.lstrip("/")]
    return next((path for path in candidates if path.is_file()), None)


def _pqtz_section(data: bytes) -> tuple[int, int, int] | None:
    """(offset, size, beat count) of the PQTZ section, or None without one."""
    if len(data) < 12 or len(data) > MAX_ANLZ_BYTES or data[:4] != b"PMAI":
        raise RekordboxGridError("Invalid ANLZ header")
    header_size, file_size = struct.unpack_from(">II", data, 4)
    if not 12 <= header_size <= file_size == len(data):
        raise RekordboxGridError("Invalid ANLZ length")
    offset = header_size
    while offset < file_size:
        if offset + 12 > file_size:
            raise RekordboxGridError("Truncated ANLZ section")
        kind, header, size = struct.unpack_from(">4sII", data, offset)
        if not 12 <= header <= size or offset + size > file_size:
            raise RekordboxGridError("Invalid ANLZ section length")
        if kind == b"PQTZ":
            if header != 24:
                raise RekordboxGridError("Invalid PQTZ header")
            count = struct.unpack_from(">I", data, offset + 20)[0]
            if count > 100000 or 24 + count * 8 != size:
                raise RekordboxGridError("Invalid PQTZ beat count")
            return offset, size, count
        offset += size
    return None


def parse_pqtz(data: bytes) -> BeatGrid | None:
    section = _pqtz_section(data)
    if section is None or section[2] < 2:
        return None
    offset, size, _ = section
    entries = list(struct.iter_unpack(">HHI", data[offset + 24:offset + size]))
    numbers, tempos, times = map(list, zip(*entries))
    try:
        return BeatGrid(bpm=statistics.median(tempos) / 100,
                        first_beat_ms=times[0], beats_per_bar=max(numbers),
                        beat_times_ms=times, beat_numbers=numbers,
                        source="rekordbox", confidence=None)
    except ValueError as exc:
        raise RekordboxGridError("Invalid PQTZ beat values") from exc


def _read_anlz(path: Path) -> bytes:
    with path.open("rb") as source:
        # Size the read to the file: read(MAX + 1) would allocate 32 MiB per call.
        size = min(os.fstat(source.fileno()).st_size, MAX_ANLZ_BYTES)
        return source.read(size + 1)


def read_grid(path: Path) -> BeatGrid | None:
    return parse_pqtz(_read_anlz(path))


def has_usable_grid(analysis_data_path: str, database: Path) -> bool:
    """Whether the ANLZ file holds a PQTZ grid of at least two beats.

    Checks the section structure only (a library-wide scan stays fast); a grid
    whose beat values turn out invalid at load time falls back to analysis.
    """
    try:
        path = analysis_file(analysis_data_path, database)
        if path is None:
            return False
        section = _pqtz_section(_read_anlz(path))
        return section is not None and section[2] >= 2
    except (RekordboxGridError, OSError):
        return False
