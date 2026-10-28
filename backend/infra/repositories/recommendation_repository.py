from typing import List, Tuple, Optional, Dict, Any
from sqlmodel import Session, select, text
from domain.models.track import Track, TrackEmbedding
from domain.models.lyrics import Lyrics
from utils.embedding import LEGACY_MODELS, embedding_space
from domain.constants import EMBEDDING_DIM
from utils.audio_math import DEFAULT_MIXABILITY_WEIGHTS
import numpy as np
import json
import math

class RecommendationRepository:
    def __init__(self, session: Session):
        self.session = session

    def _parse_embedding(self, embedding_json: Optional[str]) -> Optional[np.ndarray]:
        if not embedding_json:
            return None
        try:
            vec = np.array(json.loads(embedding_json), dtype=float)
            return vec if vec.ndim == 1 and vec.size > 0 and np.isfinite(vec).all() else None
        except:
            return None

    @staticmethod
    def _model_condition(model_name):
        if embedding_space(model_name) == "musicnn":
            return TrackEmbedding.model_name.in_(LEGACY_MODELS) | (TrackEmbedding.model_name == None)
        return TrackEmbedding.model_name == model_name

    def get_embedding_model(self, track_id: int) -> str:
        emb = self.session.get(TrackEmbedding, track_id)
        return embedding_space(emb.model_name if emb else None)

    def get_candidate_vectors(self, mode: str = "genre", model_name=None) -> np.ndarray:
        query = select(TrackEmbedding.embedding_json).join(Track)
        if model_name is not None:
            query = query.where(self._model_condition(model_name))
        if mode == "subgenre":
            query = query.where((Track.subgenre == None) | (Track.subgenre == ""))
        else:
            query = query.where(Track.is_genre_verified == False)
            
        candidates = self.session.exec(query).all()
        vectors = [self._parse_embedding(emb) for emb in candidates]
        vectors = [v for v in vectors if v is not None]
        return np.array(vectors) if vectors else np.array([])

    def get_candidates_with_ids(self, mode: str = "genre", model_name=None) -> Tuple[List[int], np.ndarray]:
        query = select(Track.id, TrackEmbedding.embedding_json).join(TrackEmbedding)
        if model_name is not None:
            query = query.where(self._model_condition(model_name))
        if mode == "subgenre":
            query = query.where((Track.subgenre == None) | (Track.subgenre == ""))
        else:
            query = query.where(Track.is_genre_verified == False)
            
        results = self.session.exec(query).all()
        ids = []
        vectors = []
        for tid, emb in results:
            vec = self._parse_embedding(emb)
            if vec is not None:
                ids.append(tid)
                vectors.append(vec)
        return ids, np.array(vectors) if vectors else np.array([])

    def get_parent_vectors(self) -> List[Tuple[int, np.ndarray]]:
        stmt = select(Track.id, TrackEmbedding.embedding_json, TrackEmbedding.model_name).join(TrackEmbedding).where(Track.is_genre_verified == True)
        results = self.session.exec(stmt).all()
        parents = []
        for tid, emb, model in results:
            vec = self._parse_embedding(emb)
            if vec is not None:
                parents.append((tid, vec, embedding_space(model)))
        return parents

    def get_verified_tracks_with_embeddings(self, exclude_track_id: int = None, model_name=None) -> List[Tuple[str, np.ndarray]]:
        query = select(Track.genre, TrackEmbedding.embedding_json).join(TrackEmbedding).where(Track.is_genre_verified == True)
        if model_name is not None:
            query = query.where(self._model_condition(model_name))
        if exclude_track_id:
            query = query.where(Track.id != exclude_track_id)
        
        results = self.session.exec(query).all()
        data = []
        for genre, emb_json in results:
            if not genre: continue
            vec = self._parse_embedding(emb_json)
            if vec is not None:
                data.append((genre, vec))
        return data

    def get_track_embedding(self, track_id: int) -> Optional[np.ndarray]:
        emb = self.session.get(TrackEmbedding, track_id)
        return self._parse_embedding(emb.embedding_json) if emb else None

    def get_tracks_by_ids(self, track_ids: List[int]) -> Dict[int, Track]:
        if not track_ids:
            return {}
        stmt = select(Track).where(Track.id.in_(track_ids))
        tracks = self.session.exec(stmt).all()
        return {t.id: t for t in tracks}

    @staticmethod
    def _to_float(value) -> Optional[float]:
        if isinstance(value, bool):
            return None
        try:
            number = float(value)
            return number if math.isfinite(number) else None
        except (TypeError, ValueError):
            return None

    def fetch_candidates_pool(
        self,
        target_params: Dict[str, Any],
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
        limit: int = 200,
        exclude_ids: List[int] = None,
        candidate_ids: Optional[List[int]] = None,
    ) -> List[Dict[str, Any]]:
        """
        指定された構造化ターゲットとジャンルに基づき、歌詞情報とリリース年を含めて候補を取得
        """
        query_str = """
            SELECT
                t.id, t.title, t.artist, t.bpm, t.key, t.genre, t.subgenre,
                t.duration, t.album, t.filepath, t.year,
                t.energy, t.danceability, t.brightness, t.loudness, t.contrast, t.noisiness,
                te.embedding_json, te.model_name,
                (l.content IS NOT NULL AND length(trim(l.content)) > 0) as db_has_lyrics
            FROM tracks t
            LEFT JOIN track_embeddings te ON t.id = te.track_id
            LEFT JOIN lyrics l ON t.id = l.track_id
            WHERE 1=1
        """
        params = {}
        order_clauses = []

        # Id lists travel as one comma-separated string. Bound element by element,
        # duckdb probes every value for pandas types, which costs about a second
        # for a library-sized list on every assist request.
        if candidate_ids is not None:
            if not candidate_ids:
                return []
            query_str += " AND t.id IN (SELECT CAST(unnest(string_split(:candidate_ids, ',')) AS BIGINT))"
            params["candidate_ids"] = ",".join(str(int(track_id)) for track_id in candidate_ids)

        if exclude_ids:
            query_str += " AND t.id NOT IN (SELECT CAST(unnest(string_split(:exclude_ids, ',')) AS BIGINT))"
            params["exclude_ids"] = ",".join(str(int(track_id)) for track_id in exclude_ids)

        genre_conditions = []
        if genres:
            genre_conditions.append("t.genre IN :genres")
            params["genres"] = tuple(genres)
        if subgenres:
            genre_conditions.append("t.subgenre IN :subgenres")
            params["subgenres"] = tuple(subgenres)
        if genre_conditions:
            query_str += " AND (" + " OR ".join(genre_conditions) + ")"

        target_bpm = self._to_float(target_params.get("bpm"))
        if target_bpm is not None and target_bpm > 0:
            # Use the same half/double-time interpretations as the transition scorer.
            tempo_distance = """CASE WHEN t.bpm > 0 AND isfinite(t.bpm) THEN LEAST(
                ABS(LOG2(t.bpm / :target_bpm)),
                ABS(LOG2(t.bpm / :target_bpm) - 1),
                ABS(LOG2(t.bpm / :target_bpm) + 1)
            ) ELSE 1000 END"""
            params["target_bpm"] = target_bpm
            # A caller with a narrow tempo window cuts the pool to it.
            max_distance = self._to_float(target_params.get("max_tempo_distance"))
            params["max_tempo_distance"] = max_distance if max_distance and max_distance > 0 else math.log2(1.4)
            query_str += f" AND (({tempo_distance}) <= :max_tempo_distance OR t.bpm = 0 OR t.bpm IS NULL)"
            order_clauses.append(tempo_distance)

        for name, operator in (("year_min", ">="), ("year_max", "<=")):
            year = self._to_float(target_params.get(name))
            if year is not None:
                query_str += f" AND t.year {operator} :{name}"
                params[name] = int(year)

        target_energy = self._to_float(target_params.get("energy"))
        if target_energy is not None:
            query_str += " AND t.energy BETWEEN :min_energy AND :max_energy"
            params["min_energy"] = max(0.0, target_energy - 0.4)
            params["max_energy"] = min(1.0, target_energy + 0.4)
            order_clauses.append("ABS(t.energy - :order_energy)")
            params["order_energy"] = target_energy

        for feature in ("danceability", "brightness", "noisiness"):
            target = self._to_float(target_params.get(feature))
            if target is not None:
                order_clauses.append(f"COALESCE(ABS(t.{feature} - :order_{feature}), 1.0)")
                params[f"order_{feature}"] = target

        if order_clauses:
            query_str += " ORDER BY (" + " + ".join(order_clauses) + ") ASC, t.id ASC"
        else:
            query_str += " ORDER BY t.created_at DESC, t.id ASC"

        query_str += " LIMIT :pool_limit"
        params["pool_limit"] = int(limit)

        results = self.session.connection().execute(text(query_str), params).fetchall()
        
        candidates = []
        for row in results:
            vec = self._parse_embedding(row.embedding_json)
            has_ly = bool(row.db_has_lyrics)
            
            track_obj = Track(
                id=row.id, title=row.title, artist=row.artist, bpm=row.bpm, key=row.key, 
                genre=row.genre, subgenre=row.subgenre or "", duration=row.duration, 
                album=row.album or "", filepath=row.filepath, year=row.year,
                energy=row.energy, danceability=row.danceability, brightness=row.brightness, 
                loudness=row.loudness, contrast=row.contrast, noisiness=row.noisiness,
                has_lyrics=has_ly # オブジェクトに直接セット
            )
            
            candidates.append({
                "id": row.id,
                "track": track_obj,
                "vector": vec,
                "embedding_model": row.model_name,
                "has_lyrics": has_ly # 辞書側にもセット（サービス層での利用を確実に）
            })
            
        return candidates

    def fetch_ranked_page(
        self,
        target_track: Track,
        target_params: Dict[str, Any],
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
        limit: int = 100,
        offset: int = 0,
    ) -> Dict[str, Any]:
        """Score the complete eligible set in SQL, then apply pagination.

        This avoids the old "take 200, score, then slice" behavior, whose later
        pages and total could not describe the actual filtered recommendation set.
        """
        where = ["t.id <> :track_id"]
        params: Dict[str, Any] = {
            "track_id": int(target_track.id), "limit": int(limit), "offset": int(offset),
        }
        candidate_order_terms = []
        genre_parts = []
        if genres:
            genre_parts.append("t.genre IN :genres")
            params["genres"] = tuple(genres)
        if subgenres:
            genre_parts.append("t.subgenre IN :subgenres")
            params["subgenres"] = tuple(subgenres)
        if genre_parts:
            where.append("(" + " OR ".join(genre_parts) + ")")

        filter_target_bpm = self._to_float(target_params.get("bpm"))
        if filter_target_bpm is None or filter_target_bpm <= 0:
            filter_target_bpm = self._to_float(target_track.bpm)
        if filter_target_bpm is not None and filter_target_bpm > 0:
            params["filter_target_bpm"] = filter_target_bpm
            params["max_tempo_distance"] = math.log2(1.4)
            filter_tempo_distance = """CASE WHEN t.bpm > 0 AND isfinite(t.bpm) THEN LEAST(
                ABS(LOG2(t.bpm / :filter_target_bpm)),
                ABS(LOG2(t.bpm / :filter_target_bpm) - 1),
                ABS(LOG2(t.bpm / :filter_target_bpm) + 1)
            ) ELSE 1000 END"""
            where.append(
                f"((t.bpm > 0 AND isfinite(t.bpm) AND ({filter_tempo_distance}) <= :max_tempo_distance) "
                "OR t.bpm = 0 OR t.bpm IS NULL)"
            )
            candidate_order_terms.append(filter_tempo_distance)
        score_target_bpm = self._to_float(target_track.bpm)
        if score_target_bpm is not None and score_target_bpm > 0:
            params["score_target_bpm"] = score_target_bpm
            score_tempo_distance = """CASE WHEN t.bpm > 0 AND isfinite(t.bpm) THEN LEAST(
                ABS(LOG2(t.bpm / :score_target_bpm)),
                ABS(LOG2(t.bpm / :score_target_bpm) - 1),
                ABS(LOG2(t.bpm / :score_target_bpm) + 1)
            ) ELSE 1000 END"""
            bpm_score = f"EXP(-0.5 * POWER(({score_tempo_distance}) / :bpm_sigma, 2))"
            params["bpm_sigma"] = math.log2(1.08)
        else:
            bpm_score = "0"

        for name, operator in (("year_min", ">="), ("year_max", "<=")):
            year = self._to_float(target_params.get(name))
            if year is not None:
                where.append(f"t.year {operator} :{name}")
                params[name] = int(year)
        target_energy = self._to_float(target_params.get("energy"))
        if target_energy is not None:
            where.append("t.energy BETWEEN :min_energy AND :max_energy")
            params["min_energy"] = max(0.0, target_energy - 0.4)
            params["max_energy"] = min(1.0, target_energy + 0.4)
            params["order_energy"] = target_energy
            candidate_order_terms.append("ABS(t.energy - :order_energy)")
        for feature in ("danceability", "brightness", "noisiness"):
            target = self._to_float(target_params.get(feature))
            if target is not None:
                params[f"order_{feature}"] = target
                candidate_order_terms.append(
                    f"COALESCE(ABS(t.{feature} - :order_{feature}), 1.0)"
                )

        target_embedding = self.session.get(TrackEmbedding, target_track.id)
        target_vector = self._parse_embedding(target_embedding.embedding_json) if target_embedding else None
        if (target_embedding and target_vector is not None
                and target_vector.size == EMBEDDING_DIM and np.linalg.norm(target_vector) > 0):
            params["target_vec"] = target_embedding.embedding_json
            if embedding_space(target_embedding.model_name) == "musicnn":
                model_filter = "(te.model_name IN :legacy_models OR te.model_name IS NULL)"
                params["legacy_models"] = tuple(LEGACY_MODELS)
            else:
                model_filter = "te.model_name = :target_model"
                params["target_model"] = target_embedding.model_name
            # The target vector was validated above (finite, nonzero norm).
            candidate_vector = f"TRY_CAST(CASE WHEN {model_filter} THEN te.embedding_json END AS FLOAT[{EMBEDDING_DIM}])"
            cosine = f"TRY(array_cosine_similarity(candidate_vector, CAST(:target_vec AS FLOAT[{EMBEDDING_DIM}])))"
            # DuckDB returns -1 for a zero/NaN vector, so validate the norm
            # before treating its cosine result as measured dissimilarity.
            measured = f"""CASE WHEN isfinite(candidate_norm) AND candidate_norm > 0 AND isfinite({cosine})
                THEN {cosine} END"""
        else:
            candidate_vector = f"CAST(NULL AS FLOAT[{EMBEDDING_DIM}])"
            measured = "CAST(NULL AS DOUBLE)"

        params["bpm_weight"] = DEFAULT_MIXABILITY_WEIGHTS["bpm"]
        params["vector_weight"] = DEFAULT_MIXABILITY_WEIGHTS["vector"]
        # GREATEST/LEAST skip NULLs in DuckDB; keep "nothing measured" NULL.
        vector_score = """CASE WHEN t.vector_similarity IS NOT NULL
            THEN GREATEST(0.0, LEAST(1.0, t.vector_similarity)) END"""
        score = f"""(
            (({bpm_score}) * :bpm_weight + COALESCE({vector_score}, 0.0) * :vector_weight)
            / (:bpm_weight + CASE WHEN t.vector_similarity IS NOT NULL THEN :vector_weight ELSE 0 END) +
            CASE WHEN EXISTS (
                SELECT 1 FROM wordplay_pairs wp
                WHERE wp.from_track_id=:track_id AND wp.to_track_id=t.id AND wp.status='approved'
            ) THEN 0.25 ELSE 0 END
        )"""
        where_sql = " AND ".join(where)
        candidate_order = (
            "(" + " + ".join(candidate_order_terms) + ") ASC, t.id ASC"
            if candidate_order_terms else "t.created_at DESC, t.id ASC"
        )
        count_sql = f"SELECT count(*) FROM tracks t WHERE {where_sql}"
        total = int(self.session.connection().execute(text(count_sql), params).scalar_one())
        # Similarity is computed once per candidate. A pending/incomparable
        # embedding takes the median similarity of the filtered candidates
        # (utils.embedding.neutral_similarity), so it ranks like a typical one.
        page_sql = f"""
            WITH vectors AS (
                SELECT t.*, l.content AS lyrics,
                       (l.content IS NOT NULL AND length(trim(l.content)) > 0) AS has_lyrics,
                       {candidate_vector} AS candidate_vector
                FROM tracks t
                LEFT JOIN track_embeddings te ON te.track_id=t.id
                LEFT JOIN lyrics l ON l.track_id=t.id
                WHERE {where_sql}
            ), norms AS (
                SELECT *, TRY(array_inner_product(candidate_vector, candidate_vector)) AS candidate_norm
                FROM vectors
            ), measured AS (
                SELECT * EXCLUDE (candidate_vector, candidate_norm), {measured} AS measured_similarity
                FROM norms
            ), imputed AS (
                SELECT *, COALESCE(measured_similarity, MEDIAN(measured_similarity) OVER ()) AS vector_similarity
                FROM measured
            )
            SELECT t.* EXCLUDE (measured_similarity, vector_similarity),
                   {score} AS recommendation_score
            FROM imputed t
            ORDER BY recommendation_score DESC, {candidate_order}
            LIMIT :limit OFFSET :offset
        """
        rows = self.session.connection().execute(text(page_sql), params).fetchall()
        items = [dict(row._mapping) for row in rows]
        return {
            "items": items, "total": total, "limit": int(limit), "offset": int(offset),
            "has_more": int(offset) + len(items) < total,
        }
