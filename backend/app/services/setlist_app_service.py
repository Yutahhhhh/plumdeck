from typing import List, Optional, Dict, Any
from sqlmodel import Session, select
from datetime import datetime
import json
import os
import random
import numpy as np

from domain.models.setlist import Setlist, SetlistTrack
from domain.models.track import Track, TrackEmbedding
from domain.models.lyrics import Lyrics
from domain.models.wordplay import WordplayPair
from infra.repositories.setlist_repository import SetlistRepository
from infra.repositories.track_repository import TrackRepository
from infra.repositories.recommendation_repository import RecommendationRepository
from domain.services.setlist_builder import SetlistBuilder, APPROVED_WORDPLAY_BONUS
from infra.database.connection import get_setting_value

from domain.services.target_parameters import sanitize_target_parameters
from utils.audio_math import calculate_mixability_score
from utils.embedding import cosine_similarity
from domain.services.set_duration import calculate_set_duration

class SetlistAppService:
    def __init__(self, session: Session):
        self.session = session
        self.repository = SetlistRepository(session)
        self.track_repository = TrackRepository(session)
        self.recommendation_repository = RecommendationRepository(session)
        self.setlist_builder = SetlistBuilder()

    def _approved_wordplay(self, from_track_id=None):
        """Only human-approved, directed relationships influence generation."""
        stmt = select(WordplayPair).where(WordplayPair.status == "approved")
        if from_track_id is not None:
            stmt = stmt.where(WordplayPair.from_track_id == from_track_id)
        pairs = self.session.exec(stmt).all()
        # Prefer a tested relationship if several words connect the same versions.
        pairs = sorted(pairs, key=lambda p: (p.verification_status != "tested", p.id))
        edges = {}
        for pair in pairs:
            edges.setdefault((pair.from_track_id, pair.to_track_id), pair)
        return edges

    def _include_wordplay_candidates(self, pool, edges, targets, genres, subgenres, exclude_ids):
        """Include approved targets beyond the normal pool cap, with identical filters."""
        missing_ids = sorted({target for _, target in edges} - {c["id"] for c in pool})
        if missing_ids:
            additional = self.recommendation_repository.fetch_candidates_pool(
                targets, genres=genres, subgenres=subgenres,
                limit=len(missing_ids), exclude_ids=exclude_ids, candidate_ids=missing_ids,
            )
            for candidate in additional:
                # A target added solely because of an approved edge is eligible
                # only after that edge's source, not as an unrelated new candidate.
                candidate["wordplay_only_from_ids"] = {
                    source for source, target in edges if target == candidate["id"]
                }
            pool = pool + additional
        return pool

    @staticmethod
    def _wordplay_payload(pair):
        return {
            "pair_id": pair.id,
            "from_track_id": pair.from_track_id,
            "to_track_id": pair.to_track_id,
            "keyword": pair.keyword,
            "source_phrase": pair.source_phrase,
            "target_phrase": pair.target_phrase,
            "source_cue_mode": pair.source_cue_mode,
            "from_timestamp": pair.from_timestamp,
            "source_cue_end_timestamp": pair.source_cue_end_timestamp,
            "to_timestamp": pair.to_timestamp,
            "target_intro_timestamp": pair.target_intro_timestamp,
            "target_landing_timestamp": (
                pair.target_landing_timestamp
                if pair.target_landing_timestamp is not None
                else pair.to_timestamp
            ),
            "transition_notes": pair.transition_notes,
            "source_url": pair.source_url,
            "evidence_type": pair.evidence_type,
            "verification_status": pair.verification_status,
        }

    def _annotate_wordplay(self, tracks, edges):
        for previous, current in zip(tracks, tracks[1:]):
            pair = edges.get((previous["id"], current["id"]))
            if pair:
                current["wordplay_json"] = json.dumps(self._wordplay_payload(pair), ensure_ascii=False)
        return tracks

    def get_setlists(self) -> List[Setlist]:
        return self.repository.find_all()

    def get_setlists_page(self, limit: int, offset: int) -> Dict[str, Any]:
        return self.repository.find_page(limit, offset)

    def create_setlist(self, name: str) -> Setlist:
        setlist = Setlist(name=name)
        return self.repository.create(setlist)

    def update_setlist(self, setlist_id: int, setlist_data: Dict[str, Any]) -> Optional[Setlist]:
        setlist = self.repository.get_by_id(setlist_id)
        if not setlist:
            return None
        
        for key, value in setlist_data.items():
            if hasattr(setlist, key):
                setattr(setlist, key, value)
        
        return self.repository.update(setlist)

    def delete_setlist(self, setlist_id: int) -> bool:
        setlist = self.repository.get_by_id(setlist_id)
        if not setlist:
            return False
        
        self.repository.clear_tracks(setlist_id, commit=False)
        try:
            self.repository.delete(setlist)
        except Exception:
            self.session.rollback()
            raise
        return True

    def get_setlist_tracks(self, setlist_id: int) -> List[Dict[str, Any]]:
        results = self.repository.get_tracks(setlist_id)
        tracks = []
        for st, t, lyrics_content in results:
            t_dict = t.model_dump()
            t_dict["setlist_track_id"] = st.id
            t_dict["position"] = st.position
            t_dict["wordplay_json"] = st.wordplay_json
            t_dict.update({
                "in_ms": st.in_ms,
                "out_ms": st.out_ms,
                "playback_rate": st.playback_rate,
                "extra_duration_ms": st.extra_duration_ms,
                "overlap_next_ms": st.overlap_next_ms,
                "revision": st.revision,
            })
            # JOIN結果から歌詞の有無を判定
            t_dict["has_lyrics"] = bool(lyrics_content and lyrics_content.strip())
            tracks.append(t_dict)
        return tracks

    def get_setlist_tracks_page(self, setlist_id: int, limit: int, offset: int) -> Optional[Dict[str, Any]]:
        if not self.repository.get_by_id(setlist_id):
            return None
        return self.repository.get_tracks_page(setlist_id, limit, offset)

    def add_setlist_track(self, setlist_id: int, track_id: int, position: Optional[int] = None) -> Optional[int]:
        if not self.repository.get_by_id(setlist_id):
            return None
        if not self.track_repository.get_by_id(track_id):
            raise ValueError("Track not found")
        return self.repository.insert_track(setlist_id, track_id, position)

    def remove_setlist_track(self, setlist_id: int, entry_id: int) -> bool:
        if not self.repository.get_by_id(setlist_id):
            return False
        return self.repository.remove_track_entry(setlist_id, entry_id)

    def update_setlist_tracks(self, setlist_id: int, track_data: List[Any]) -> bool:
        setlist = self.repository.get_by_id(setlist_id)
        if not setlist:
            return False

        existing = self.session.exec(
            select(SetlistTrack).where(SetlistTrack.setlist_id == setlist_id)
        ).all()
        existing_by_id = {row.id: row for row in existing}
        ordered_existing = sorted(existing, key=lambda item: (item.position, item.id or 0))
        reusable_by_track: dict[int, list[SetlistTrack]] = {}
        for existing_row in ordered_existing:
            reusable_by_track.setdefault(existing_row.track_id, []).append(existing_row)
        previous_next = {row.id: ordered_existing[index + 1].id if index + 1 < len(ordered_existing) else None
                         for index, row in enumerate(ordered_existing)}
        desired: List[SetlistTrack] = []
        claimed: set[int] = set()
        for i, data in enumerate(track_data):
            if isinstance(data, dict):
                tid = data.get("track_id", data.get("id"))
                entry_id = data.get("entry_id", data.get("setlist_track_id"))
            else:
                tid, entry_id = data, None
                data = {}
            if tid is None:
                continue
            if not self.track_repository.get_by_id(int(tid)):
                raise ValueError(f"Track not found: {tid}")
            row = existing_by_id.get(int(entry_id)) if entry_id else None
            if entry_id and row is None:
                raise ValueError("Setlist entry is stale or belongs to another setlist")
            if row is None:
                # Compatibility for older clients that only send track IDs:
                # preserve the Nth existing occurrence of the same track.
                candidates = reusable_by_track.get(int(tid), [])
                row = next((candidate for candidate in candidates if candidate.id not in claimed), None)
            if row and row.id in claimed:
                raise ValueError("The same setlist entry was supplied more than once")
            if row:
                claimed.add(int(row.id))
                expected = data.get("revision")
                if expected is not None and int(expected) != row.revision:
                    raise ValueError("Setlist entry was changed in another view")
                row.track_id = int(tid)
            else:
                row = SetlistTrack(setlist_id=setlist_id, track_id=int(tid), position=i)
            row.position = i
            for field in ("wordplay_json", "in_ms", "out_ms", "playback_rate", "extra_duration_ms", "overlap_next_ms"):
                if field in data:
                    setattr(row, field, data[field])
            desired.append(row)

        # A connection overlap belongs to an exact adjacent entry pair. Moving
        # either side clears it instead of silently applying it to a new pair.
        for index, row in enumerate(desired):
            next_id = desired[index + 1].id if index + 1 < len(desired) else None
            if row.id and previous_next.get(row.id) != next_id:
                row.overlap_next_ms = 0
        calculate_set_duration([{**row.model_dump(), "duration": self.track_repository.get_by_id(row.track_id).duration}
                                for row in desired])
        for row in existing:
            if row.id not in claimed:
                self.session.delete(row)
        for row in desired:
            row.revision = (row.revision or 0) + 1 if row.id else 1
            self.session.add(row)

        setlist.updated_at = datetime.now()
        self.session.add(setlist)
        try:
            self.session.commit()
        except Exception:
            self.session.rollback()
            raise
        return True

    def set_duration(self, setlist_id: int) -> Dict[str, Any]:
        rows = self.get_setlist_tracks(setlist_id)
        result = calculate_set_duration(rows)
        result["full_duration_ms"] = sum(float(row.get("duration") or 0) * 1000 for row in rows)
        result["track_count"] = len(rows)
        return result

    def export_as_m3u8(self, setlist_id: int) -> str:
        setlist = self.repository.get_by_id(setlist_id)
        if not setlist:
            raise ValueError("Setlist not found")

        results = self.repository.get_tracks(setlist_id)
        lines = ["#EXTM3U"]
        for st, track, lyrics_content in results:
            duration = int(track.duration) if track.duration else -1
            artist = track.artist or "Unknown Artist"
            title_text = track.title or "Unknown Title"
            if track.filepath and not os.path.exists(track.filepath):
                lines.append(f"# MISSING: {artist} - {title_text} ({track.filepath})")
                continue
            lines.append(f"#EXTINF:{duration},{artist} - {title_text}")
            if track.filepath:
                lines.append(track.filepath)
        return "\n".join(lines)

    def validate_export(self, setlist_id: int) -> Dict[str, Any]:
        """エクスポート前にファイルの存在を検証し、欠落している曲を返す"""
        setlist = self.repository.get_by_id(setlist_id)
        if not setlist:
            raise ValueError("Setlist not found")

        results = self.repository.get_tracks(setlist_id)
        missing = []
        for st, track, _lyrics_content in results:
            if not track.filepath or not os.path.exists(track.filepath):
                missing.append({
                    "id": track.id,
                    "title": track.title,
                    "artist": track.artist,
                    "filepath": track.filepath,
                })
        return {"total": len(results), "missing": missing}

    def recommend_next_track(
        self,
        track_id: int,
        limit: int = 20,
        target_params: Optional[Dict[str, Any]] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None
    ) -> List[Dict[str, Any]]:
        target_track = self.track_repository.get_by_id(track_id)
        if not target_track:
            raise ValueError("Track not found")

        validated_targets = sanitize_target_parameters(target_params)

        if "bpm" not in validated_targets:
            validated_targets["bpm"] = target_track.bpm

        pool = self.recommendation_repository.fetch_candidates_pool(
            validated_targets,
            genres=genres,
            subgenres=subgenres,
            limit=200,
            exclude_ids=[track_id]
        )
        wordplay = self._approved_wordplay(track_id)
        pool = self._include_wordplay_candidates(
            pool, wordplay, validated_targets, genres, subgenres, [track_id]
        )

        target_vec = None
        target_emb = self.session.get(TrackEmbedding, track_id)
        if target_emb and target_emb.embedding_json:
            try:
                target_vec = np.array(json.loads(target_emb.embedding_json))
            except: pass

        scored_candidates = []
        for cand in pool:
            vec_sim = cosine_similarity(target_vec, cand["vector"],
                                        target_emb.model_name if target_emb else None, cand.get("embedding_model"))
            
            score = calculate_mixability_score(
                target_bpm=target_track.bpm,
                target_key=target_track.key,
                candidate_bpm=cand["track"].bpm,
                candidate_key=cand["track"].key,
                vector_similarity=vec_sim
            )
            
            track_dict = cand["track"].model_dump()
            # リポジトリの pool 取得時に計算された has_lyrics を注入
            track_dict["has_lyrics"] = cand.get("has_lyrics", False)
            pair = wordplay.get((track_id, cand["id"]))
            if pair:
                score += APPROVED_WORDPLAY_BONUS
                track_dict["wordplay_json"] = json.dumps(self._wordplay_payload(pair), ensure_ascii=False)
            scored_candidates.append((track_dict, score))
        
        scored_candidates.sort(key=lambda x: x[1], reverse=True)
        return [c[0] for c in scored_candidates[:limit]]

    def recommend_next_track_page(
        self,
        track_id: int,
        limit: int = 100,
        offset: int = 0,
        target_params: Optional[Dict[str, Any]] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
    ) -> Dict[str, Any]:
        target_track = self.track_repository.get_by_id(track_id)
        if not target_track:
            raise ValueError("Track not found")
        validated_targets = sanitize_target_parameters(target_params)
        if "bpm" not in validated_targets:
            validated_targets["bpm"] = target_track.bpm
        page = self.recommendation_repository.fetch_ranked_page(
            target_track, validated_targets, genres=genres, subgenres=subgenres,
            limit=limit, offset=offset,
        )
        wordplay = self._approved_wordplay(track_id)
        for item in page["items"]:
            pair = wordplay.get((track_id, item["id"]))
            if pair:
                item["wordplay_json"] = json.dumps(self._wordplay_payload(pair), ensure_ascii=False)
        return page

    def generate_auto_setlist(
        self,
        target_params: Optional[Dict[str, Any]] = None,
        limit: Optional[int] = None,
        min_length: Optional[int] = None,
        max_length: Optional[int] = None,
        seed_track_ids: Optional[List[int]] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None
    ) -> List[Dict[str, Any]]:
        # 曲数解決: limit > min/max 両方 > min のみ > max のみ > UI 設定のデフォルト値
        if limit is not None:
            target_length = limit
        elif min_length is not None and max_length is not None:
            target_length = random.randint(min_length, max_length)
        elif min_length is not None:
            target_length = min_length
        elif max_length is not None:
            target_length = max_length
        else:
            try:
                target_length = int(get_setting_value(self.session, "setlist_default_length", "10"))
            except (TypeError, ValueError):
                target_length = 10

        validated_targets = sanitize_target_parameters(target_params)

        seeds = []
        if seed_track_ids:
            seed_objs = self.session.exec(select(Track).where(Track.id.in_(seed_track_ids))).all()
            seeds_by_id = {t.id: t for t in seed_objs}
            # SQL IN does not preserve the order supplied by the DJ.
            for tid in dict.fromkeys(seed_track_ids):
                t = seeds_by_id.get(tid)
                if t is None:
                    continue
                emb = self.session.get(TrackEmbedding, t.id)
                vec = self.recommendation_repository._parse_embedding(emb.embedding_json) if emb else None
                # シード曲についても歌詞情報を取得
                ly = self.session.get(Lyrics, t.id)
                seeds.append({
                    "id": t.id, 
                    "track": t, 
                    "vector": vec,
                    "embedding_model": emb.model_name if emb else None,
                    "has_lyrics": bool(ly and ly.content.strip())
                })

        exclude_ids = seed_track_ids or []
        pool = self.recommendation_repository.fetch_candidates_pool(
            validated_targets,
            genres=genres,
            subgenres=subgenres,
            limit=300,
            exclude_ids=exclude_ids
        )

        # pool と seeds から Track オブジェクトのリストを取得
        wordplay = self._approved_wordplay()
        pool = self._include_wordplay_candidates(
            pool, wordplay, validated_targets, genres, subgenres, exclude_ids
        )
        result_tracks = self.setlist_builder.build_chain(
            pool, seeds, target_length, validated_targets, approved_wordplay_edges=wordplay.keys()
        )
        
        enriched_result = []
        for t_obj in result_tracks:
            t_dict = t_obj.model_dump()
            # pool または seeds から has_lyrics 情報を探して再注入
            matching_cand = next((c for c in pool if c["id"] == t_obj.id), None)
            if not matching_cand:
                matching_cand = next((s for s in seeds if s["id"] == t_obj.id), None)
            
            t_dict["has_lyrics"] = matching_cand.get("has_lyrics", False) if matching_cand else False
            enriched_result.append(t_dict)
        
        return self._annotate_wordplay(enriched_result, wordplay)

    def generate_path_setlist(
        self,
        start_track_id: int,
        end_track_id: int,
        length: int,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None
    ) -> List[Dict[str, Any]]:
        start_track = self.track_repository.get_by_id(start_track_id)
        end_track = self.track_repository.get_by_id(end_track_id)
        if not start_track or not end_track:
            raise ValueError("Start or End track not found")

        def make_node(t):
            emb = self.session.get(TrackEmbedding, t.id)
            vec = self.recommendation_repository._parse_embedding(emb.embedding_json) if emb else None
            ly = self.session.get(Lyrics, t.id)
            return {
                "id": t.id, 
                "track": t, 
                "vector": vec,
                "embedding_model": emb.model_name if emb else None,
                "has_lyrics": bool(ly and ly.content.strip())
            }
        
        start_node = make_node(start_track)
        end_node = make_node(end_track)
        
        pool = self.recommendation_repository.fetch_candidates_pool(
            {"bpm": ((start_track.bpm or 0) + (end_track.bpm or 0)) / 2},
            genres=genres,
            subgenres=subgenres,
            limit=400,
            exclude_ids=[start_track_id, end_track_id]
        )
        
        wordplay = self._approved_wordplay()
        pool = self._include_wordplay_candidates(
            pool, wordplay, {"bpm": ((start_track.bpm or 0) + (end_track.bpm or 0)) / 2},
            genres, subgenres, [start_track_id, end_track_id],
        )
        result_tracks = self.setlist_builder.build_path(
            pool, start_node, end_node, length, approved_wordplay_edges=wordplay.keys()
        )
        
        enriched_result = []
        for t_obj in result_tracks:
            t_dict = t_obj.model_dump()
            # pool, start, end から has_lyrics 情報をマッピング
            matching_cand = next((c for c in pool if c["id"] == t_obj.id), None)
            if matching_cand:
                t_dict["has_lyrics"] = matching_cand.get("has_lyrics", False)
            elif t_obj.id == start_track_id:
                t_dict["has_lyrics"] = start_node["has_lyrics"]
            elif t_obj.id == end_track_id:
                t_dict["has_lyrics"] = end_node["has_lyrics"]
            else:
                t_dict["has_lyrics"] = False
                
            enriched_result.append(t_dict)
            
        return self._annotate_wordplay(enriched_result, wordplay)
