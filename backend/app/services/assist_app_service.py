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
cannot drag onto a deck is not a suggestion.
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
from infra import rekordbox_library
from infra.rekordbox_library import RekordboxLibraryUnavailable
from infra.repositories.recommendation_repository import RecommendationRepository
from infra.repositories.track_repository import TrackRepository
from utils.embedding import cosine_similarity

# Deck resolution states, surfaced verbatim so the UI never has to guess either.
STATUS_EMPTY = "empty"
STATUS_RESOLVED = "resolved"
STATUS_AMBIGUOUS = "ambiguous"
STATUS_UNRESOLVED = "unresolved"
STATUS_NOT_IN_LIBRARY = "not_in_library"

MAX_CANDIDATE_POOL = 400
MAX_SUGGESTIONS = 50


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
        energy_direction: str = "hold",
        limit: int = 12,
        exclude_track_ids: Optional[list[int]] = None,
        genres: Optional[list[str]] = None,
        genre_scope: str = "any",
    ) -> dict[str, Any]:
        if intent not in scoring.INTENTS:
            raise ValueError(f"unknown intent: {intent}")
        if energy_direction not in scoring.ENERGY_DIRECTIONS:
            raise ValueError(f"unknown energy direction: {energy_direction}")

        if genre_scope not in {"any", "same_genre", "same_subgenre"}:
            raise ValueError(f"unknown genre scope: {genre_scope}")

        source = self.tracks.get_by_id(source_track_id)
        if not source:
            raise ValueError("Track not found")

        scope_field = {"same_genre": "genre", "same_subgenre": "subgenre"}.get(genre_scope)
        scope_value = rekordbox_library.normalize_text(getattr(source, scope_field)) if scope_field else None
        if scope_field and not scope_value:
            label = "ジャンル" if scope_field == "genre" else "サブジャンル"
            return {
                "intent": intent, "energy_direction": energy_direction,
                "source_track_id": source_track_id, "candidates": [],
                "notes": [f"元曲の{label}が未登録のため、同じ{label}の候補を提案できません"],
                "unavailable_originals": 0,
            }
        excluded = sorted({source_track_id, *(exclude_track_ids or [])})
        # Read only lightweight metadata, so strict normalized scopes and history
        # identity apply before the repository's bounded embedding pool.
        metadata = self.session.exec(select(Track.id, Track.title, Track.artist,
                                             Track.genre, Track.subgenre)).all()
        excluded_ids = set(excluded)
        identities = {self._recording_identity(row.title, row.artist) for row in metadata
                      if row.id in excluded_ids}
        identities.discard(None)
        excluded_ids.update(row.id for row in metadata
                            if self._recording_identity(row.title, row.artist) in identities)
        excluded = sorted(excluded_ids)
        eligible_ids = [row.id for row in metadata if row.id not in excluded_ids
                        and (scope_field is None or
                             rekordbox_library.normalize_text(getattr(row, scope_field)) == scope_value)]
        pairs = self._approved_pairs(source_track_id)
        notes: list[str] = []

        if intent == "wordplay" and not pairs:
            return {
                "intent": intent,
                "energy_direction": energy_direction,
                "source_track_id": source_track_id,
                "candidates": [],
                "notes": [
                    "この曲を起点とする承認済みワードプレイはまだありません。"
                    "ワードプレイ画面で候補を承認すると出てきます"
                ],
                "unavailable_originals": 0,
            }

        pool = self._candidate_pool(source, intent, energy_direction, excluded, genres, pairs, eligible_ids)

        try:
            registered = rekordbox_library.registered_paths()
        except RekordboxLibraryUnavailable as error:
            return {
                "intent": intent,
                "energy_direction": energy_direction,
                "source_track_id": source_track_id,
                "candidates": [],
                "notes": [f"{error}。登録済みの原本を確認できないため、提案を停止しました"],
                "unavailable_originals": 0,
            }

        source_payload = self._track_payload(source)
        source_embedding = self.session.get(TrackEmbedding, source_track_id)
        source_vector = self._vector(source_embedding)

        scored: list[tuple[dict[str, Any], scoring.Scored]] = []
        unavailable = 0
        for candidate in pool:
            track = candidate["track"]
            filepath = track.filepath or ""
            if not rekordbox_library.is_registered_path(filepath, registered):
                unavailable += 1
                continue
            if not filepath or not os.path.exists(track.filepath):
                unavailable += 1
                continue

            payload = self._track_payload(track, bool(candidate.get("has_lyrics")))
            if not scoring.passes_intent_filter(intent, source_payload, payload):
                continue

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
            result = scoring.evaluate(
                source_payload,
                payload,
                intent,
                energy_direction,
                vector_similarity=similarity,
                pair=pairs.get(track.id),
                has_analysis=candidate.get("vector") is not None,
            )
            scored.append((payload, result))

        ranked = []
        seen_recordings = set()
        for payload, result in scoring.rank(scored, len(scored)):
            identity = self._recording_identity(payload.get("title"), payload.get("artist"))
            if identity is not None and identity in seen_recordings:
                continue
            if identity is not None:
                seen_recordings.add(identity)
            ranked.append((payload, result))
            if len(ranked) >= min(limit, MAX_SUGGESTIONS):
                break
        if unavailable:
            notes.append(
                f"rekordbox 未登録、またはファイルが見つからない {unavailable} 曲を除外しました"
            )
        return {
            "intent": intent,
            "energy_direction": energy_direction,
            "source_track_id": source_track_id,
            "candidates": [
                {
                    **payload,
                    "score": round(result.score, 4),
                    "components": {k: round(v, 4) for k, v in result.components.items()},
                    "reasons": [reason.to_dict() for reason in result.reasons],
                    "wordplay": pairs.get(payload["id"]),
                }
                for payload, result in ranked
            ],
            "notes": notes,
            "unavailable_originals": unavailable,
        }

    def _candidate_pool(
        self,
        source: Track,
        intent: str,
        energy_direction: str,
        excluded: list[int],
        genres: Optional[list[str]],
        pairs: dict[int, dict[str, Any]],
        eligible_ids: list[int],
    ) -> list[dict[str, Any]]:
        """Pull candidates with intent-appropriate pre-filtering."""
        if intent == "wordplay":
            # Approved targets are the whole search space; tempo/energy filters
            # must not silently drop an edge the DJ deliberately approved.
            return self.recommendations.fetch_candidates_pool(
                {},
                limit=len(pairs),
                exclude_ids=excluded,
                candidate_ids=sorted(set(pairs) & set(eligible_ids)),
            )

        targets: dict[str, Any] = {"bpm": source.bpm}
        source_energy = source.energy if source.energy is not None else None
        if source_energy is not None and energy_direction != "hold":
            step = scoring.ENERGY_STEP if energy_direction == "up" else -scoring.ENERGY_STEP
            targets["energy"] = max(0.0, min(1.0, source_energy + step))
        elif source_energy is not None and intent == "groove":
            targets["energy"] = source_energy
        if intent == "groove":
            targets["danceability"] = source.danceability

        pool = self.recommendations.fetch_candidates_pool(
            targets,
            genres=genres,
            limit=MAX_CANDIDATE_POOL,
            exclude_ids=excluded,
            candidate_ids=eligible_ids,
        )
        # Approved wordplay targets are eligible under every intent, so make sure
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
