"""Keys stored in track_analyses.features_extra_json.

Analyzer results carry more: beat/waveform arrays (stored as BLOBs) and
diagnostic keys asserted by the packaged self-tests. Only keys that something
reads from the saved row are persisted.
"""
from typing import Any, Dict

PERSISTED_KEYS = (
    "analysis_components",
    "playback_grid",
    "playback_grid_version",
    # plumdeck's own BPM and key kept beside a manual or rekordbox one, so
    # turning the rekordbox key source off can restore own keys. The old
    # bpm_authority marker is rewritten to analysis_components.sources.bpm.
    "analysis_bpm",
    "analysis_key",
    # Analyzer diagnostics with readers: the library CSV export writes all
    # three, and the fallback grid reader passes bpm_confidence on.
    "bpm_confidence",
    "key_strength",
    "bpm_raw",
)


def persisted_features_extra(extra: Dict[str, Any]) -> Dict[str, Any]:
    """Return a copy limited to PERSISTED_KEYS; the input is not modified."""
    return {key: extra[key] for key in PERSISTED_KEYS if key in extra}
