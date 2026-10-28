"""Shared guards for the parity helpers.

Every helper here imports the real Python backend, which opens DB_PATH and
writes under USER_DATA_DIR (and the waveform cache, and reads rekordbox's
master.db). They must only ever run against sandbox copies, never against the
owner's live library.

The guard runs twice: `require_sandbox_env()` before anything is imported,
then `require_sandbox_settings()` on the values backend/config.py actually
resolved, because pydantic-settings matches env names case-insensitively and
also reads ./.env.
"""
from __future__ import annotations

import os
import sys
import unicodedata
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
BACKEND_DIR = REPO_ROOT / "backend"
CONTRACTS_DIR = REPO_ROOT / "contracts"

APP_NAME = "plumdeck"

# backend/config.py Settings fields. pydantic-settings lowercases env names, so
# `db_path` sets DB_PATH and, when both exist, whichever comes last wins.
SETTINGS_FIELDS = (
    "APP_NAME", "APP_AUTHOR", "ENV", "USER_DATA_DIR", "DB_PATH", "MUSIC_DIR", "PLUMDECK_PORT",
    "FRONTEND_PORT", "TF_CPP_MIN_LOG_LEVEL", "NUM_WORKERS", "PLUMDECK_LOG_DIR",
    "NUMBA_CACHE_DIR", "MPLCONFIGDIR",
)
# Settings the backend writes under.
WRITTEN_SETTINGS = ("DB_PATH", "USER_DATA_DIR", "PLUMDECK_LOG_DIR", "NUMBA_CACHE_DIR", "MPLCONFIGDIR")


def _without(names: tuple[str, ...], call):
    """Run `call` with the named env vars removed (platformdirs' OS default)."""
    saved = {name: os.environ.pop(name) for name in names if name in os.environ}
    try:
        return call()
    finally:
        os.environ.update(saved)


def _unique(paths) -> list[Path]:
    out: list[Path] = []
    for path in paths:
        if path not in out:
            out.append(path)
    return out


def live_data_dirs() -> list[Path]:
    """Every place the live library can live: platformdirs' answer with and
    without XDG overrides, plus each OS's fixed default."""
    import platformdirs

    home = Path.home()
    dirs = [
        Path(platformdirs.user_data_dir(APP_NAME, APP_NAME)),
        _without(("XDG_DATA_HOME",), lambda: Path(platformdirs.user_data_dir(APP_NAME, APP_NAME))),
        home / "Library" / "Application Support" / APP_NAME,
        home / ".local" / "share" / APP_NAME,
        home / "AppData" / "Local" / APP_NAME / APP_NAME,
    ]
    if os.environ.get("LOCALAPPDATA"):
        dirs.append(Path(os.environ["LOCALAPPDATA"]) / APP_NAME / APP_NAME)
    return _unique(dirs)


def live_cache_dirs() -> list[Path]:
    """Where the live app keeps its cache (waveform-detail-v1): platformdirs'
    OS default and each OS's fixed default."""
    import platformdirs

    home = Path.home()
    dirs = [
        _without(("XDG_CACHE_HOME",), lambda: Path(platformdirs.user_cache_dir(APP_NAME, APP_NAME))),
        home / "Library" / "Caches" / APP_NAME,
        home / ".cache" / APP_NAME,
        home / "AppData" / "Local" / APP_NAME / APP_NAME / "Cache",
    ]
    return _unique(dirs)


def live_rekordbox_dirs() -> list[Path]:
    """rekordbox's own library folders (infra/rekordbox_grid.py master_db_path)."""
    dirs = [Path.home() / "Library" / "Pioneer", Path.home() / "AppData" / "Roaming" / "Pioneer"]
    if os.environ.get("APPDATA"):
        dirs.append(Path(os.environ["APPDATA"]) / "Pioneer")
    return _unique(dirs)


def _fold(path: Path) -> str:
    # APFS and NTFS are case-insensitive by default; compare conservatively.
    text = unicodedata.normalize("NFC", str(path))
    return text.casefold() if sys.platform in ("darwin", "win32") else text


def _inside(path: Path, root: Path) -> bool:
    candidate = _fold(path.expanduser().resolve())
    base = _fold(root.expanduser().resolve())
    return candidate == base or candidate.startswith(base.rstrip(os.sep) + os.sep)


def _refuse_inside(name: str, value: str, roots: list[Path]) -> None:
    for root in roots:
        if _inside(Path(value), root):
            sys.exit(f"refusing to start: {name}={value} is inside the live dir {root}")


def _refuse_case_variants(fields) -> None:
    upper = {field.upper() for field in fields}
    for name in os.environ:
        if name.upper() in upper and name != name.upper():
            sys.exit(f"refusing to start: {name} would override {name.upper()} "
                     "(pydantic-settings ignores case); unset it")


def require_sandbox_env(names=("DB_PATH", "USER_DATA_DIR"), required_rekordbox: bool = False
                        ) -> dict[str, Path]:
    """Exit unless every name is set and resolves outside the live data dirs.

    With `required_rekordbox`, PLUMDECK_REKORDBOX_DB must be set as well (a
    per-side copy, or a missing file for "no rekordbox").
    """
    _refuse_case_variants(SETTINGS_FIELDS)
    live = live_data_dirs()
    resolved: dict[str, Path] = {}
    for name in names:
        value = os.environ.get(name)
        if not value:
            sys.exit(f"refusing to start: {name} must be set to a sandbox path")
        _refuse_inside(name, value, live)
        resolved[name] = Path(value)
    for name in WRITTEN_SETTINGS:
        if os.environ.get(name):
            _refuse_inside(name, os.environ[name], live)
    rekordbox = os.environ.get("PLUMDECK_REKORDBOX_DB")
    if required_rekordbox and not rekordbox:
        sys.exit("refusing to start: PLUMDECK_REKORDBOX_DB must point at this side's copy of "
                 "master.db (or a missing file for no rekordbox)")
    if rekordbox:
        _refuse_inside("PLUMDECK_REKORDBOX_DB", rekordbox, live + live_rekordbox_dirs())
    return resolved


def sandbox_cache(root: Path) -> Path:
    """Point platformdirs' cache at `root` (unless the caller already set
    XDG_CACHE_HOME) and return the app cache dir there. platformdirs honours
    XDG_CACHE_HOME on macOS and Linux only, so callers that import the backend
    also redirect waveform_detail_service._cache_dir to this dir."""
    os.environ.setdefault("XDG_CACHE_HOME", str(root))
    cache = Path(os.environ["XDG_CACHE_HOME"]) / APP_NAME
    _refuse_inside("XDG_CACHE_HOME", str(cache), live_cache_dirs())
    return cache


def use_backend() -> None:
    """Make `import main` and friends resolve to the repo's backend package."""
    backend = str(BACKEND_DIR)
    if backend not in sys.path:
        sys.path.insert(0, backend)


def require_sandbox_settings():
    """Import backend/config.py and exit unless every path it resolved is
    outside the live data dirs. Call after use_backend() and the chdir that
    keeps a developer's ./.env away, before importing main."""
    import config

    _refuse_case_variants(config.Settings.model_fields)
    live = live_data_dirs()
    for name in WRITTEN_SETTINGS:
        value = getattr(config.settings, name, None)
        if not value:
            sys.exit(f"refusing to start: settings.{name} is empty")
        _refuse_inside(f"settings.{name}", str(value), live)
    return config.settings


def sandbox_env(root: Path) -> None:
    """Point every backend location at a fresh sandbox under `root` (for the
    one-shot helpers that import the backend)."""
    os.environ["USER_DATA_DIR"] = str(root / "ud")
    os.environ["DB_PATH"] = str(root / "ud" / "plumdeck.duckdb")
    os.environ["PLUMDECK_LOG_DIR"] = str(root / "ud" / "logs")
    os.environ["XDG_CACHE_HOME"] = str(root / "cache")
    # A missing file: the backend sees no rekordbox library.
    os.environ["PLUMDECK_REKORDBOX_DB"] = str(root / "rekordbox" / "master.db")
