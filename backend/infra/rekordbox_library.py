"""Read-only lookups against the local rekordbox collection.

Assist mode needs two things from rekordbox: what a given open file actually is,
and which of plumdeck's tracks rekordbox knows about at all. Both are answered with
plain SELECTs over the encrypted master.db through the same read-only,
``query_only`` connection the beat-grid reader uses. Nothing here writes, and no
higher-level pyrekordbox API is instantiated, so the running rekordbox is never
disturbed.
"""
from __future__ import annotations

from dataclasses import dataclass
from functools import lru_cache
import ntpath
import sys
from pathlib import Path
import os
import threading
import time
import unicodedata

from infra.rekordbox_grid import RekordboxGridError, connect_readonly, master_db_path

# A full collection scan is cheap (~60 ms for 25k rows) but pointless to repeat
# on every deck change, so it is cached until master.db is touched again.
_COLLECTION_TTL_SECONDS = 300.0
# Guards against a pathological library making the request hang.
_MAX_LOOKUP_PATHS = 256
# SQLite's default parameter ceiling is 999; path variants multiply the count,
# so the IN list is issued in chunks well below it.
_SQL_CHUNK = 250


class RekordboxLibraryUnavailable(RuntimeError):
    """The local rekordbox collection could not be read at all."""


@dataclass(frozen=True)
class RekordboxEntry:
    """One row of the rekordbox collection, identified by its exact file path."""

    content_id: str
    filepath: str
    title: str
    artist: str
    bpm: float | None


def normalize_path(filepath: str) -> str:
    """The comparison key for a path: composed form, exact otherwise.

    macOS hands out decomposed (NFD) filenames while tags, imports and lsof may
    carry the composed (NFC) spelling of the same file, so the two must compare
    equal. Case is deliberately left alone: on a case-sensitive volume two
    spellings really are two files.
    """
    if sys.platform == "win32":
        filepath = ntpath.normpath(filepath)
    return unicodedata.normalize("NFC", filepath)


def path_variants(filepath: str) -> list[str]:
    """Every spelling of one path that a database might legitimately store.

    SQL comparison is byte-exact, so normalizing before the query would miss the
    row whenever master.db and the observed path disagree on composition. The
    observed spelling is queried first and kept as the real path; the canonical
    forms are only extra spellings to look for.
    """
    if not filepath:
        return []
    variants = [filepath, unicodedata.normalize("NFC", filepath), unicodedata.normalize("NFD", filepath)]
    if sys.platform == "win32":
        variants += [variant.replace("\\", "/") for variant in variants.copy()]
        variants += [variant.replace("/", "\\") for variant in variants.copy()]
    return list(dict.fromkeys(variants))


def same_path(left: str, right: str) -> bool:
    """Accept alternate Unicode spelling only when it refers to the same file."""
    if left == right:
        return True
    if sys.platform != "win32" and normalize_path(left) != normalize_path(right):
        return False
    try:
        return os.path.samefile(left, right)
    except OSError:
        return False


@lru_cache(maxsize=2)
def _windows_path_index(registered: frozenset[str]) -> dict[str, tuple[str, ...]]:
    grouped: dict[str, list[str]] = {}
    for path in registered:
        grouped.setdefault(ntpath.normcase(normalize_path(path)), []).append(path)
    return {key: tuple(values) for key, values in grouped.items()}


def is_registered_path(filepath: str, registered: frozenset[str]) -> bool:
    if sys.platform == "win32":
        candidates = _windows_path_index(registered).get(ntpath.normcase(normalize_path(filepath)), ())
        # Case-insensitive candidate lookup must still prove filesystem identity:
        # Windows can also have directories with case-sensitive filenames.
        return any(same_path(filepath, path) for path in candidates)
    return any(path in registered and same_path(filepath, path)
               for path in path_variants(filepath))


def normalize_text(value: str | None) -> str:
    """Fold a displayed title/artist for comparison without losing distinctions.

    Case and width folding only: two different tracks that merely differ in
    punctuation must still compare as different, because picking the wrong one
    would silently recommend transitions out of a track that is not playing.
    """
    if not value:
        return ""
    folded = unicodedata.normalize("NFKC", value).casefold()
    return " ".join(folded.split())


def _database() -> Path:
    path = master_db_path()
    if path is None or not path.is_file():
        raise RekordboxLibraryUnavailable("rekordbox のライブラリ (master.db) が見つかりません")
    return path


def _connect(path: Path):
    try:
        return connect_readonly(path)
    except RekordboxGridError as error:
        raise RekordboxLibraryUnavailable(str(error)) from error
    except Exception as error:  # sqlcipher raises its own driver errors
        raise RekordboxLibraryUnavailable(
            "rekordbox のライブラリを読み取れませんでした"
        ) from error


def lookup_by_paths(paths: list[str]) -> dict[str, RekordboxEntry]:
    """Resolve exact file paths to rekordbox collection rows.

    Keyed by the path exactly as master.db spells it, so two rows that differ
    only in Unicode composition stay two rows: on a case- or normalization-
    sensitive volume they may be different files, and collapsing them would let
    a deck resolve to the wrong one. Paths rekordbox does not know are simply
    absent from the result rather than guessed at by title.
    """
    wanted = list(dict.fromkeys(path for path in paths if path))[:_MAX_LOOKUP_PATHS]
    spellings = list(
        dict.fromkeys(variant for path in wanted for variant in path_variants(path))
    )
    if not spellings:
        return {}
    connection = _connect(_database())
    rows: list = []
    try:
        for start in range(0, len(spellings), _SQL_CHUNK):
            chunk = spellings[start : start + _SQL_CHUNK]
            placeholders = ",".join("?" * len(chunk))
            column = "c.FolderPath"
            if sys.platform == "win32":
                column = "replace(c.FolderPath, char(92), '/') COLLATE NOCASE"
                chunk = [path.replace("\\", "/") for path in chunk]
            rows += connection.execute(
                "SELECT c.FolderPath, c.ID, c.Title, a.Name, c.BPM "
                "FROM djmdContent c LEFT JOIN djmdArtist a ON a.ID = c.ArtistID "
                f"WHERE {column} IN ({placeholders}) AND c.rb_local_deleted = 0",
                chunk,
            ).fetchall()
    except Exception as error:
        raise RekordboxLibraryUnavailable(
            "rekordbox のライブラリを読み取れませんでした"
        ) from error
    finally:
        connection.close()

    entries: dict[str, RekordboxEntry] = {}
    for folder_path, content_id, title, artist, bpm in rows:
        # rekordbox stores BPM scaled by 100; 0 means "no tempo analysed".
        tempo = float(bpm) / 100 if bpm else None
        filepath = str(folder_path)
        if not any(same_path(filepath, path) for path in wanted):
            continue
        entries[filepath] = RekordboxEntry(
            content_id=str(content_id),
            filepath=filepath,
            title=str(title or ""),
            artist=str(artist or ""),
            bpm=tempo,
        )
    return entries


def lookup_by_identities(
    identities: list[tuple[str | None, str | None]],
) -> dict[str, RekordboxEntry]:
    """Return collection rows whose normalized title and artist both match.

    This is the fallback for platforms where rekordbox does not expose every
    loaded track as an open file handle.  There are at most four deck
    identities, and a complete collection scan is bounded and inexpensive for
    a local ``master.db``.  Filtering in Python preserves the same Unicode,
    width, case and whitespace semantics used for deck resolution.
    """
    wanted = {
        (normalize_text(title), normalize_text(artist))
        for title, artist in identities
        if normalize_text(title)
    }
    if not wanted:
        return {}

    connection = _connect(_database())
    try:
        rows = connection.execute(
            "SELECT c.FolderPath, c.ID, c.Title, a.Name, c.BPM "
            "FROM djmdContent c LEFT JOIN djmdArtist a ON a.ID = c.ArtistID "
            "WHERE c.rb_local_deleted = 0 AND c.FolderPath IS NOT NULL"
        ).fetchall()
    except Exception as error:
        raise RekordboxLibraryUnavailable(
            "rekordbox のライブラリを読み取れませんでした"
        ) from error
    finally:
        connection.close()

    entries: dict[str, RekordboxEntry] = {}
    for folder_path, content_id, title, artist, bpm in rows:
        if (normalize_text(title), normalize_text(artist)) not in wanted:
            continue
        filepath = str(folder_path)
        entries[filepath] = RekordboxEntry(
            content_id=str(content_id),
            filepath=filepath,
            title=str(title or ""),
            artist=str(artist or ""),
            bpm=float(bpm) / 100 if bpm else None,
        )
    return entries


class _CollectionPathCache:
    """Caches the set of paths rekordbox has in its collection."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._paths: frozenset[str] | None = None
        self._stamp: tuple | None = None
        self._loaded_at = 0.0

    def get(self) -> frozenset[str]:
        database = _database()
        status = database.stat()
        try:
            wal = Path(str(database) + "-wal").stat()
            wal_stamp = (wal.st_ino, wal.st_mtime_ns, wal.st_size)
        except FileNotFoundError:
            wal_stamp = None
        stamp = (str(database.resolve()), status.st_ino, status.st_mtime_ns,
                 status.st_size, wal_stamp)
        with self._lock:
            fresh = time.monotonic() - self._loaded_at < _COLLECTION_TTL_SECONDS
            if self._paths is not None and self._stamp == stamp and fresh:
                return self._paths
        paths = self._load(database)
        with self._lock:
            self._paths = paths
            self._stamp = stamp
            self._loaded_at = time.monotonic()
        return paths

    @staticmethod
    def _load(database: Path) -> frozenset[str]:
        connection = _connect(database)
        try:
            rows = connection.execute(
                "SELECT FolderPath FROM djmdContent "
                "WHERE rb_local_deleted = 0 AND FolderPath IS NOT NULL"
            ).fetchall()
        except Exception as error:
            raise RekordboxLibraryUnavailable(
                "rekordbox のライブラリを読み取れませんでした"
            ) from error
        finally:
            connection.close()
        return frozenset(str(row[0]) for row in rows if row[0])

    def invalidate(self) -> None:
        with self._lock:
            self._paths = None
            self._stamp = None
            self._loaded_at = 0.0


_collection_paths = _CollectionPathCache()


def registered_paths() -> frozenset[str]:
    """Every file path currently in the rekordbox collection."""
    return _collection_paths.get()


def reset_cache() -> None:
    """Drops the cached collection. Used by tests and after an import."""
    _collection_paths.invalidate()
