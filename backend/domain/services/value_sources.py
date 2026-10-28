"""Where tracks.bpm and tracks.key come from: manual > rekordbox > own analysis.

Every save that carries an analysis result resolves both fields with the same
rule, reading the rekordbox collection at that moment, so a track rekordbox
analyses later is picked up on its next save. The chosen source is recorded in
features_extra.analysis_components.sources. Manual values come from the CSV
metadata import. The legacy ``bpm_authority`` marker, which the library rebuild
rewrites to a recorded source, still counts as a rekordbox BPM until then.
"""
from __future__ import annotations

from dataclasses import dataclass
import logging
import math
from typing import Any

from infra import rekordbox_library
from utils.audio_math import key_name

logger = logging.getLogger(__name__)

MANUAL, REKORDBOX, OWN = "manual", "rekordbox", "own"
FIELDS = ("bpm", "key")

# Owner decision: rekordbox replaces plumdeck's own key detection only after a
# replay evaluation shows no regression for emotional, bright and wordplay. The
# rule fixed before the run (no MRR or hit@5 drop for emotional or bright) fails:
# over 600 HISTORY transitions (BPM synced in both arms) emotional dips (MRR
# .053 -> .050, hit@5 .077 -> .072), bright gains (MRR .083 -> .087), wordplay
# has no signal and pooled hit@5 drops (.0650 -> .0633). So the default stays
# off until the owner decides; the settings row overrides it.
KEY_SOURCE_SETTING = "rekordbox_key_source"
KEY_SOURCE_DEFAULT = False


@dataclass(frozen=True)
class RekordboxValues:
    """The collection's values for one file at save time."""

    readable: bool = False  # False when master.db is missing or unreadable
    bpm: float | None = None  # None when unregistered or rekordbox has no tempo
    key: str | None = None  # stored spelling ('F minor'); None when absent or not used

    def value(self, field: str):
        return self.bpm if field == "bpm" else self.key


def lookup(filepath: str, *, keys: bool) -> RekordboxValues:
    """Read rekordbox's BPM (and key, when ``keys``) for a file; never raises."""
    try:
        entry = rekordbox_library.collection_index().get(filepath)
    except Exception as error:  # master.db missing, locked or undecryptable
        logger.info("rekordbox values unavailable for %s: %s", filepath, error)
        return RekordboxValues()
    if entry is None:
        return RekordboxValues(readable=True)
    bpm = entry.bpm if entry.bpm and math.isfinite(entry.bpm) and entry.bpm > 0 else None
    return RekordboxValues(True, bpm, key_name(entry.key) if keys else None)


def key_source_enabled(session) -> bool:
    """Whether rekordbox's key outranks own detection (settings 'rekordbox_key_source')."""
    from domain.models.setting import Setting

    try:
        row = session.get(Setting, KEY_SOURCE_SETTING)
    except Exception:
        return KEY_SOURCE_DEFAULT
    return key_source_value(row.value if row is not None else None)


def key_source_value(value: Any) -> bool:
    """Read a stored 'rekordbox_key_source' value; anything unrecognised is the default."""
    value = str(value).strip().lower() if value is not None else ""
    if value in {"1", "true", "on", "yes"}:
        return True
    if value in {"0", "false", "off", "no"}:
        return False
    return KEY_SOURCE_DEFAULT


def valid(field: str, value: Any) -> bool:
    if field == "bpm":
        return (isinstance(value, (int, float)) and not isinstance(value, bool)
                and math.isfinite(value) and value > 0)
    return isinstance(value, str) and bool(value.strip()) and value.strip().lower() != "unknown"


def stored_sources(extra: Any) -> dict[str, str]:
    """Sources recorded in a features_extra dict, including the legacy BPM marker."""
    if not isinstance(extra, dict):
        return {}
    components = extra.get("analysis_components")
    recorded = components.get("sources") if isinstance(components, dict) else None
    sources = {field: source for field, source in (recorded or {}).items()
               if field in FIELDS and isinstance(source, str)} if isinstance(recorded, dict) else {}
    if extra.get("bpm_authority") and "bpm" not in sources:
        sources["bpm"] = REKORDBOX
    return sources


def choose(field: str, incoming: Any, incoming_source: str | None,
           stored: Any, stored_source: str | None,
           rekordbox: RekordboxValues) -> tuple[Any, str | None]:
    """(value, source) for one field.

    ``incoming`` is what this save brings: the analyzer's value, or a value the
    caller already took from rekordbox (``incoming_source`` REKORDBOX).
    ``stored`` is the row's current value. Returns (None, None) when there is
    no usable value, in which case the caller writes nothing.
    """
    if stored_source == MANUAL and valid(field, stored):
        return stored, MANUAL
    external = rekordbox.value(field)
    if external is not None:
        return external, REKORDBOX
    if incoming_source == REKORDBOX and valid(field, incoming):
        return incoming, REKORDBOX
    if stored_source == REKORDBOX and valid(field, stored):
        # master.db is offline, or rekordbox no longer has a value (the file
        # left the collection, another path spelling, BPM 0): keep the synced
        # value instead of an estimate.
        return stored, REKORDBOX
    if valid(field, incoming):
        return incoming, incoming_source or OWN
    if valid(field, stored):
        return stored, stored_source
    return None, None


def scale_of(key: str) -> str:
    """'F minor' -> 'minor' (tracks.scale mirrors the key's second word)."""
    parts = key.split()
    return parts[-1].lower() if len(parts) == 2 else ""


def with_sources(extra: dict, sources: dict[str, str]) -> dict:
    """Copy of ``extra`` whose analysis_components.sources includes ``sources``."""
    updated = dict(extra)
    components = dict(updated.get("analysis_components") or {})
    merged = {**stored_sources(updated), **sources}
    if merged:
        components["sources"] = merged
    updated["analysis_components"] = components
    if "bpm" in sources:
        # The recorded source replaces the one-off sync marker.
        updated.pop("bpm_authority", None)
    return updated
