"""Shared guards for the parity helpers.

Every helper here imports the real Python backend, which opens DB_PATH and
writes under USER_DATA_DIR. They must only ever run against sandbox copies,
never against the owner's live library.
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


def real_user_data_dir() -> Path:
    """The live data dir, resolved like backend/config.py (platformdirs)."""
    import platformdirs

    return Path(platformdirs.user_data_dir(APP_NAME, APP_NAME))


def _fold(path: Path) -> str:
    # APFS and NTFS are case-insensitive by default; compare conservatively.
    text = unicodedata.normalize("NFC", str(path))
    return text.casefold() if sys.platform in ("darwin", "win32") else text


def _inside(path: Path, root: Path) -> bool:
    candidate = _fold(path.expanduser().resolve())
    base = _fold(root.expanduser().resolve())
    return candidate == base or candidate.startswith(base.rstrip(os.sep) + os.sep)


def require_sandbox_env(names=("DB_PATH", "USER_DATA_DIR")) -> dict[str, Path]:
    """Exit unless every name is set and resolves outside the live data dir."""
    real = real_user_data_dir()
    resolved: dict[str, Path] = {}
    for name in names:
        value = os.environ.get(name)
        if not value:
            sys.exit(f"refusing to start: {name} must be set to a sandbox path")
        path = Path(value)
        if _inside(path, real):
            sys.exit(f"refusing to start: {name}={value} is inside the live data dir {real}")
        resolved[name] = path
    for name in ("PLUMDECK_LOG_DIR", "PLUMDECK_REKORDBOX_DB"):
        value = os.environ.get(name)
        if value and _inside(Path(value), real):
            sys.exit(f"refusing to start: {name}={value} is inside the live data dir {real}")
    return resolved


def use_backend() -> None:
    """Make `import main` and friends resolve to the repo's backend package."""
    backend = str(BACKEND_DIR)
    if backend not in sys.path:
        sys.path.insert(0, backend)
