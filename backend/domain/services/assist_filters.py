"""Compound search conditions for assist mode.

Conditions combine with AND across fields and OR inside one field ("genre is
House or Disco, and BPM is 120-126"). They are hard filters: a track whose
value is unknown for a constrained field never matches, because "we do not know
its BPM" must not quietly pass as "its BPM is in range".
"""
from __future__ import annotations

from dataclasses import dataclass, field
import math
from typing import Any, Iterable, Optional
import unicodedata

from utils.audio_math import normalize_key

MAX_VALUES = 32


def normalize_text(value: Any) -> str:
    """Case, width and whitespace folding; same rule as deck identity matching."""
    if not value:
        return ""
    folded = unicodedata.normalize("NFKC", str(value)).casefold()
    return " ".join(folded.split())


def _number(value: Any) -> Optional[float]:
    if isinstance(value, bool) or value is None:
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def _texts(values: Optional[Iterable[Any]]) -> tuple[str, ...]:
    """Distinct non-empty values, deduplicated by their folded form."""
    seen: dict[str, str] = {}
    for value in values or []:
        text = " ".join(str(value).split()) if value is not None else ""
        if text:
            seen.setdefault(normalize_text(text), text)
    return tuple(seen.values())[:MAX_VALUES]


def _folded(values: Iterable[str]) -> frozenset[str]:
    return frozenset(normalize_text(value) for value in values)


@dataclass(frozen=True)
class AssistFilters:
    bpm_min: Optional[float] = None
    bpm_max: Optional[float] = None
    genres: tuple[str, ...] = field(default_factory=tuple)
    subgenres: tuple[str, ...] = field(default_factory=tuple)
    artists: tuple[str, ...] = field(default_factory=tuple)
    keys: tuple[str, ...] = field(default_factory=tuple)
    year_min: Optional[int] = None
    year_max: Optional[int] = None
    query: str = ""

    @classmethod
    def from_dict(cls, raw: Optional[dict[str, Any]]) -> "AssistFilters":
        raw = raw or {}
        keys = []
        for value in raw.get("keys") or []:
            key = normalize_key(value if isinstance(value, str) else None)
            if key is None:
                raise ValueError(f"キー「{value}」を解釈できません（例: 8A, A minor）")
            keys.append(key)
        bpm_min, bpm_max = _number(raw.get("bpm_min")), _number(raw.get("bpm_max"))
        year_min, year_max = _number(raw.get("year_min")), _number(raw.get("year_max"))
        if bpm_min is not None and bpm_max is not None and bpm_min > bpm_max:
            bpm_min, bpm_max = bpm_max, bpm_min
        if year_min is not None and year_max is not None and year_min > year_max:
            year_min, year_max = year_max, year_min
        return cls(
            bpm_min=bpm_min,
            bpm_max=bpm_max,
            genres=_texts(raw.get("genres")),
            subgenres=_texts(raw.get("subgenres")),
            artists=_texts(raw.get("artists")),
            keys=tuple(dict.fromkeys(keys))[:MAX_VALUES],
            year_min=int(year_min) if year_min is not None else None,
            year_max=int(year_max) if year_max is not None else None,
            query=" ".join(str(raw.get("query") or "").split())[:200],
        )

    def is_empty(self) -> bool:
        return not (
            self.bpm_min is not None or self.bpm_max is not None or self.genres
            or self.subgenres or self.artists or self.keys
            or self.year_min is not None or self.year_max is not None or self.query
        )

    def matches(self, track: Any) -> bool:
        """`track` is a Track, a row or a dict with the usual track columns."""
        value = _getter(track)
        if self.bpm_min is not None or self.bpm_max is not None:
            bpm = _number(value("bpm"))
            if bpm is None or bpm <= 0:
                return False
            if self.bpm_min is not None and bpm < self.bpm_min:
                return False
            if self.bpm_max is not None and bpm > self.bpm_max:
                return False
        if self.genres and normalize_text(value("genre")) not in _folded(self.genres):
            return False
        if self.subgenres and normalize_text(value("subgenre")) not in _folded(self.subgenres):
            return False
        if self.artists:
            artist = normalize_text(value("artist"))
            if not artist or not any(normalize_text(wanted) in artist for wanted in self.artists):
                return False
        if self.keys and normalize_key(value("key") if isinstance(value("key"), str) else None) not in self.keys:
            return False
        if self.year_min is not None or self.year_max is not None:
            year = _number(value("year"))
            if year is None or year <= 0:
                return False
            if self.year_min is not None and year < self.year_min:
                return False
            if self.year_max is not None and year > self.year_max:
                return False
        if self.query:
            haystack = " ".join(normalize_text(value(name)) for name in ("title", "artist", "album"))
            if not all(word in haystack for word in normalize_text(self.query).split()):
                return False
        return True

    def describe(self) -> list[str]:
        """Human-readable conditions, in the order the UI shows them."""
        parts = []
        if self.bpm_min is not None or self.bpm_max is not None:
            low = f"{self.bpm_min:g}" if self.bpm_min is not None else ""
            high = f"{self.bpm_max:g}" if self.bpm_max is not None else ""
            parts.append(f"BPM {low}〜{high}")
        if self.genres:
            parts.append("ジャンル " + " / ".join(self.genres))
        if self.subgenres:
            parts.append("サブジャンル " + " / ".join(self.subgenres))
        if self.artists:
            parts.append("アーティスト " + " / ".join(self.artists))
        if self.keys:
            parts.append("キー " + " / ".join(self.keys))
        if self.year_min is not None or self.year_max is not None:
            low = str(self.year_min) if self.year_min is not None else ""
            high = str(self.year_max) if self.year_max is not None else ""
            parts.append(f"{low}〜{high}年")
        if self.query:
            parts.append(f"「{self.query}」を含む")
        return parts

    def to_dict(self) -> dict[str, Any]:
        return {
            "bpm_min": self.bpm_min,
            "bpm_max": self.bpm_max,
            "genres": list(self.genres),
            "subgenres": list(self.subgenres),
            "artists": list(self.artists),
            "keys": list(self.keys),
            "year_min": self.year_min,
            "year_max": self.year_max,
            "query": self.query,
        }


def _getter(track: Any):
    if isinstance(track, dict):
        return track.get
    return lambda name: getattr(track, name, None)
