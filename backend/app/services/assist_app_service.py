"""Assist mode: turn observed rekordbox decks into playable suggestions.

Two distinct jobs live here.

*Resolution* answers "which library track is actually on deck N". The deck header
only gives a displayed title and artist, and the process's open files include the
sampler banks and the metronome click, so neither alone is an identity. An open
collection file whose title and artist match is preferred. However, rekordbox
does not expose every loaded track as an open file handle on every build, so an
exact title-and-artist collection match is used as a fallback. If duplicate rows
have the same identity, one is chosen deterministically, preferring a file
already registered in plumdeck.

*Recommendation* ranks the library for a chosen intent, restricted to originals
rekordbox already knows about and that still exist on disk — a suggestion the DJ
cannot drag onto a deck is not a suggestion. Compound filters narrow the pool as
hard conditions, the chosen preset (keep, hype, dance...) decides which way the
floor should move, and the best pick of every other preset is shown alongside.
Transition mode looks for half/double-time partners and titled tempo-changing
edits instead of nearby tempos. "Like X" references exist for the chat agent's
tools only; judging resemblance properly needs the agent's reasoning.

*Search* applies the same filters and availability rule without a source deck.
"""
from __future__ import annotations

import json
import os
from typing import Any, Optional

import numpy as np
from sqlmodel import Session, select

from domain.models.lyrics import Lyrics
from domain.models.track import Track, TrackEmbedding
from domain.models.wordplay import WordplayPair
from domain.services import assist_recommendation as scoring
from domain.services.assist_filters import AssistFilters
from infra import rekordbox_library
from infra.rekordbox_library import RekordboxLibraryUnavailable
from infra.repositories.recommendation_repository import RecommendationRepository
from infra.repositories.track_repository import TrackRepository
from utils.embedding import cosine_similarity, embedding_space

# Deck resolution states, surfaced verbatim so the UI never has to guess either.
STATUS_EMPTY = "empty"
STATUS_RESOLVED = "resolved"
STATUS_AMBIGUOUS = "ambiguous"
STATUS_UNRESOLVED = "unresolved"
STATUS_NOT_IN_LIBRARY = "not_in_library"

MAX_CANDIDATE_POOL = 400
MAX_SUGGESTIONS = 50
MAX_ARTIST_ANCHORS = 40
MAX_ROUTE_INTERMEDIATE = 4
MAX_ROUTE_BEAM = 18
# Picks shown under "another preset" must genuinely suit that preset.
ALTERNATIVE_MIN_SCORE = 0.6


class AssistInputError(ValueError):
    """A request the caller can fix (bad filter value, unknown reference)."""


def _conditions(filters) -> AssistFilters:
    if isinstance(filters, AssistFilters):
        return filters
    try:
        return AssistFilters.from_dict(filters)
    except ValueError as error:
        raise AssistInputError(str(error)) from error


class AssistAppService:
    def __init__(self, session: Session):
        self.session = session
        self.tracks = TrackRepository(session)
        self.recommendations = RecommendationRepository(session)

    # ------------------------------------------------------------------ decks

    def resolve_decks(
        self, observations: list[dict[str, Any]], open_audio_paths: list[str]
    ) -> dict[str, Any]:
        """Match each observed deck to exactly one library track, or say why not."""
        library_error: Optional[str] = None
        entries: dict[str, rekordbox_library.RekordboxEntry] = {}
        identity_entries: dict[str, rekordbox_library.RekordboxEntry] = {}
        try:
            entries = rekordbox_library.lookup_by_paths(open_audio_paths)
            identity_entries = rekordbox_library.lookup_by_identities([
                (observation.get("title"), observation.get("artist"))
                for observation in observations
                if observation.get("loaded")
            ])
        except RekordboxLibraryUnavailable as error:
            library_error = str(error)

        all_entries = {**identity_entries, **entries}
        plumdeck_tracks = self._tracks_by_filepath(
            [entry.filepath for entry in all_entries.values()]
        )

        decks = []
        for observation in observations:
            decks.append(
                self._resolve_deck(
                    observation, entries, identity_entries, plumdeck_tracks, library_error
                )
            )
        return {
            "decks": decks,
            "library_error": library_error,
            "inspected_paths": len(open_audio_paths),
        }

    def _resolve_deck(
        self,
        observation: dict[str, Any],
        entries: dict[str, rekordbox_library.RekordboxEntry],
        identity_entries: dict[str, rekordbox_library.RekordboxEntry],
        plumdeck_tracks: dict[str, Track],
        library_error: Optional[str],
    ) -> dict[str, Any]:
        slot = int(observation.get("slot") or 0)
        base: dict[str, Any] = {
            "slot": slot,
            "observed": {
                "title": observation.get("title"),
                "artist": observation.get("artist"),
                "track_bpm": observation.get("track_bpm"),
                "tempo_bpm": observation.get("tempo_bpm"),
                "display_key": observation.get("display_key"),
            },
            "track": None,
            "rekordbox_id": None,
            "filepath": None,
            "match_confidence": None,
            "candidates": [],
        }

        if not observation.get("loaded") or not (observation.get("title") or "").strip():
            return {**base, "status": STATUS_EMPTY, "message": "曲が読み込まれていません"}

        if library_error:
            return {
                **base,
                "status": STATUS_UNRESOLVED,
                "message": f"{library_error}のため、曲を特定できません",
            }

        title = rekordbox_library.normalize_text(observation.get("title"))
        artist = rekordbox_library.normalize_text(observation.get("artist"))

        exact = [
            entry
            for entry in entries.values()
            if rekordbox_library.normalize_text(entry.title) == title
            and rekordbox_library.normalize_text(entry.artist) == artist
        ]
        if not exact:
            exact = [
                entry
                for entry in identity_entries.values()
                if rekordbox_library.normalize_text(entry.title) == title
                and rekordbox_library.normalize_text(entry.artist) == artist
            ]
        confidence = "title_and_artist"

        if not exact:
            return {
                **base,
                "status": STATUS_UNRESOLVED,
                "message": (
                    "デッキの曲に対応するファイルが rekordbox の開いているファイルの中に"
                    "見つかりませんでした"
                ),
            }

        # Equal title/artist rows represent the same recording identity for
        # Assist. Prefer a usable plumdeck file, then make the choice stable.
        entry = min(
            exact,
            key=lambda candidate: (
                candidate.filepath not in plumdeck_tracks,
                rekordbox_library.normalize_path(candidate.filepath),
                candidate.content_id,
            ),
        )
        track = plumdeck_tracks.get(entry.filepath)
        if track is None:
            return {
                **base,
                "status": STATUS_NOT_IN_LIBRARY,
                "rekordbox_id": entry.content_id,
                "filepath": entry.filepath,
                "match_confidence": confidence,
                "message": "この曲は plumdeck のライブラリに未登録のため、提案できません",
            }

        return {
            **base,
            "status": STATUS_RESOLVED,
            "track": self._track_payload(track),
            "rekordbox_id": entry.content_id,
            "filepath": entry.filepath,
            "match_confidence": confidence,
            "message": None,
        }

    @staticmethod
    def _entry_summary(entry: rekordbox_library.RekordboxEntry) -> dict[str, Any]:
        return {
            "rekordbox_id": entry.content_id,
            "filepath": entry.filepath,
            "title": entry.title,
            "artist": entry.artist,
        }

    def _tracks_by_filepath(self, filepaths: list[str]) -> dict[str, Track]:
        if not filepaths:
            return {}
        spellings = list(dict.fromkeys(
            variant for path in filepaths for variant in rekordbox_library.path_variants(path)
        ))
        rows = self.session.exec(select(Track).where(Track.filepath.in_(spellings))).all()
        matches = {}
        for path in filepaths:
            candidates = [track for track in rows if rekordbox_library.same_path(path, track.filepath)]
            if len(candidates) == 1:
                matches[path] = candidates[0]
        return matches

    # --------------------------------------------------------- recommendation

    def recommend(
        self,
        source_track_id: int,
        intent: str,
        limit: int = 12,
        exclude_track_ids: Optional[list[int]] = None,
        genres: Optional[list[str]] = None,
        genre_scope: str = "any",
        filters: Optional[dict[str, Any]] = None,
        transition: bool = False,
        reference: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        intent = scoring.resolve_intent(intent)
        # Approved wordplay edges are the whole search space; no tempo rule applies.
        transition = bool(transition) and intent != "wordplay"
        if genre_scope not in {"any", "same_genre", "same_subgenre"}:
            raise ValueError(f"unknown genre scope: {genre_scope}")
        conditions = _conditions(filters)

        source = self.tracks.get_by_id(source_track_id)
        if not source:
            raise ValueError("Track not found")
        target = self._reference_anchor(reference)
        preset = scoring.PRESETS[intent]
        caveats = list(scoring.TRANSITION_CAVEATS) if transition else []

        def response(candidates=(), notes=(), unavailable=0, alternatives=()):
            return {
                "intent": intent,
                "intent_label": preset.label,
                "transition": transition,
                "source_track_id": source_track_id,
                "basis": scoring.describe_weights(intent),
                "reference": self._anchor_summary(target) if target else None,
                "filters": conditions.describe(),
                "candidates": list(candidates),
                "alternatives": list(alternatives),
                "notes": list(notes),
                "caveats": caveats,
                "unavailable_originals": unavailable,
            }

        if reference and (reference.get("kind") or "source") != "source" and target is None:
            return response(notes=[self._missing_reference_note(reference)])
        scope_field = {"same_genre": "genre", "same_subgenre": "subgenre"}.get(genre_scope)
        scope_value = rekordbox_library.normalize_text(getattr(source, scope_field)) if scope_field else None
        if scope_field and not scope_value:
            label = "ジャンル" if scope_field == "genre" else "サブジャンル"
            return response(notes=[f"元曲の{label}が未登録のため、同じ{label}の候補を提案できません"])
        source_payload = self._track_payload(source)
        leading_notes: list[str] = []
        declared = scoring.parse_transition(source.title)
        if declared is not None:
            # After a tempo-changing edit the floor is at its closing tempo, and
            # the analysed BPM may describe either end.
            source_payload["bpm"] = declared[1]
            leading_notes.append(
                f"基準曲はタイトル上 {declared[0]:g}→{declared[1]:g} BPM のトランジション曲のため、"
                f"出口の {declared[1]:g} BPM を基準にしています"
            )
        source_bpm = source_payload.get("bpm")
        if not scoring._positive(source_bpm) and intent != "wordplay":
            return response(notes=["基準曲の BPM が未解析のため、つなぎを判定できません"])
        if intent == "throwback" and not scoring._positive(source.year):
            return response(notes=["基準曲のリリース年が未登録のため、「時代を戻す」は使えません"])

        excluded_ids = self._excluded_with_duplicates(
            source_track_id, [*(exclude_track_ids or []), *(target["exclude_ids"] if target else [])]
        )
        excluded = sorted(excluded_ids)
        # Read only lightweight metadata, so strict normalized scopes, compound
        # filters and history identity apply before the bounded embedding pool.
        eligible = [row for row in self._metadata() if row.id not in excluded_ids
                    and (scope_field is None or
                         rekordbox_library.normalize_text(getattr(row, scope_field)) == scope_value)
                    and conditions.matches(row)]
        pairs = self._approved_pairs(source_track_id)
        notes: list[str] = [*leading_notes, *(target["notes"] if target else [])]

        if intent == "wordplay" and not pairs:
            return response(notes=[
                "この曲を起点とする承認済みワードプレイはまだありません。"
                "ワードプレイ画面で候補を承認すると出てきます"
            ])
        if not conditions.is_empty() and not eligible:
            return response(notes=["指定した条件に合う曲がライブラリにありません"])
        if transition:
            fits = {row.id: scoring.transition_fit(source_bpm, {"title": row.title, "bpm": row.bpm})
                    for row in eligible}
            eligible = sorted((row for row in eligible if fits[row.id] is not None),
                              key=lambda row: (-fits[row.id].score, row.id))
            if not eligible:
                return response(notes=[
                    f"基準 {source_bpm:g} BPM から入れるトランジション曲・倍テン・ハーフテンの曲が見つかりません"
                ])

        try:
            registered = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return response(notes=[f"{error}。登録済みの原本を確認できないため、提案を停止しました"])

        context = self._scoring_context(source, source_payload, pairs, transition, target)
        eligible_ids = [row.id for row in eligible]
        pool = self._candidate_pool(source, intent, excluded, genres, pairs, eligible_ids,
                                    conditions, transition, source_bpm=source_bpm)
        entries, unavailable, filtered = self._score_pool(pool, intent, registered, context)

        ranked = self._rank(entries, self._order(target))[: min(limit, MAX_SUGGESTIONS)]
        if unavailable:
            notes.append(
                f"rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"
            )
        if not entries and filtered and not transition and intent != "wordplay":
            low, high = preset.tempo_window
            notes.append(
                f"「{preset.label}」のテンポ範囲（{low:+g}〜{high:+g}%）に合う曲がありません。"
                "テンポを大きく変えたいときは「トランジション」をオンにしてください"
            )

        # Other presets rank a general pool: throwback's own pool is only older
        # records, which would make every alternative a throwback too.
        general = pool
        if intent == "throwback" and not transition:
            general = self._candidate_pool(source, "keep", excluded, genres, pairs,
                                           eligible_ids=None, conditions=conditions, transition=False,
                                           metadata_ids=self._eligible_ids(scope_field, scope_value,
                                                                           conditions, excluded_ids),
                                           source_bpm=source_bpm)
        return response(
            candidates=[self._entry_payload(entry, pairs) for entry in ranked],
            alternatives=self._alternatives(intent, ranked, general, registered, context, pairs, target),
            notes=notes,
            unavailable=unavailable,
        )

    def route(
        self,
        source_track_id: int,
        target_track_id: int,
        intent: str = "keep",
        max_intermediate: int = 2,
        limit: int = 3,
        exclude_track_ids: Optional[list[int]] = None,
        genre_scope: str = "any",
        filters: Optional[dict[str, Any]] = None,
        transition: bool = False,
    ) -> dict[str, Any]:
        """Find short, playable paths from one exact track to another.

        The target is always the requested track; it is never replaced with a
        merely similar result. Intermediate tracks use the same mixability and
        intent scoring as ordinary Assist recommendations. A small beam search
        keeps the route useful on a large library without pretending to solve a
        complete set-planning problem.
        """
        intent = scoring.resolve_intent(intent)
        transition = bool(transition) and intent != "wordplay"
        if genre_scope not in {"any", "same_genre", "same_subgenre"}:
            raise ValueError(f"unknown genre scope: {genre_scope}")
        if intent == "wordplay":
            return {
                "source": None,
                "target": None,
                "intent": intent,
                "intent_label": scoring.PRESETS[intent].label,
                "transition": False,
                "max_intermediate": max_intermediate,
                "routes": [],
                "notes": ["ワードプレイは承認済みの直接ペアを使う機能のため、ルート提案には対応していません"],
                "caveats": [],
            }
        max_intermediate = max(0, min(int(max_intermediate), MAX_ROUTE_INTERMEDIATE))
        limit = max(1, min(int(limit), 5))
        conditions = _conditions(filters)
        source = self.tracks.get_by_id(source_track_id)
        target = self.tracks.get_by_id(target_track_id)
        if source is None:
            raise ValueError("Source track not found")
        if target is None:
            raise ValueError("Target track not found")
        if source.id == target.id or self._recording_identity(source.title, source.artist) == \
                self._recording_identity(target.title, target.artist):
            return {
                "source": self._track_payload(source),
                "target": self._track_payload(target),
                "intent": intent,
                "intent_label": scoring.PRESETS[intent].label,
                "transition": transition,
                "max_intermediate": max_intermediate,
                "routes": [],
                "notes": ["出発曲と目的曲が同じ録音のため、ルートを作れません"],
                "caveats": list(scoring.TRANSITION_CAVEATS) if transition else [],
            }

        try:
            registered = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return {
                "source": self._track_payload(source),
                "target": self._track_payload(target),
                "intent": intent,
                "intent_label": scoring.PRESETS[intent].label,
                "transition": transition,
                "max_intermediate": max_intermediate,
                "routes": [],
                "notes": [f"{error}。登録済みの原本を確認できないため、ルート提案を停止しました"],
                "caveats": list(scoring.TRANSITION_CAVEATS) if transition else [],
            }

        response_base = {
            "source": self._track_payload(source),
            "target": self._track_payload(target),
            "intent": intent,
            "intent_label": scoring.PRESETS[intent].label,
            "transition": transition,
            "max_intermediate": max_intermediate,
            "caveats": list(scoring.TRANSITION_CAVEATS) if transition else [],
        }
        if not self._is_available(source, registered):
            return {**response_base, "routes": [], "notes": ["出発曲が rekordbox に登録されていないか、ファイルが見つかりません"]}
        if not self._is_available(target, registered):
            return {**response_base, "routes": [], "notes": ["目的曲が rekordbox に登録されていないか、ファイルが見つかりません"]}

        scope_field = {"same_genre": "genre", "same_subgenre": "subgenre"}.get(genre_scope)
        source_scope_value = (
            rekordbox_library.normalize_text(getattr(source, scope_field)) if scope_field else None
        )
        if scope_field and not source_scope_value:
            label = "ジャンル" if scope_field == "genre" else "サブジャンル"
            return {**response_base, "routes": [], "notes": [f"元曲の{label}が未登録のため、ルートを作れません"]}
        if scope_field and rekordbox_library.normalize_text(getattr(target, scope_field)) != source_scope_value:
            label = "ジャンル" if scope_field == "genre" else "サブジャンル"
            return {**response_base, "routes": [], "notes": [f"目的曲が元曲と同じ{label}ではないため、現在の範囲では到達できません"]}

        excluded_ids = self._excluded_with_duplicates(source.id, exclude_track_ids or [])
        # An explicitly selected destination is allowed even if it was in the
        # loaded-history list; it remains the fixed endpoint of the route.
        excluded_ids.discard(target.id)
        conditions_rows = [
            row for row in self._metadata()
            if row.id not in excluded_ids
            and row.id != target.id
            and (scope_field is None or
                 rekordbox_library.normalize_text(getattr(row, scope_field)) == source_scope_value)
            and conditions.matches(row)
        ]
        eligible_ids = [row.id for row in conditions_rows]
        if not conditions.is_empty() and not eligible_ids:
            return {**response_base, "routes": [], "notes": ["中継曲に指定した条件に合う曲がライブラリにありません"]}

        source_payload = self._route_source_payload(source)
        target_info = self.recommendations.fetch_candidates_pool(
            {}, limit=1, candidate_ids=[target.id], exclude_ids=[]
        )
        if not target_info:
            return {**response_base, "routes": [], "notes": ["目的曲の解析情報を読み込めませんでした"]}
        target_info = target_info[0]
        target_anchor = self._track_anchor(target, "track", f"『{target.title}』（{target.artist}）")

        states = [{
            "track": source,
            "payload": source_payload,
            "ids": {source.id},
            "steps": [],
            "scores": [],
            "priority": 1.0,
        }]
        routes: list[dict[str, Any]] = []
        notes: list[str] = []
        if not eligible_ids and max_intermediate:
            notes.append("条件に合う中継曲がありません。出発曲から目的曲への直接接続だけを確認しました")

        for depth in range(max_intermediate + 1):
            next_states: list[dict[str, Any]] = []
            for state in states:
                direct = self._route_edge(
                    state["track"], state["payload"], target_info,
                    intent, transition,
                )
                if direct is not None:
                    routes.append(self._route_payload(
                        source, target, state["steps"], direct, state["scores"],
                    ))
                if depth >= max_intermediate:
                    continue
                blocked = excluded_ids | state["ids"] | {target.id}
                pool = self._candidate_pool(
                    state["track"], intent, sorted(blocked), None, {}, eligible_ids,
                    conditions, transition,
                    source_bpm=state["payload"].get("bpm"),
                )
                context = self._scoring_context(
                    state["track"], state["payload"], {}, transition, None,
                )
                entries, _, _ = self._score_pool(pool, intent, registered, context)
                by_id = {item["id"]: item for item in pool}
                for entry in entries:
                    track_id = int(entry["payload"]["id"])
                    if track_id in blocked:
                        continue
                    candidate_info = by_id.get(track_id)
                    if candidate_info is None:
                        continue
                    like = self._likeness(target_anchor, entry["payload"], candidate_info)
                    edge_score = entry["result"].score
                    scores = [*state["scores"], edge_score]
                    average = sum(scores) / len(scores)
                    destination_likeness = like.score if like else 0.0
                    next_states.append({
                        "track": candidate_info["track"],
                        "payload": self._route_source_payload(candidate_info["track"]),
                        "ids": state["ids"] | {track_id},
                        "steps": [*state["steps"], {
                            "from_track_id": state["payload"]["id"],
                            "to_track_id": track_id,
                            "result": entry["result"],
                            "payload": entry["payload"],
                        }],
                        "scores": scores,
                        "priority": 0.65 * average + 0.35 * destination_likeness,
                    })
            if not next_states:
                break
            next_states.sort(key=lambda state: (-state["priority"], tuple(sorted(state["ids"]))))
            states = next_states[:MAX_ROUTE_BEAM]

        unique: dict[tuple[int, ...], dict[str, Any]] = {}
        for route in routes:
            key = tuple(track["id"] for track in route["tracks"])
            unique.setdefault(key, route)
        ordered = sorted(unique.values(), key=lambda route: (-route["score"], len(route["steps"]),
                                                               tuple(track["id"] for track in route["tracks"])))
        if not ordered:
            notes.append("指定した条件では、目的曲までのつなぎやすいルートが見つかりません")
            if genre_scope == "any":
                notes.append("BPM・キー・系統の条件を緩めるか、中継曲の条件を減らして試してください")
        elif genre_scope == "any" and any(
            scoring.genre_continuity_score(route["tracks"][0], route["tracks"][1]) is not None
            and scoring.genre_continuity_score(route["tracks"][0], route["tracks"][1]) < 0.75
            for route in ordered
            if len(route["tracks"]) > 1
        ):
            notes.append("一部のルートでジャンルが変わります。各ステップの連続性を確認してください")
        return {**response_base, "routes": ordered[:limit], "notes": notes}

    def _route_source_payload(self, track: Track) -> dict[str, Any]:
        payload = self._track_payload(track)
        declared = scoring.parse_transition(track.title)
        if declared is not None:
            payload["bpm"] = declared[1]
        return payload

    def _route_edge(self, source_track, source_payload, candidate, intent, transition):
        payload = self._track_payload(candidate["track"], bool(candidate.get("has_lyrics")))
        fit = scoring.transition_fit(source_payload.get("bpm"), payload) if transition else None
        if transition:
            if fit is None or not scoring.passes_transition_filter(intent, source_payload, payload):
                return None
        elif not scoring.passes_intent_filter(intent, source_payload, payload):
            return None
        embedding = self.session.get(TrackEmbedding, source_track.id)
        result = self._evaluate(
            source_payload, embedding, self._vector(embedding), payload,
            candidate, intent, {}, fit,
        )
        return {
            "from_track_id": source_track.id,
            "to_track_id": payload["id"],
            "result": result,
            "payload": payload,
        }

    def _route_payload(self, source, target, previous_steps, direct, previous_scores):
        steps = [*previous_steps, direct]
        scores = [*previous_scores, direct["result"].score]
        tracks = [self._track_payload(source)]
        tracks.extend(step["payload"] for step in steps[:-1])
        tracks.append(self._track_payload(target))
        quality = sum(scores) / len(scores) - 0.03 * max(0, len(steps) - 1)
        return {
            "score": round(max(0.0, quality), 4),
            "tracks": tracks,
            "steps": [
                {
                    "from_track_id": step["from_track_id"],
                    "to_track_id": step["to_track_id"],
                    "score": round(step["result"].score, 4),
                    "summary": step["result"].summary,
                    "reasons": [reason.to_dict() for reason in step["result"].reasons],
                }
                for step in steps
            ],
        }

    def _eligible_ids(self, scope_field, scope_value, conditions, excluded_ids) -> list[int]:
        return [row.id for row in self._metadata() if row.id not in excluded_ids
                and (scope_field is None or
                     rekordbox_library.normalize_text(getattr(row, scope_field)) == scope_value)
                and conditions.matches(row)]

    def _scoring_context(self, source, source_payload, pairs, transition, target) -> dict[str, Any]:
        embedding = self.session.get(TrackEmbedding, source.id)
        return {
            "source": source_payload,
            "embedding": embedding,
            "vector": self._vector(embedding),
            "pairs": pairs,
            "transition": transition,
            "target": target,
        }

    def _score_pool(self, pool, intent, registered, context):
        """Score an already-fetched pool for one preset.

        Returns (entries, unavailable originals, dropped by the preset's rules).
        """
        source_payload = context["source"]
        entries, unavailable, filtered = [], 0, 0
        for candidate in pool:
            track = candidate["track"]
            if not self._is_available(track, registered):
                unavailable += 1
                continue
            payload = self._track_payload(track, bool(candidate.get("has_lyrics")))
            fit = None
            if context["transition"]:
                fit = scoring.transition_fit(source_payload.get("bpm"), payload)
                if fit is None or not scoring.passes_transition_filter(intent, source_payload, payload):
                    filtered += 1
                    continue
            elif not scoring.passes_intent_filter(intent, source_payload, payload):
                filtered += 1
                continue
            result = self._evaluate(source_payload, context["embedding"], context["vector"],
                                    payload, candidate, intent, context["pairs"], fit)
            target = context["target"]
            like = self._likeness(target, payload, candidate) if target else None
            entries.append({"payload": payload, "result": result, "like": like, "target": target})
        return entries, unavailable, filtered

    @staticmethod
    def _order(target):
        if target is None:
            return lambda entry: entry["result"].score
        return lambda entry: scoring.blend(entry["result"].score, entry["like"])

    def _rank(self, entries, key) -> list[dict[str, Any]]:
        ordered = sorted(entries, key=lambda entry: (-key(entry), int(entry["payload"]["id"] or 0)))
        seen = set()
        distinct = []
        for entry in ordered:
            identity = self._recording_identity(entry["payload"].get("title"), entry["payload"].get("artist"))
            if identity is not None and identity in seen:
                continue
            if identity is not None:
                seen.add(identity)
            distinct.append(entry)
        return distinct

    def _alternatives(self, intent, ranked, pool, registered, context, pairs, target) -> list[dict[str, Any]]:
        """For each other preset, its best pick that the main list missed."""
        taken = {entry["payload"]["id"] for entry in ranked}
        taken_recordings = {
            self._recording_identity(entry["payload"].get("title"), entry["payload"].get("artist"))
            for entry in ranked
        }
        alternatives = []
        for name in scoring.MOOD_PRESETS:
            if name == intent:
                continue
            if name == "throwback" and not scoring._positive(context["source"].get("year")):
                continue
            entries, _, _ = self._score_pool(pool, name, registered, context)
            for entry in self._rank(entries, self._order(target)):
                if entry["result"].score < ALTERNATIVE_MIN_SCORE:
                    break
                payload = entry["payload"]
                identity = self._recording_identity(payload.get("title"), payload.get("artist"))
                if payload["id"] in taken or (identity is not None and identity in taken_recordings):
                    continue
                taken.add(payload["id"])
                taken_recordings.add(identity)
                alternatives.append({
                    "intent": name,
                    "label": f"{scoring.PRESETS[name].label}なら",
                    "track": self._entry_payload(entry, pairs),
                })
                break
        return alternatives

    def _entry_payload(self, entry, pairs) -> dict[str, Any]:
        payload, result = entry["payload"], entry["result"]
        like = self._describe_like(entry.get("like"), entry.get("target"))
        return {
            **payload,
            "score": round(result.score, 4),
            "components": {k: round(v, 4) for k, v in result.components.items()},
            "like": like,
            "summary": f"{like['text']}／{result.summary}" if like else result.summary,
            "strengths": result.strengths,
            "reasons": [reason.to_dict() for reason in result.reasons],
            "wordplay": pairs.get(payload["id"]),
        }

    @staticmethod
    def _describe_like(like, target) -> Optional[dict[str, Any]]:
        if target is None or like is None or like.score < scoring.LIKE_THRESHOLD:
            return None
        return {
            "reference": AssistAppService._anchor_summary(target),
            "score": round(like.score, 4),
            "aspects": [scoring.LIKENESS_LABELS[name] for name in like.aspects],
            "text": scoring.describe_like(target["label"], like),
        }

    def _evaluate(
        self, source_payload, source_embedding, source_vector, payload, candidate,
        intent, pairs, transition=None,
    ) -> scoring.Scored:
        similarity = (
            cosine_similarity(
                source_vector,
                candidate.get("vector"),
                source_embedding.model_name if source_embedding else None,
                candidate.get("embedding_model"),
            )
            if source_vector is not None and candidate.get("vector") is not None
            else None
        )
        return scoring.evaluate(
            source_payload,
            payload,
            intent,
            vector_similarity=similarity,
            pair=pairs.get(payload["id"]),
            has_analysis=candidate.get("vector") is not None,
            transition=transition,
        )

    # ---------------------------------------------------------------- anchors

    def _track_anchor(self, track: Track, kind: str, label: str) -> dict[str, Any]:
        embedding = self.session.get(TrackEmbedding, track.id)
        return {
            "key": f"{kind}:{track.id}",
            "kind": kind,
            "label": label,
            "track_id": track.id,
            "artist": None,
            "members": [(self._track_payload(track, False), self._vector(embedding),
                         embedding.model_name if embedding else None)],
            "exclude_ids": [track.id] if kind == "track" else [],
            "notes": [],
        }

    def _reference_anchor(self, reference: Optional[dict[str, Any]]) -> Optional[dict[str, Any]]:
        """The "like X" target, or None for the default (the deck track itself)."""
        if not reference or (reference.get("kind") or "source") == "source":
            return None
        kind = reference.get("kind")
        if kind == "track":
            track = self.tracks.get_by_id(int(reference.get("track_id") or 0))
            if track is None:
                return None
            return self._track_anchor(track, "track", f"『{track.title}』（{track.artist}）")
        if kind == "artist":
            wanted = rekordbox_library.normalize_text(reference.get("artist"))
            if not wanted:
                return None
            rows = self.session.exec(select(Track).where(Track.artist.is_not(None))).all()
            members = [t for t in rows if rekordbox_library.normalize_text(t.artist) == wanted]
            if not members:
                members = [t for t in rows if wanted in rekordbox_library.normalize_text(t.artist)]
            if not members:
                return None
            members = sorted(members, key=lambda t: t.id)[:MAX_ARTIST_ANCHORS]
            name = members[0].artist if len({rekordbox_library.normalize_text(t.artist) for t in members}) == 1 \
                else reference.get("artist")
            embeddings = {
                row.track_id: row for row in self.session.exec(
                    select(TrackEmbedding).where(TrackEmbedding.track_id.in_([t.id for t in members]))
                ).all()
            }
            return {
                "key": f"artist:{wanted}",
                "kind": "artist",
                "label": name,
                "track_id": None,
                "artist": name,
                "members": [
                    (self._track_payload(t, False), self._vector(embeddings.get(t.id)),
                     embeddings[t.id].model_name if t.id in embeddings else None)
                    for t in members
                ],
                # "Like the artist" means other artists; their own songs are one
                # artist filter away and would otherwise fill the whole list.
                "exclude_ids": [t.id for t in members],
                "notes": [f"{name} 本人の曲は除外しています（アーティスト条件で絞り込めます）"],
            }
        raise AssistInputError(f"unknown reference kind: {kind}")

    @staticmethod
    def _missing_reference_note(reference: dict[str, Any]) -> str:
        if reference.get("kind") == "artist":
            return f"アーティスト「{reference.get('artist')}」がライブラリに見つかりません"
        return "参照する曲がライブラリに見つかりません"

    @staticmethod
    def _anchor_summary(anchor: dict[str, Any]) -> dict[str, Any]:
        return {
            "kind": anchor["kind"],
            "label": anchor["label"],
            "track_id": anchor.get("track_id"),
            "artist": anchor.get("artist"),
        }

    @staticmethod
    def _likeness(anchor, payload, candidate) -> Optional[scoring.Likeness]:
        """Likeness to an anchor; for an artist, the mean of its three closest songs."""
        vector, model = candidate.get("vector"), candidate.get("embedding_model")
        likes = []
        for member, member_vector, member_model in anchor["members"]:
            if member.get("id") == payload.get("id"):
                continue
            similarity = None
            if (member_vector is not None and vector is not None
                    and embedding_space(member_model) == embedding_space(model)):
                similarity = cosine_similarity(member_vector, vector, member_model, model)
            like = scoring.likeness(member, payload, similarity)
            if like is not None:
                likes.append(like)
        if not likes:
            return None
        likes.sort(key=lambda like: -like.score)
        top = likes[:3]
        return scoring.Likeness(score=sum(like.score for like in top) / len(top), aspects=top[0].aspects)

    # ------------------------------------------------------------------ search

    def search(
        self,
        filters: Optional[dict[str, Any]],
        limit: int = 12,
        exclude_track_ids: Optional[list[int]] = None,
        reference: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Tracks matching the compound conditions, when there is no source deck.

        Same availability rule as recommendations: only originals rekordbox knows
        about and that exist on disk. Without a source there is no transition to
        score: with a "like X" reference the order is likeness, otherwise plain
        (tempo when a BPM range is given, else artist and title).
        """
        conditions = _conditions(filters)
        target = self._reference_anchor(reference)

        def response(candidates=(), notes=(), unavailable=0, total=0):
            return {
                "filters": conditions.describe(),
                "reference": self._anchor_summary(target) if target else None,
                "candidates": list(candidates),
                "notes": list(notes),
                "unavailable_originals": unavailable,
                "total_matches": total,
            }

        if reference and (reference.get("kind") or "source") != "source" and target is None:
            return response(notes=[self._missing_reference_note(reference)])
        if conditions.is_empty() and target is None:
            return response(notes=["検索条件か「〜のような曲」を1つ以上指定してください"])
        excluded_ids = self._excluded_with_duplicates(
            None, [*(exclude_track_ids or []), *(target["exclude_ids"] if target else [])]
        )
        rows = [row for row in self._metadata(with_path=True)
                if row.id not in excluded_ids and conditions.matches(row)]
        if not rows:
            return response(notes=["指定した条件に合う曲がライブラリにありません"])
        try:
            registered = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return response(notes=[f"{error}。登録済みの原本を確認できないため、検索を停止しました"])
        notes = list(target["notes"]) if target else []

        if target is not None:
            candidate_ids = [row.id for row in rows]
            targets: dict[str, Any] = {}
            if len(candidate_ids) > MAX_CANDIDATE_POOL:
                first = target["members"][0][0]
                targets = {"bpm": first.get("bpm"), "energy": first.get("energy")}
            pool = self.recommendations.fetch_candidates_pool(
                targets, limit=MAX_CANDIDATE_POOL, exclude_ids=sorted(excluded_ids),
                candidate_ids=candidate_ids,
            )
            entries, unavailable = [], 0
            for candidate in pool:
                track = candidate["track"]
                if not self._is_available(track, registered):
                    unavailable += 1
                    continue
                payload = self._track_payload(track, bool(candidate.get("has_lyrics")))
                entries.append({"payload": payload, "like": self._likeness(target, payload, candidate)})
            ranking = self._rank(
                entries, lambda entry: entry["like"].score if entry["like"] else -1.0,
            )[: min(limit, MAX_SUGGESTIONS)]
            if unavailable:
                notes.append(
                    f"rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"
                )
            candidates = []
            for entry in ranking:
                like = self._describe_like(entry["like"], target)
                item = self._search_payload_from(entry["payload"])
                if like:
                    item["like"] = like
                    item["summary"] = like["text"]
                candidates.append(item)
            return response(candidates=candidates, notes=notes, unavailable=unavailable, total=len(rows))

        by_tempo = conditions.bpm_min is not None or conditions.bpm_max is not None
        rows.sort(key=lambda row: (
            (row.bpm or 0.0) if by_tempo else 0.0,
            rekordbox_library.normalize_text(row.artist),
            rekordbox_library.normalize_text(row.title),
            row.id,
        ))
        chosen, seen, unavailable = [], set(), 0
        for row in rows:
            if not self._is_available(row, registered):
                unavailable += 1
                continue
            identity = self._recording_identity(row.title, row.artist)
            if identity is not None and identity in seen:
                continue
            seen.add(identity)
            chosen.append(row.id)
            if len(chosen) >= min(limit, MAX_SUGGESTIONS):
                break
        tracks = self.recommendations.get_tracks_by_ids(chosen)
        if unavailable:
            notes.append(
                f"rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"
            )
        return response(
            candidates=[self._search_payload(tracks[track_id]) for track_id in chosen if track_id in tracks],
            notes=notes,
            unavailable=unavailable,
            total=len(rows),
        )

    def _search_payload(self, track: Track) -> dict[str, Any]:
        return self._search_payload_from(self._track_payload(track))

    @staticmethod
    def _search_payload_from(payload: dict[str, Any]) -> dict[str, Any]:
        facts = [
            f"{payload['bpm']:g} BPM" if payload.get("bpm") else "BPM 不明",
            payload.get("key") or "キー不明",
            payload.get("genre") or "ジャンル未登録",
            str(payload["year"]) if payload.get("year") else "年不明",
        ]
        return {
            **payload,
            "score": None,
            "components": {},
            "like": None,
            "summary": "条件に一致（つなぎの相性は基準デッキがあると判定できます）",
            "strengths": [],
            "reasons": [{"kind": "match", "tone": "neutral", "text": " · ".join(facts)}],
            "wordplay": None,
        }

    # ------------------------------------------------------- agent-picked tracks

    def describe_tracks(
        self,
        track_ids: list[int],
        source_track_id: Optional[int] = None,
        intent: str = "keep",
    ) -> dict[str, Any]:
        """Check and explain tracks someone else picked (e.g. a chat agent).

        Each track is either offered, with the same objective reasons the assist
        ranking would show against the source deck, or refused with the reason.
        """
        try:
            intent = scoring.resolve_intent(intent)
        except ValueError:
            intent = "keep"
        wanted = list(dict.fromkeys(int(track_id) for track_id in track_ids))
        try:
            registered = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return {
                "tracks": [],
                "rejected": [{"track_id": track_id, "reason": str(error)} for track_id in wanted],
            }
        source = self.tracks.get_by_id(source_track_id) if source_track_id else None
        source_payload = self._track_payload(source) if source else None
        declared = scoring.parse_transition(source.title) if source else None
        if declared is not None:
            source_payload["bpm"] = declared[1]
        source_embedding = self.session.get(TrackEmbedding, source.id) if source else None
        source_vector = self._vector(source_embedding)
        pairs = self._approved_pairs(source.id) if source else {}
        pool = {
            item["id"]: item
            for item in self.recommendations.fetch_candidates_pool({}, limit=len(wanted), candidate_ids=wanted)
        } if wanted else {}

        offered, rejected = [], []
        for track_id in wanted:
            candidate = pool.get(track_id)
            if candidate is None:
                rejected.append({"track_id": track_id, "reason": "ライブラリに存在しない曲 ID です"})
                continue
            if source and track_id == source.id:
                rejected.append({"track_id": track_id, "reason": "基準デッキの曲そのものです"})
                continue
            track = candidate["track"]
            if not self._is_available(track, registered):
                rejected.append({
                    "track_id": track_id,
                    "reason": "rekordbox 未登録、またはファイルが見つからないためデッキに載せられません",
                })
                continue
            payload = self._track_payload(track, bool(candidate.get("has_lyrics")))
            if source_payload is None:
                offered.append(self._search_payload_from(payload))
                continue
            result = self._evaluate(
                source_payload, source_embedding, source_vector, payload, candidate, intent, pairs,
            )
            offered.append(self._entry_payload({"payload": payload, "result": result}, pairs))
        return {"tracks": offered, "rejected": rejected}

    def _candidate_pool(
        self,
        source: Track,
        intent: str,
        excluded: list[int],
        genres: Optional[list[str]],
        pairs: dict[int, dict[str, Any]],
        eligible_ids: Optional[list[int]],
        conditions: Optional[AssistFilters] = None,
        transition: bool = False,
        metadata_ids: Optional[list[int]] = None,
        source_bpm: Optional[float] = None,
    ) -> list[dict[str, Any]]:
        """Pull candidates with preset-appropriate pre-filtering.

        `eligible_ids` already carries scope, filters and, in transition mode,
        the transition rule (ordered best first). Throwback narrows to older
        records here, because a tempo-ordered pool of a large library would
        otherwise be mostly recent releases.
        """
        if eligible_ids is None:
            eligible_ids = metadata_ids or []
        if intent == "wordplay":
            # Approved targets are the whole search space; tempo/energy filters
            # must not silently drop an edge the DJ deliberately approved.
            return self.recommendations.fetch_candidates_pool(
                {},
                limit=len(pairs),
                exclude_ids=excluded,
                candidate_ids=sorted(set(pairs) & set(eligible_ids)),
            )
        if transition:
            return self.recommendations.fetch_candidates_pool(
                {}, limit=MAX_CANDIDATE_POOL, exclude_ids=excluded,
                candidate_ids=eligible_ids[:MAX_CANDIDATE_POOL],
            )
        if intent == "throwback" and source.year:
            older = set(
                self.session.exec(
                    select(Track.id).where(Track.year > 0)
                    .where(Track.year <= source.year - scoring.THROWBACK_MIN_YEARS)
                ).all()
            )
            eligible_ids = [track_id for track_id in eligible_ids if track_id in older]

        preset = scoring.PRESETS[intent]
        targets: dict[str, Any] = {}
        tempo = source_bpm if source_bpm is not None else source.bpm
        if tempo:
            targets["bpm"] = tempo * (1 + preset.tempo_center / 100)
        for feature, direction in preset.directions.items():
            value = getattr(source, feature, None)
            if value is None:
                continue
            step = scoring.FEATURE_STEPS[feature]
            offset = step if direction == "up" else -step if direction == "down" else 0.0
            if feature == "energy" and direction in ("up", "down"):
                targets["energy"] = max(0.0, min(1.0, value + offset))
            elif feature in ("danceability", "brightness", "noisiness"):
                targets[feature] = value + offset
        if conditions is not None and not conditions.is_empty():
            if len(eligible_ids) <= MAX_CANDIDATE_POOL:
                # The DJ already narrowed the library; score all of it rather
                # than letting the tempo/energy pre-filter second-guess them.
                targets = {}
            elif conditions.bpm_min is not None or conditions.bpm_max is not None:
                targets.pop("bpm", None)

        pool = self.recommendations.fetch_candidates_pool(
            targets,
            genres=genres,
            limit=MAX_CANDIDATE_POOL,
            exclude_ids=excluded,
            candidate_ids=eligible_ids,
        )
        # Approved wordplay targets are eligible under every preset, so make sure
        # the tempo-ordered pool cap never hides one.
        missing = sorted((set(pairs) & set(eligible_ids)) - {item["id"] for item in pool} - set(excluded))
        if missing:
            pool += self.recommendations.fetch_candidates_pool(
                {}, limit=len(missing), exclude_ids=excluded, candidate_ids=missing
            )
        return pool

    def _approved_pairs(self, source_track_id: int) -> dict[int, dict[str, Any]]:
        """Approved, directed wordplay edges out of the source, keyed by target."""
        rows = self.session.exec(
            select(WordplayPair)
            .where(WordplayPair.status == "approved")
            .where(WordplayPair.from_track_id == source_track_id)
        ).all()
        # A tested edge outranks an untested one for the same pair of tracks.
        rows = sorted(rows, key=lambda pair: (pair.verification_status != "tested", pair.id or 0))
        edges: dict[int, dict[str, Any]] = {}
        for pair in rows:
            edges.setdefault(
                pair.to_track_id,
                {
                    "pair_id": pair.id,
                    "keyword": pair.keyword,
                    "source_phrase": pair.source_phrase,
                    "target_phrase": pair.target_phrase,
                    "source_cue_mode": pair.source_cue_mode,
                    "from_timestamp": pair.from_timestamp,
                    "source_cue_end_timestamp": pair.source_cue_end_timestamp,
                    "target_intro_timestamp": pair.target_intro_timestamp,
                    "target_landing_timestamp": (
                        pair.target_landing_timestamp
                        if pair.target_landing_timestamp is not None
                        else pair.to_timestamp
                    ),
                    "transition_notes": pair.transition_notes,
                    "evidence_type": pair.evidence_type,
                    "verification_status": pair.verification_status,
                },
            )
        return edges

    # --------------------------------------------------------------- helpers

    def _metadata(self, with_path: bool = False):
        columns = [Track.id, Track.title, Track.artist, Track.album, Track.genre,
                   Track.subgenre, Track.bpm, Track.key, Track.year]
        if with_path:
            columns.append(Track.filepath)
        return self.session.exec(select(*columns)).all()

    def _excluded_with_duplicates(
        self, source_track_id: Optional[int], exclude_track_ids: Optional[list[int]]
    ) -> set[int]:
        """Excluded ids plus every other copy of the same recording."""
        excluded_ids = {*(exclude_track_ids or [])}
        if source_track_id is not None:
            excluded_ids.add(source_track_id)
        if not excluded_ids:
            return excluded_ids
        rows = self.session.exec(select(Track.id, Track.title, Track.artist)).all()
        identities = {self._recording_identity(row.title, row.artist) for row in rows
                      if row.id in excluded_ids}
        identities.discard(None)
        excluded_ids.update(row.id for row in rows
                            if self._recording_identity(row.title, row.artist) in identities)
        return excluded_ids

    @staticmethod
    def _is_available(track, registered: frozenset[str]) -> bool:
        filepath = track.filepath or ""
        return bool(filepath) and rekordbox_library.is_registered_path(filepath, registered) \
            and os.path.exists(filepath)

    @staticmethod
    def _recording_identity(title, artist):
        title = rekordbox_library.normalize_text(title)
        artist = rekordbox_library.normalize_text(artist)
        return (title, artist) if title and artist else None

    @staticmethod
    def _vector(embedding: Optional[TrackEmbedding]):
        if not embedding or not embedding.embedding_json:
            return None
        try:
            vector = np.array(json.loads(embedding.embedding_json), dtype=float)
        except (TypeError, ValueError):
            return None
        return vector if vector.ndim == 1 and vector.size else None

    def _track_payload(self, track: Track, has_lyrics: Optional[bool] = None) -> dict[str, Any]:
        """Dict form of a track. `has_lyrics` is passed in wherever the caller
        already knows it, so ranking a pool never turns into one query per row."""
        payload = track.model_dump()
        payload["has_lyrics"] = (
            has_lyrics if has_lyrics is not None else bool(self.session.get(Lyrics, track.id))
        )
        return payload

    def library_status(self) -> dict[str, Any]:
        """Whether the rekordbox collection can be read right now."""
        try:
            paths = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return {"available": False, "message": str(error), "registered_tracks": 0}
        return {"available": True, "message": None, "registered_tracks": len(paths)}
