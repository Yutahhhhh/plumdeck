"""Plan a whole set from rekordbox playlists and/or the DJ's stated tastes.

Two MCP-driven workflows share this service:

* Playlist-driven: the DJ's own rekordbox folders (``現場物/イベ2`` then
  ``現場物/HIPHOP&Latin``) become the phases of the night.
* Preference-driven: the MCP client interviews the DJ, then passes favourite
  artists/tracks per phase; sound-alikes and approved wordplay widen the pool.

The MCP client owns the musical story (phase order, counts, energy ramps).
plumdeck only resolves pools and orders them deterministically.
"""
from __future__ import annotations

import json
import os
from typing import Any, Optional

from sqlmodel import Session, col, select

from app.services.assist_app_service import AssistAppService
from app.services.setlist_app_service import SetlistAppService
from domain.models.track import Track
from domain.services.phased_setlist_builder import Phase, PhasedSetlistBuilder
from domain.services.setlist_builder import APPROVED_WORDPLAY_BONUS
from domain.services.target_parameters import sanitize_target_parameters
from infra import rekordbox_playlists
from infra.repositories.recommendation_repository import RecommendationRepository
from infra.repositories.track_repository import TrackRepository

MAX_PHASES = 12
MAX_PHASE_COUNT = 60
MAX_SIMILAR_SEEDS = 12
LIBRARY_POOL_LIMIT = 300
ARTIST_MATCH_LIMIT = 200


def _clean_list(values) -> list:
    return [value for value in (values or []) if value not in (None, "")]


def _bounds(start, end) -> Optional[tuple[float, float]]:
    if start is None and end is None:
        return None
    start = end if start is None else start
    end = start if end is None else end
    return float(start), float(end)


class SetPlannerService:
    def __init__(self, session: Session):
        self.session = session
        self.recommendations = RecommendationRepository(session)
        self.tracks = TrackRepository(session)
        self.setlists = SetlistAppService(session)

    # ------------------------------------------------------------ playlists

    def list_playlists(self, under: Optional[str] = None, depth: int = 2) -> dict[str, Any]:
        roots = rekordbox_playlists.load_tree()
        if under:
            node = rekordbox_playlists.find_node(roots, under)
            roots, base = node.children, node.path
        else:
            base = None

        def payload(node, level):
            item = {"id": node.id, "name": node.name, "path": node.path, "kind": node.kind}
            if node.kind == "folder":
                item["child_count"] = len(node.children)
                if level < depth:
                    item["children"] = [payload(child, level + 1) for child in node.children]
            else:
                item["track_count"] = node.track_count
            return item

        return {"under": base, "playlists": [payload(node, 1) for node in roots]}

    def resolve_playlist(self, ref: str, recursive: bool = True):
        """(node, [(entry, Track|None)], notes) for one playlist or folder."""
        roots = rekordbox_playlists.load_tree()
        node = rekordbox_playlists.find_node(roots, ref)
        entries, notes = rekordbox_playlists.playlist_entries(node, recursive=recursive)
        by_path = AssistAppService(self.session)._tracks_by_filepath(
            [entry.filepath for entry in entries if entry.filepath]
        )
        return node, [(entry, by_path.get(entry.filepath)) for entry in entries], notes

    def playlist_tracks(self, ref: str, recursive: bool = True) -> dict[str, Any]:
        node, rows, notes = self.resolve_playlist(ref, recursive)
        tracks, unmatched = [], []
        for entry, track in rows:
            if track is None:
                unmatched.append({"playlist": entry.playlist_path, "title": entry.title,
                                  "artist": entry.artist, "filepath": entry.filepath})
                continue
            tracks.append({
                "id": track.id, "title": track.title, "artist": track.artist,
                "bpm": track.bpm, "key": track.key, "genre": track.genre,
                "subgenre": track.subgenre, "year": track.year, "energy": track.energy,
                "danceability": track.danceability, "duration": track.duration,
                "playlist": entry.playlist_path, "track_no": entry.track_no,
            })
        if unmatched:
            notes.append(f"{len(unmatched)} 曲は plumdeck のライブラリに未登録のため候補になりません")
        return {
            "playlist": {"id": node.id, "path": node.path, "kind": node.kind},
            "count": len(tracks), "tracks": tracks,
            "unmatched_count": len(unmatched), "unmatched": unmatched[:30], "notes": notes,
        }

    # ----------------------------------------------------------------- plan

    def plan(
        self,
        phases: list[dict[str, Any]],
        exclude_track_ids: Optional[list[int]] = None,
        exclude_artists: Optional[list[str]] = None,
        opening_track_id: Optional[int] = None,
        include_wordplay_targets: bool = True,
        wordplay_bonus: float = APPROVED_WORDPLAY_BONUS,
        save_as_setlist_name: Optional[str] = None,
    ) -> dict[str, Any]:
        if not 0 <= wordplay_bonus <= 1:
            raise ValueError("wordplay_bonus は 0〜1 で指定してください")
        if not phases:
            raise ValueError("phases を1つ以上指定してください")
        if len(phases) > MAX_PHASES:
            raise ValueError(f"phases は最大 {MAX_PHASES} 個までです")

        excluded_ids = set(_clean_list(exclude_track_ids))
        excluded_artists = [" ".join(str(name).casefold().split()) for name in _clean_list(exclude_artists)]
        wordplay = self.setlists._approved_wordplay()
        notes: list[str] = []
        built: list[Phase] = []
        summaries: list[dict[str, Any]] = []

        for index, spec in enumerate(phases):
            phase, summary = self._phase(index, spec, excluded_ids, excluded_artists,
                                         wordplay if include_wordplay_targets else {}, notes)
            built.append(phase)
            summaries.append(summary)

        opening = None
        if opening_track_id is not None:
            found = self.recommendations.fetch_candidates_pool({}, limit=1, candidate_ids=[opening_track_id])
            if not found:
                raise ValueError(f"opening_track_id {opening_track_id} が見つかりません")
            opening = found[0]

        placements, build_notes = PhasedSetlistBuilder(wordplay_bonus).build(
            built, approved_wordplay_edges=wordplay.keys(), opening=opening,
        )
        notes += build_notes

        tracks = []
        for position, placement in enumerate(placements):
            node = placement["node"]
            track = node["track"]
            previous_id = placement["transition"]["from_track_id"] if placement["transition"] else None
            pair = wordplay.get((previous_id, node["id"])) if previous_id is not None else None
            tracks.append({
                "position": position,
                "phase": placement["phase"],
                "phase_switch": placement["phase_switch"],
                "id": track.id, "title": track.title, "artist": track.artist,
                "bpm": track.bpm, "key": track.key, "genre": track.genre,
                "subgenre": track.subgenre, "year": track.year,
                "energy": track.energy, "duration": track.duration,
                "has_lyrics": bool(node.get("has_lyrics")),
                "file_exists": bool(track.filepath) and os.path.exists(track.filepath),
                "sources": node.get("sources", []),
                "transition": placement["transition"],
                "wordplay_json": json.dumps(self.setlists._wordplay_payload(pair), ensure_ascii=False)
                if pair else None,
            })
        for summary in summaries:
            summary["placed"] = sum(1 for item in tracks if item["phase"] == summary["label"])
        missing_files = sum(1 for item in tracks if not item["file_exists"])
        if missing_files:
            notes.append(f"{missing_files} 曲は音源ファイルが見つかりません")

        result: dict[str, Any] = {
            "count": len(tracks),
            "total_duration_sec": round(sum(float(item["duration"] or 0) for item in tracks)),
            "phases": summaries,
            "tracks": tracks,
            "notes": notes,
            "track_data": [{"id": item["id"], "wordplay_json": item["wordplay_json"]} for item in tracks],
        }
        if save_as_setlist_name:
            setlist = self.setlists.create_setlist(save_as_setlist_name)
            self.setlists.update_setlist_tracks(
                setlist.id, [{"id": item["id"], "wordplay_json": item["wordplay_json"]} for item in tracks]
            )
            result["saved_setlist"] = {"id": setlist.id, "name": setlist.name}
        return result

    def _phase(self, index, spec, excluded_ids, excluded_artists, wordplay, notes):
        label = str(spec.get("label") or f"Phase {index + 1}")
        count = spec.get("count")
        if type(count) is not int or not 1 <= count <= MAX_PHASE_COUNT:
            raise ValueError(f"「{label}」の count は 1〜{MAX_PHASE_COUNT} の整数で指定してください")

        targets = sanitize_target_parameters({"year_min": spec.get("year_min"), "year_max": spec.get("year_max")})
        genres = _clean_list(spec.get("genres")) or None
        subgenres = _clean_list(spec.get("subgenres")) or None
        # Sources the DJ named are taken as-is; only expansions obey the filters.
        pool: dict[int, dict[str, Any]] = {}
        order: list[int] = []

        def add(nodes, source, explicit=False):
            for node in nodes:
                # Tracks the DJ named by id override the exclusion lists.
                if not explicit and (node["id"] in excluded_ids
                                     or self._excluded_artist(node, excluded_artists)):
                    continue
                existing = pool.get(node["id"])
                if existing is None:
                    node["sources"] = []
                    pool[node["id"]] = existing = node
                    order.append(node["id"])
                if source not in existing["sources"]:
                    existing["sources"].append(source)

        def fetch(ids, filtered, explicit=False):
            ids = [tid for tid in dict.fromkeys(ids) if explicit or tid not in excluded_ids]
            if not ids:
                return []
            nodes = self.recommendations.fetch_candidates_pool(
                targets if filtered else {}, genres=genres if filtered else None,
                subgenres=subgenres if filtered else None, limit=len(ids), candidate_ids=ids,
            )
            rank = {tid: position for position, tid in enumerate(ids)}
            return sorted(nodes, key=lambda node: rank[node["id"]])

        anchors = [int(tid) for tid in _clean_list(spec.get("anchor_track_ids"))]
        add(fetch(anchors, False, True), {"type": "anchor"}, True)
        explicit_ids = [int(tid) for tid in _clean_list(spec.get("track_ids"))]
        add(fetch(explicit_ids, False, True), {"type": "track"}, True)

        playlist_summaries = []
        for ref in _clean_list(spec.get("rekordbox_playlists")):
            node, rows, playlist_notes = self.resolve_playlist(str(ref), recursive=True)
            notes.extend(playlist_notes)
            matched = [track.id for _, track in rows if track is not None]
            path_of = {track.id: entry.playlist_path for entry, track in rows if track is not None}
            for candidate in fetch(matched, False):
                add([candidate], {"type": "playlist", "detail": path_of.get(candidate["id"], node.path)})
            unmatched = sum(1 for _, track in rows if track is None)
            playlist_summaries.append({"path": node.path, "matched": len(set(matched)), "unmatched": unmatched})
            if unmatched:
                notes.append(f"「{node.path}」の {unmatched} 曲は plumdeck 未登録のため除外しました")

        for artist in _clean_list(spec.get("artists")):
            ids = self.session.exec(
                select(Track.id).where(col(Track.artist).ilike(f"%{artist}%"))
                .order_by(Track.id).limit(ARTIST_MATCH_LIMIT)
            ).all()
            if len(ids) >= ARTIST_MATCH_LIMIT:
                notes.append(f"「{label}」: アーティスト「{artist}」は {ARTIST_MATCH_LIMIT} 曲を超えるため先頭 {ARTIST_MATCH_LIMIT} 曲だけを候補にしました")
            found = fetch(list(ids), True)
            if not found:
                notes.append(f"「{label}」: アーティスト「{artist}」の曲が条件内に見つかりません")
            add(found, {"type": "artist", "detail": artist})

        similar_seeds = [int(tid) for tid in _clean_list(spec.get("similar_to_track_ids"))]
        if spec.get("expand_similar"):
            similar_seeds += [tid for tid in order if tid not in similar_seeds]
        similar_per_seed = max(1, min(30, int(spec.get("similar_per_seed") or 8)))
        for seed in list(dict.fromkeys(similar_seeds))[:MAX_SIMILAR_SEEDS]:
            try:
                similar = self.tracks.get_similar_tracks(seed, similar_per_seed * 2)
            except ValueError:
                continue
            found = fetch([item["id"] for item in similar], True)[:similar_per_seed]
            add(found, {"type": "similar", "detail": seed})

        if not pool:
            library_targets = dict(targets)
            energy, bpm = _bounds(spec.get("energy_start"), spec.get("energy_end")), \
                _bounds(spec.get("bpm_start"), spec.get("bpm_end"))
            if energy:
                library_targets["energy"] = sum(energy) / 2
            if bpm:
                library_targets["bpm"] = sum(bpm) / 2
            candidates = self.recommendations.fetch_candidates_pool(
                sanitize_target_parameters(library_targets), genres=genres, subgenres=subgenres,
                limit=LIBRARY_POOL_LIMIT, exclude_ids=sorted(excluded_ids) or None,
            )
            add(candidates, {"type": "filter"})

        if wordplay:
            sources = set(pool)
            targets_needed = {}
            for (source, target) in wordplay:
                if source in sources and target not in pool:
                    targets_needed.setdefault(target, set()).add(source)
            for candidate in fetch(sorted(targets_needed), True):
                candidate["wordplay_only_from_ids"] = targets_needed[candidate["id"]]
                add([candidate], {"type": "wordplay", "detail": sorted(targets_needed[candidate["id"]])})

        if not pool:
            notes.append(f"「{label}」の候補がありません")
        phase = Phase(
            label=label, count=count, pool=[pool[tid] for tid in order],
            anchor_ids=[tid for tid in anchors if tid in pool],
            energy=_bounds(spec.get("energy_start"), spec.get("energy_end")),
            bpm=_bounds(spec.get("bpm_start"), spec.get("bpm_end")),
            keep_order=bool(spec.get("keep_order")),
        )
        source_counts: dict[str, int] = {}
        for node in phase.pool:
            for kind in {source["type"] for source in node["sources"]}:
                source_counts[kind] = source_counts.get(kind, 0) + 1
        return phase, {"label": label, "requested": count, "pool_size": len(phase.pool),
                       "pool_sources": source_counts, "playlists": playlist_summaries}

    @staticmethod
    def _excluded_artist(node, excluded_artists) -> bool:
        artist = " ".join((node["track"].artist or "").casefold().split())
        return any(name and name in artist for name in excluded_artists)
