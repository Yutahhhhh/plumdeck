"""Order a set that moves through named phases (展開).

The MCP client decides the story of the night: which pools belong to which
phase, how many tracks each phase gets and where energy/tempo should head.
This builder only does the deterministic part: pick and order tracks so that
every transition mixes well, each phase follows its energy/tempo ramp, the
DJ's must-play tracks are placed, and phase boundaries land on the smoothest
available switch.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import math
import re
import unicodedata
from typing import Any, Optional

from domain.services.setlist_builder import APPROVED_WORDPLAY_BONUS
from utils.audio_math import (
    bpm_distance, calculate_mixability_score, normalize_key, CAMELOT_ADJACENCY, SET_TRANSITION_WEIGHTS,
)
from utils.embedding import cosine_similarity, neutral_similarity

# Weights are relative to calculate_mixability_score, which is 0..1.
ENERGY_WEIGHT = 0.6
TEMPO_WEIGHT = 1.5
ANCHOR_BONUS = 0.3
# Tracks picked by the DJ (playlist, favourite artist) outrank sound-alikes.
SOURCE_BONUS = {"anchor": 0.0, "track": 0.12, "playlist": 0.12, "artist": 0.12,
                "similar": 0.0, "filter": 0.0, "wordplay": 0.0}
# A phase switch is where a set usually breaks, so transition quality counts more.
PHASE_SWITCH_MIX_WEIGHT = 1.5
SAME_ARTIST_PENALTY = (0.35, 0.15)  # previous track, the one before it
TRANSITION_WEIGHTS = SET_TRANSITION_WEIGHTS


@dataclass
class Phase:
    label: str
    count: int
    pool: list[dict[str, Any]]
    anchor_ids: list[int] = field(default_factory=list)
    energy: Optional[tuple[float, float]] = None
    bpm: Optional[tuple[float, float]] = None
    keep_order: bool = False


def _ramp(bounds: Optional[tuple[float, float]], progress: float) -> Optional[float]:
    if not bounds:
        return None
    start, end = bounds
    return start + (end - start) * progress


def key_relation(previous_key: Optional[str], key: Optional[str]) -> str:
    left, right = normalize_key(previous_key), normalize_key(key)
    if not left or not right:
        return "unknown"
    if left == right:
        return "same"
    return "compatible" if right in CAMELOT_ADJACENCY.get(left, []) else "clash"


def _similarity(previous: dict[str, Any], candidate: dict[str, Any]) -> Optional[float]:
    return cosine_similarity(previous.get("vector"), candidate.get("vector"),
                             previous.get("embedding_model"), candidate.get("embedding_model"))


def transition_score(previous: dict[str, Any], candidate: dict[str, Any],
                     neutral: Optional[float] = None) -> float:
    """``neutral`` stands in for a pending embedding (see neutral_similarity)."""
    return calculate_mixability_score(
        target_bpm=previous["track"].bpm, target_key=previous["track"].key,
        candidate_bpm=candidate["track"].bpm, candidate_key=candidate["track"].key,
        vector_similarity=_similarity(previous, candidate), weights=TRANSITION_WEIGHTS,
        neutral_similarity=neutral,
    )


_ARTIST_SPLIT = re.compile(r"\s*(?:,|&|/|;|\bx\b|\bvs\.?|\bfeat\.?|\bft\.?|\bfeaturing\b|\bwith\b|\band\b)\s*")
# "(Dirty)", "[Intro]", "- DJ X Remix", "A vs B" mashups: edits of one song
# still count as that song.
_TITLE_NOISE = re.compile(r"\s*[\(\[【].*?[\)\]】]|\s+-\s+.*$|\s+vs\.?\s+.*$")


def _fold(value: Optional[str]) -> str:
    return " ".join(unicodedata.normalize("NFKC", value or "").casefold().split())


def _artists(node: dict[str, Any]) -> frozenset[str]:
    names = _ARTIST_SPLIT.split(_fold(node["track"].artist))
    return frozenset(name for name in names if name)


def _core_title(node: dict[str, Any]) -> str:
    return _fold(_TITLE_NOISE.sub("", _fold(node["track"].title))) or _fold(node["track"].title)


def _same_song(left: dict[str, Any], right: dict[str, Any]) -> bool:
    """Two edits/versions of one song: same core title and a shared artist."""
    title = _core_title(left)
    return bool(title) and title == _core_title(right) and bool(_artists(left) & _artists(right))


class PhasedSetlistBuilder:
    def __init__(self, wordplay_bonus: float = APPROVED_WORDPLAY_BONUS):
        self._wordplay_bonus = wordplay_bonus

    def build(
        self,
        phases: list[Phase],
        approved_wordplay_edges=(),
        opening: Optional[dict[str, Any]] = None,
    ) -> tuple[list[dict[str, Any]], list[str]]:
        """Return (ordered placements, notes).

        Each placement is {"node", "phase", "phase_index", "score", "transition"}.
        ``opening`` is a track already playing before the set (not returned).
        """
        edges = set(approved_wordplay_edges or ())
        placements: list[dict[str, Any]] = []
        notes: list[str] = []
        used_ids: set[int] = set()
        history: list[dict[str, Any]] = [opening] if opening else []
        if opening:
            used_ids.add(opening["id"])

        for phase_index, phase in enumerate(phases):
            by_id: dict[int, dict[str, Any]] = {}
            for node in phase.pool:
                by_id.setdefault(node["id"], node)
            anchors = [by_id[tid] for tid in dict.fromkeys(phase.anchor_ids)
                       if tid in by_id and tid not in used_ids]
            count = max(phase.count, len(anchors))
            placed = 0
            for slot in range(count):
                progress = slot / (count - 1) if count > 1 else 0.0
                previous = history[-1] if history else None
                remaining_anchors = [node for node in anchors if node["id"] not in used_ids]

                def eligible(node: dict[str, Any]) -> bool:
                    if node["id"] in used_ids:
                        return False
                    if any(_same_song(node, placed_node) for placed_node in history):
                        return False
                    only_from = node.get("wordplay_only_from_ids")
                    return not only_from or (previous is not None and previous["id"] in only_from)

                if phase.keep_order:
                    ordered = [node for node in phase.pool if eligible(node)]
                    choices = ordered[:1]
                elif remaining_anchors and len(remaining_anchors) >= count - slot:
                    # Every remaining slot is needed for a must-play track.
                    choices = [node for node in remaining_anchors if eligible(node)] or remaining_anchors
                else:
                    choices = [node for node in by_id.values() if eligible(node)]
                if not choices:
                    break

                neutral = (neutral_similarity(_similarity(previous, node) for node in choices)
                           if previous is not None else None)
                best = max(
                    choices,
                    key=lambda node: (
                        self._score(node, previous, history, phase, progress, slot, anchors, edges, neutral),
                        -node["id"],
                    ),
                )
                score = self._score(best, previous, history, phase, progress, slot, anchors, edges, neutral)
                placements.append({
                    "node": best,
                    "phase": phase.label,
                    "phase_index": phase_index,
                    "phase_switch": slot == 0 and bool(history),
                    "score": round(score, 4),
                    "transition": self._describe(previous, best, edges, neutral),
                })
                used_ids.add(best["id"])
                history.append(best)
                placed += 1

            if placed < phase.count:
                notes.append(
                    f"「{phase.label}」は候補が足りず {placed}/{phase.count} 曲しか配置できませんでした"
                )
            missing = [node for node in anchors if node["id"] not in used_ids]
            if missing:
                notes.append(f"「{phase.label}」の必須曲 {len(missing)} 曲を配置できませんでした")
        return placements, notes

    def _score(self, node, previous, history, phase, progress, slot, anchors, edges, neutral=None) -> float:
        track = node["track"]
        score = 0.0
        if previous is not None:
            mix = transition_score(previous, node, neutral)
            score += mix * (PHASE_SWITCH_MIX_WEIGHT if slot == 0 else 1.0)
            if (previous["id"], node["id"]) in edges:
                score += self._wordplay_bonus
            artists = _artists(node)
            if artists:
                for back, penalty in enumerate(SAME_ARTIST_PENALTY, start=1):
                    if len(history) >= back and artists & _artists(history[-back]):
                        score -= penalty
        target_energy = _ramp(phase.energy, progress)
        if target_energy is not None:
            energy = track.energy
            score -= ENERGY_WEIGHT * (abs(energy - target_energy) if energy is not None else 0.5)
        target_bpm = _ramp(phase.bpm, progress)
        if target_bpm is not None:
            distance = bpm_distance(target_bpm, track.bpm)
            score -= TEMPO_WEIGHT * (distance if math.isfinite(distance) else 0.3)
        if any(anchor["id"] == node["id"] for anchor in anchors):
            score += ANCHOR_BONUS
        sources = node.get("sources") or ()
        score += max((SOURCE_BONUS.get(source.get("type"), 0.0) for source in sources), default=0.0)
        return score

    @staticmethod
    def _describe(previous, node, edges, neutral=None) -> Optional[dict[str, Any]]:
        if previous is None:
            return None
        before, after = previous["track"], node["track"]
        bpm_diff = None
        if before.bpm and after.bpm:
            bpm_diff = round(after.bpm - before.bpm, 1)
        return {
            "from_track_id": previous["id"],
            "bpm_diff": bpm_diff,
            "key_relation": key_relation(before.key, after.key),
            "mix_score": round(transition_score(previous, node, neutral), 3),
            "approved_wordplay": (previous["id"], node["id"]) in edges,
        }
