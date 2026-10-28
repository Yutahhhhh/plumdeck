import math
import re
from typing import Optional, Dict, List

# Camelot Wheel Adjacency Map (Harmonic Mixing Rules)
CAMELOT_ADJACENCY: Dict[str, List[str]] = {
    "1A": ["1A", "1B", "2A", "12A"],
    "1B": ["1B", "1A", "2B", "12B"],
    "2A": ["2A", "2B", "3A", "1A"],
    "2B": ["2B", "2A", "3B", "1B"],
    "3A": ["3A", "3B", "4A", "2A"],
    "3B": ["3B", "3A", "4B", "2B"],
    "4A": ["4A", "4B", "5A", "3A"],
    "4B": ["4B", "4A", "5B", "3B"],
    "5A": ["5A", "5B", "6A", "4A"],
    "5B": ["5B", "5A", "6B", "4B"],
    "6A": ["6A", "6B", "7A", "5A"],
    "6B": ["6B", "6A", "7B", "5B"],
    "7A": ["7A", "7B", "8A", "6A"],
    "7B": ["7B", "7A", "8B", "6B"],
    "8A": ["8A", "8B", "9A", "7A"],
    "8B": ["8B", "8A", "9B", "7B"],
    "9A": ["9A", "9B", "10A", "8A"],
    "9B": ["9B", "9A", "10B", "8B"],
    "10A": ["10A", "10B", "11A", "9A"],
    "10B": ["10B", "10A", "11B", "9B"],
    "11A": ["11A", "11B", "12A", "10A"],
    "11B": ["11B", "11A", "12B", "10B"],
    "12A": ["12A", "12B", "1A", "11A"],
    "12B": ["12B", "12A", "1B", "11B"],
}

KEY_TO_CAMELOT = {
    "C Major": "8B", "C Minor": "5A",
    "C# Major": "3B", "Db Major": "3B", "C# Minor": "12A",
    "D Major": "10B", "D Minor": "7A",
    "D# Major": "5B", "Eb Major": "5B", "D# Minor": "2A", "Eb Minor": "2A",
    "E Major": "12B", "E Minor": "9A",
    "F Major": "7B", "F Minor": "4A",
    "F# Major": "2B", "Gb Major": "2B", "F# Minor": "11A",
    "G Major": "9B", "G Minor": "6A",
    "G# Major": "4B", "Ab Major": "4B", "G# Minor": "1A",
    "A Major": "11B", "A Minor": "8A",
    "A# Major": "6B", "Bb Major": "6B", "A# Minor": "3A", "Bb Minor": "3A",
    "B Major": "1B", "B Minor": "10A",
    "Db Minor": "12A", "Gb Minor": "11A", "Ab Minor": "1A",
    "Cb Major": "1B", "Cb Minor": "10A",
    "E# Major": "7B", "E# Minor": "4A", "Fb Major": "12B", "Fb Minor": "9A",
    "B# Major": "8B", "B# Minor": "5A",
}

def normalize_key(key_str: Optional[str]) -> Optional[str]:
    """Normalize key string to Camelot format (e.g. '8A')."""
    if not isinstance(key_str, str) or not key_str.strip():
        return None
    key_str = key_str.strip().replace("♯", "#").replace("♭", "b")
    camelot = key_str.upper()
    if camelot in CAMELOT_ADJACENCY:
        return camelot

    # Match the entire key: substring matching confuses 'Db minor' with 'B Minor'.
    match = re.fullmatch(r"([A-Ga-g])([#b]?)\s*(major|minor|maj|min|m)?", key_str, re.IGNORECASE)
    if not match:
        return None
    note, accidental, scale = match.groups()
    mode = "Minor" if scale and scale != "M" and scale.lower() in ("minor", "min", "m") else "Major"
    return KEY_TO_CAMELOT.get(f"{note.upper()}{accidental.lower()} {mode}")


# The spelling tracks.key stores: what the key detectors emit ('F minor',
# 'C# minor', 'Eb major'), so sorting, filters and KEY SYNC see one form.
CAMELOT_TO_KEY = {
    "1A": "Ab minor", "1B": "B major", "2A": "Eb minor", "2B": "F# major",
    "3A": "Bb minor", "3B": "C# major", "4A": "F minor", "4B": "Ab major",
    "5A": "C minor", "5B": "Eb major", "6A": "G minor", "6B": "Bb major",
    "7A": "D minor", "7B": "F major", "8A": "A minor", "8B": "C major",
    "9A": "E minor", "9B": "G major", "10A": "B minor", "10B": "D major",
    "11A": "F# minor", "11B": "A major", "12A": "C# minor", "12B": "E major",
}


def key_name(key_str: Optional[str]) -> Optional[str]:
    """Spell any accepted key ('Fm', 'Dbm', '4A') as stored ('F minor'); None if unreadable."""
    return CAMELOT_TO_KEY.get(normalize_key(key_str) or "")


def bpm_distance(target_bpm: float, candidate_bpm: float) -> float:
    """Symmetric tempo distance, allowing one half/double-time interpretation.

    Unknown tempo has no evidence of compatibility; never substitute 120 BPM.
    """
    try:
        target, candidate = float(target_bpm), float(candidate_bpm)
    except (TypeError, ValueError):
        return math.inf
    if not math.isfinite(target) or not math.isfinite(candidate) or min(target, candidate) <= 0:
        return math.inf
    ratio = math.log2(candidate) - math.log2(target)
    return min(abs(ratio + shift) for shift in (-1, 0, 1))

def calculate_mixability_score(
    target_bpm: float, 
    target_key: Optional[str], 
    candidate_bpm: float, 
    candidate_key: Optional[str],
    vector_similarity: float = 0.0,
    weights: dict = None
) -> float:
    """
    Calculate a mixability score (0.0 - 1.0) between two tracks.
    """
    w = weights or {"bpm": 0.35, "key": 0.25, "vector": 0.4}
    
    # 1. BPM Score
    # Log ratios make A -> B and B -> A equally compatible.
    distance = bpm_distance(target_bpm, candidate_bpm)
    bpm_score = math.exp(-0.5 * (distance / math.log2(1.08)) ** 2)
    
    # 2. Key Score
    norm_t = normalize_key(target_key)
    norm_c = normalize_key(candidate_key)
    key_score = 0.0
    
    if norm_t and norm_c:
        if norm_t == norm_c:
            key_score = 1.0
        elif norm_c in CAMELOT_ADJACENCY.get(norm_t, []):
            key_score = 0.9
        else:
            key_score = 0.1
    else:
        key_score = 0.5
        
    # 3. Vector Score
    vec_score = max(0.0, min(1.0, vector_similarity))
    
    final_score = (bpm_score * w["bpm"]) + (key_score * w["key"]) + (vec_score * w["vector"])
    return final_score
