"""Keys stored in track_analyses.features_extra_json.

Analyzer results carry more: beat/waveform arrays (stored as BLOBs) and
diagnostic keys asserted by the packaged self-tests. Neither has a reader in
the saved row, so only these keys are persisted.
"""
from typing import Any, Dict

PERSISTED_KEYS = (
    "analysis_components",
    "playback_grid",
    "playback_grid_version",
    # Rekordbox-synced BPM marker and the analyzer's own value kept beside it.
    "bpm_authority",
    "analysis_bpm",
)


def persisted_features_extra(extra: Dict[str, Any]) -> Dict[str, Any]:
    """Return a copy limited to PERSISTED_KEYS; the input is not modified."""
    return {key: extra[key] for key in PERSISTED_KEYS if key in extra}
