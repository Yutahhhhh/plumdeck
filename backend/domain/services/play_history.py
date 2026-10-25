"""What the DJ has actually played, as evidence for the next suggestion.

Analysed features say whether two tracks *can* mix; the DJ's own rekordbox
HISTORY says which ones they *do* reach for. Replaying a DJ's earlier sets
against later ones, three signals predicted the real next track far better than
any feature weighting: the candidate followed this exact track before, it was
played within a few positions of it, and it is in the DJ's current rotation.

Tracks are keyed by recording identity (normalized title and artist), so a
second file of the same recording shares its history. Sessions started within
the last few hours are left out: that is the set being played right now, whose
tracks are already excluded and must not vouch for each other.
"""
from __future__ import annotations

from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime, timedelta
import math
from typing import Hashable, Iterable, Optional, Sequence

Identity = Hashable

# Positions apart that still count as "played near each other" in one set.
NEAR_WINDOW = 3
# Sessions shorter than this are previews or practice, not sets.
MIN_SESSION_TRACKS = 8
ACTIVE_SESSION_WINDOW = timedelta(hours=12)
ROTATION_DECAY_DAYS = 60.0
# Fitted on time-ordered history (earlier sets predicting later ones) and
# squashed so the affinity stays in 0..1 beside a preset score.
FOLLOWED_WEIGHT = 1.4
NEARBY_WEIGHT = 2.8
ROTATION_WEIGHT = 2.1
PLAYED_WEIGHT = 0.9
PLAYS_WEIGHT = 0.4
AFFINITY_SCALE = 3.0


@dataclass(frozen=True)
class Evidence:
    followed: int  # sets where the candidate came right after the source
    nearby: int  # times played within NEAR_WINDOW positions of the source
    plays: int  # sets that played the candidate at all
    days_since: Optional[float]  # since the candidate was last played
    affinity: float

    @property
    def related(self) -> bool:
        return self.followed > 0 or self.nearby > 0

    def to_dict(self) -> dict:
        return {
            "followed": self.followed,
            "nearby": self.nearby,
            "plays": self.plays,
            "days_since": round(self.days_since, 1) if self.days_since is not None else None,
            "affinity": round(self.affinity, 4),
        }


class PlayHistory:
    def __init__(
        self,
        sessions: Iterable[tuple[Optional[datetime], Sequence[Optional[Identity]]]],
        now: datetime,
    ):
        self.now = now
        self._followed: dict[Identity, Counter] = defaultdict(Counter)
        self._nearby: dict[Identity, Counter] = defaultdict(Counter)
        self._plays: Counter = Counter()
        self._last: dict[Identity, datetime] = {}
        for started_at, tracks in sessions:
            if len(tracks) < MIN_SESSION_TRACKS:
                continue
            if started_at is not None and now - started_at < ACTIVE_SESSION_WINDOW:
                continue
            self._add(started_at, list(tracks))

    def _add(self, started_at: Optional[datetime], tracks: list[Optional[Identity]]) -> None:
        for previous, current in zip(tracks, tracks[1:]):
            if previous is not None and current is not None and previous != current:
                self._followed[previous][current] += 1
        for position, current in enumerate(tracks):
            if current is None:
                continue
            for other in tracks[max(0, position - NEAR_WINDOW):position]:
                if other is not None and other != current:
                    self._nearby[current][other] += 1
                    self._nearby[other][current] += 1
        for identity in {track for track in tracks if track is not None}:
            self._plays[identity] += 1
            if started_at is not None and (identity not in self._last or started_at > self._last[identity]):
                self._last[identity] = started_at

    def related(self, source: Identity) -> list[Identity]:
        """Tracks played next to the source before, most often first."""
        counts = Counter(self._nearby.get(source, {}))
        counts.update(self._followed.get(source, {}))
        return [identity for identity, _ in sorted(counts.items(), key=lambda item: (-item[1], str(item[0])))]

    def evidence(self, source: Optional[Identity], candidate: Optional[Identity]) -> Optional[Evidence]:
        if candidate is None:
            return None
        followed = self._followed.get(source, {}).get(candidate, 0) if source is not None else 0
        nearby = self._nearby.get(source, {}).get(candidate, 0) if source is not None else 0
        plays = self._plays.get(candidate, 0)
        if not (followed or nearby or plays):
            return None
        last = self._last.get(candidate)
        days = max(0.0, (self.now - last).total_seconds() / 86400) if last is not None else None
        rotation = math.exp(-days / ROTATION_DECAY_DAYS) if days is not None else 0.0
        strength = (
            FOLLOWED_WEIGHT * math.log1p(followed)
            + NEARBY_WEIGHT * math.log1p(nearby)
            + ROTATION_WEIGHT * rotation
            + PLAYED_WEIGHT * (plays > 0)
            + PLAYS_WEIGHT * math.log1p(plays)
        )
        return Evidence(followed, nearby, plays, days, 1.0 - math.exp(-strength / AFFINITY_SCALE))


def describe(evidence: Optional[Evidence]) -> Optional[str]:
    """One line a DJ can check against their own memory of the sets."""
    if evidence is None:
        return None
    if evidence.followed:
        head = f"過去のセットでこの曲の直後に {evidence.followed} 回掛けています"
    elif evidence.nearby:
        head = f"過去のセットでこの曲の近くで {evidence.nearby} 回掛けています"
    else:
        head = f"過去 {evidence.plays} 回のセットで使った曲です"
    if evidence.days_since is not None:
        head += f"（最後は{_ago(evidence.days_since)}）"
    return head


def headline(evidence: Evidence) -> str:
    """The short form that leads a suggestion's summary."""
    if evidence.followed:
        return f"実績：直後に{evidence.followed}回"
    return f"実績：近くで{evidence.nearby}回"


def _ago(days: float) -> str:
    if days < 1:
        return "今日"
    if days < 60:
        return f"{int(days)}日前"
    if days < 730:
        return f"{int(days // 30)}か月前"
    return f"{int(days // 365)}年前"
