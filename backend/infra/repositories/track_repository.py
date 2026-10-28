from typing import List, Optional, Dict, Any, Union
from sqlmodel import Session, select, or_, and_, col, text
from sqlalchemy import case, func, bindparam
import json
import re

from domain.models.track import Track, TrackEmbedding
from domain.models.lyrics import Lyrics
from domain.constants import EMBEDDING_DIM
from utils.embedding import LEGACY_MODELS, embedding_space
from utils.audio_math import CAMELOT_ADJACENCY, KEY_TO_CAMELOT, normalize_key

# Camelot ホイール順。辞書順だと隣接キーがばらけて選曲に使えない。
# 対応表はレコメンドと同じ utils.audio_math を唯一の出典にする（表記ゆれも同じ扱いになる）。
CAMELOT_ORDER = {
    **{spelling.lower(): (int(code[:-1]) - 1) * 2 + (1 if code[-1].upper() == "A" else 2)
       for spelling, code in KEY_TO_CAMELOT.items()},
    **{code.lower(): (int(code[:-1]) - 1) * 2 + (1 if code[-1].upper() == "A" else 2)
       for code in CAMELOT_ADJACENCY},
}

def _key_spellings(key: str) -> list[str]:
    """Lowercase spellings stored for the same key (e.g. 'D# Minor' -> 'eb minor', '2a')."""
    code = normalize_key(key)
    if code is None:
        return [key.strip().lower()]
    return [spelling.lower() for spelling, value in KEY_TO_CAMELOT.items() if value == code] + [code.lower()]


# 一覧のヘッダから並べ替えられる列。ここに無い値は無視して既定順のままにする。
SORTABLE_FIELDS = ("title", "artist", "album", "genre", "subgenre", "bpm", "key", "duration", "year", "energy", "danceability", "created_at")
TEXT_SORT_FIELDS = ("title", "artist", "album", "genre", "subgenre")


def apply_sort(query, sort: Optional[str], order: str):
    """未知の列や未指定は既定の並び（+ id のタイブレーク）。明示指定はそれを置き換える。

    値の無い曲（キー未解析、タイトル空）は昇順・降順のどちらでも末尾に置く。
    ページ境界がぶれないよう、最後に必ず id を足す。
    """
    if not sort or sort not in SORTABLE_FIELDS:
        return query.order_by(Track.id.asc())
    # 類似度順などの既定の並びより、ユーザーが選んだ列を優先する。
    query = query.order_by(None)
    descending = str(order).lower() == "desc"
    if sort == "key":
        # 辞書順ではなく Camelot ホイール順。隣接キーが並ぶほうが選曲に使える。
        # 表記ゆれ（"F Minor" / "f minor" / "8A"）を吸収してから順位に落とす。
        value = case(CAMELOT_ORDER, value=func.lower(func.trim(Track.key)), else_=None)
    elif sort in TEXT_SORT_FIELDS:
        value = func.nullif(func.lower(func.coalesce(getattr(Track, sort), "")), "")
    else:
        value = getattr(Track, sort)
    missing = case((value.is_(None), 1), else_=0)
    return query.order_by(missing.asc(), value.desc() if descending else value.asc(), Track.id.asc())


class TrackRepository:
    def __init__(self, session: Session):
        self.session = session

    def _build_global_search_condition(self, q: str):
        """
        Library-wide search.

        File Explorer users often search by filename, while Library rows show
        parsed title/artist metadata. Match both the raw phrase and forgiving
        tokens across visible metadata plus filepath.
        """
        search_fields = [
            Track.title,
            Track.artist,
            Track.album,
            Track.genre,
            Track.subgenre,
            Track.filepath,
        ]
        raw = q.strip()
        raw_condition = or_(*[
            col(field).ilike(f"%{raw}%") for field in search_fields
        ])

        tokens = [
            token
            for token in re.findall(r"[^\W_]+(?:'[^\W_]+)?", raw, flags=re.UNICODE)
            if token.lower() not in {"mp3", "wav", "aiff", "aif", "flac", "m4a", "aac", "ogg"}
        ]
        if len(tokens) <= 1:
            return raw_condition

        token_conditions = [
            or_(*[col(field).ilike(f"%{token}%") for field in search_fields])
            for token in tokens
        ]
        return or_(raw_condition, and_(*token_conditions))

    def get_by_id(self, track_id: int) -> Optional[Track]:
        return self.session.get(Track, track_id)

    def find_all(self, offset: int = 0, limit: int = 100) -> List[Track]:
        return self.session.exec(select(Track).offset(offset).limit(limit)).all()

    def update_genre(self, track_id: int, genre: str, verified: bool = True) -> Optional[Track]:
        track = self.get_by_id(track_id)
        if track:
            track.genre = genre
            track.is_genre_verified = verified
            self.session.add(track)
            self.session.commit()
            self.session.refresh(track)
        return track

    def get_similar_tracks(self, track_id: int, limit: int = 20) -> List[Dict[str, Any]]:
        """
        ベクトル検索: 指定された track_id に類似するトラックを取得する。
        Lyricsテーブルと外部結合し、has_lyrics フラグを付与して返します。
        """
        target_embedding = self.session.exec(
            select(TrackEmbedding).where(TrackEmbedding.track_id == track_id)
        ).first()
        
        if not target_embedding:
            raise ValueError("Track embedding not found. Please analyze the track first.")

        try:
            vec_str = target_embedding.embedding_json

            # TrackEmbeddingをJOINして類似度計算、LyricsをOUTER JOINして歌詞の有無を確認
            # DuckDB 独自の array_cosine_similarity を使用
            # (ベクトル文字列はバインドパラメータで渡し、次元数は定数で共有)
            similarity_order = text(
                f"array_cosine_similarity("
                f"CAST(track_embeddings.embedding_json AS FLOAT[{EMBEDDING_DIM}]), "
                f"CAST(:target_vec AS FLOAT[{EMBEDDING_DIM}])) DESC"
            ).bindparams(bindparam("target_vec", value=vec_str))
            query = (
                select(Track, Lyrics.content)
                .join(TrackEmbedding, Track.id == TrackEmbedding.track_id)
                .outerjoin(Lyrics, Track.id == Lyrics.track_id)
                .where(Track.id != track_id)
                .order_by(similarity_order)
                .limit(limit)
            )
            if embedding_space(target_embedding.model_name) == "musicnn":
                query = query.where(or_(TrackEmbedding.model_name.in_(LEGACY_MODELS), TrackEmbedding.model_name == None))
            else:
                query = query.where(TrackEmbedding.model_name == target_embedding.model_name)
            
            results = self.session.exec(query).all()
            
            final_tracks = []
            for track, lyrics_content in results:
                track_data = track.model_dump()
                # 歌詞が存在し、かつ空文字でない場合に has_lyrics を True に設定
                track_data["has_lyrics"] = bool(lyrics_content and lyrics_content.strip())
                final_tracks.append(track_data)
            
            return final_tracks
        except Exception as e:
            print(f"Vector search error: {e}")
            raise e

    def _apply_search_conditions(
        self,
        query,
        status: str = "all",
        q: Optional[str] = None,
        title: Optional[str] = None,
        artist: Optional[str] = None,
        album: Optional[str] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
        key: Optional[str] = None,
        bpm: Optional[float] = None,
        bpm_range: float = 5.0,
        min_duration: Optional[float] = None,
        max_duration: Optional[float] = None,
        min_energy: Optional[float] = None,
        max_energy: Optional[float] = None,
        min_danceability: Optional[float] = None,
        max_danceability: Optional[float] = None,
        min_brightness: Optional[float] = None,
        max_brightness: Optional[float] = None,
        min_year: Optional[int] = None,
        max_year: Optional[int] = None,
        year_status: str = "all",
        lyrics_status: str = "all",
        lyrics: Optional[str] = None,
        target_params: Optional[Dict[str, float]] = None,
        already_joined_lyrics: bool = False
    ):
        """検索条件や構造化された特徴量ターゲットをクエリに適用する。"""
        
        # 1. MCPクライアント等が解釈した特徴量ターゲットとの距離でソート
        # target_params はサービス層で検証済みの想定だが、
        # 外部から直接渡されるケースに備えて数値以外は無視する
        def _safe_num(value) -> Optional[float]:
            if isinstance(value, bool):
                return None
            try:
                return float(value)
            except (TypeError, ValueError):
                return None

        if target_params:
            year_min_val = _safe_num(target_params.get("year_min"))
            if min_year is None and year_min_val is not None:
                min_year = int(year_min_val)
            year_max_val = _safe_num(target_params.get("year_max"))
            if max_year is None and year_max_val is not None:
                max_year = int(year_max_val)

            dist_expr = 0
            bpm_val = _safe_num(target_params.get("bpm"))
            if bpm_val is not None and bpm_val > 0:
                dist_expr += (Track.bpm - bpm_val) * (Track.bpm - bpm_val) * 0.0001
            for feat in ("energy", "danceability", "brightness", "noisiness"):
                feat_val = _safe_num(target_params.get(feat))
                if feat_val is not None:
                    feat_col = getattr(Track, feat)
                    dist_expr += (feat_col - feat_val) * (feat_col - feat_val)

            if isinstance(dist_expr, int):
                # 有効な数値パラメータが1つもない場合は通常ソート
                query = query.order_by(Track.created_at.desc())
            else:
                query = query.order_by(dist_expr)
        else:
            query = query.order_by(Track.created_at.desc())
        
        # 2. 解析ステータスフィルタ
        if status == "analyzed":
            query = query.where(Track.bpm > 0)
        elif status == "unanalyzed":
            query = query.where(or_(Track.bpm == None, Track.bpm == 0))
        
        # 3. 基本メタデータフィルタ
        if q:
            query = query.where(self._build_global_search_condition(q))
        if title: query = query.where(col(Track.title).ilike(f"%{title}%"))
        if artist: query = query.where(col(Track.artist).ilike(f"%{artist}%"))
        if album: query = query.where(col(Track.album).ilike(f"%{album}%"))
        
        # 4. リリース年フィルタ
        if year_status == "set":
            query = query.where(and_(Track.year.is_not(None), Track.year > 0))
        elif year_status == "unset":
            query = query.where(or_(Track.year.is_(None), Track.year == 0))
        
        if min_year is not None: query = query.where(Track.year >= min_year)
        if max_year is not None: query = query.where(Track.year <= max_year)

        # 5. 歌詞ステータスフィルタ
        if lyrics_status != "all":
            valid_lyrics_ids = select(Lyrics.track_id).where(
                and_(
                    Lyrics.track_id != None,
                    Lyrics.content != None,
                    func.trim(Lyrics.content) != ""
                )
            )

            if lyrics_status == "set":
                query = query.where(Track.id.in_(valid_lyrics_ids))
            elif lyrics_status == "unset":
                query = query.where(Track.id.not_in(valid_lyrics_ids))
        
        # 歌詞テキスト検索用の結合
        if lyrics:
            if not already_joined_lyrics:
                query = query.outerjoin(Lyrics, Track.id == Lyrics.track_id)
            query = query.where(col(Lyrics.content).ilike(f"%{lyrics}%"))

        # 6. ジャンルフィルタ
        if genres:
            query = query.where(col(Track.genre).in_(genres))
        
        # 7. サブジャンルフィルタ
        if subgenres:
            query = query.where(col(Track.subgenre).in_(subgenres))
        
        # 8. キー / スケールフィルタ
        # Stored keys are 'C minor' / 'Eb minor' while the UI sends 'C Minor' /
        # 'D# Minor': match case-insensitively and across enharmonic spellings.
        if key and key != "":
            if key in ["Major", "Minor"]:
                query = query.where(func.lower(Track.key).like(f"%{key.lower()}"))
            else:
                query = query.where(func.lower(func.trim(Track.key)).in_(_key_spellings(key)))
        
        # 9. BPMフィルタ (± bpm_range)
        if bpm and bpm > 0:
            targets = [bpm, bpm * 0.5, bpm * 2.0]
            bpm_conditions = [
                and_(Track.bpm >= (t - bpm_range), Track.bpm <= (t + bpm_range))
                for t in targets
            ]
            query = query.where(or_(*bpm_conditions))
        
        # 10. その他オーディオ特徴量フィルタ
        if min_energy is not None: query = query.where(Track.energy >= min_energy)
        if max_energy is not None: query = query.where(Track.energy <= max_energy)
        if min_danceability is not None: query = query.where(Track.danceability >= min_danceability)
        if max_danceability is not None: query = query.where(Track.danceability <= max_danceability)
        if min_brightness is not None: query = query.where(Track.brightness >= min_brightness)
        if max_brightness is not None: query = query.where(Track.brightness <= max_brightness)
        if min_duration is not None: query = query.where(Track.duration >= min_duration)
        if max_duration is not None: query = query.where(Track.duration <= max_duration)
            
        return query

    def search_tracks(
        self,
        status: str = "all",
        q: Optional[str] = None,
        title: Optional[str] = None,
        artist: Optional[str] = None,
        album: Optional[str] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
        key: Optional[str] = None,
        bpm: Optional[float] = None,
        bpm_range: float = 5.0,
        min_duration: Optional[float] = None,
        max_duration: Optional[float] = None,
        min_energy: Optional[float] = None,
        max_energy: Optional[float] = None,
        min_danceability: Optional[float] = None,
        max_danceability: Optional[float] = None,
        min_brightness: Optional[float] = None,
        max_brightness: Optional[float] = None,
        min_year: Optional[int] = None,
        max_year: Optional[int] = None,
        year_status: str = "all",
        lyrics_status: str = "all",
        lyrics: Optional[str] = None,
        target_params: Optional[Dict[str, float]] = None,
        sort: Optional[str] = None,
        order: str = "asc",
        limit: int = 100, 
        offset: int = 0
    ) -> List[Dict[str, Any]]:
        """
        楽曲検索を実行し、歌詞情報を注入した結果を返却。
        Library画面だけでなく、各検索コンポーネントで一貫した情報を表示させます。
        """
        
        # 歌詞カラム取得のために outerjoin する
        query = select(Track, Lyrics.content).outerjoin(Lyrics, Track.id == Lyrics.track_id)
        
        query = self._apply_search_conditions(
            query=query,
            status=status,
            q=q,
            title=title,
            artist=artist,
            album=album,
            genres=genres,
            subgenres=subgenres,
            key=key,
            bpm=bpm,
            bpm_range=bpm_range,
            min_duration=min_duration,
            max_duration=max_duration,
            min_energy=min_energy,
            max_energy=max_energy,
            min_danceability=min_danceability,
            max_danceability=max_danceability,
            min_brightness=min_brightness,
            max_brightness=max_brightness,
            min_year=min_year,
            max_year=max_year,
            year_status=year_status,
            lyrics_status=lyrics_status,
            lyrics=lyrics,
            target_params=target_params,
            already_joined_lyrics=True
        )
        
        # Always include a unique tiebreaker so page boundaries cannot drift when
        # many imports share the same created_at or target distance.
        query = apply_sort(query, sort, order).offset(offset).limit(limit)
        results = self.session.exec(query).all()
        
        final_tracks = []
        for track, lyrics_content in results:
            track_data = track.model_dump()
            track_data["lyrics"] = lyrics_content
            track_data["has_lyrics"] = bool(lyrics_content and lyrics_content.strip())
            final_tracks.append(track_data)
            
        return final_tracks

    def search_tracks_page(self, **filters) -> Dict[str, Any]:
        """Return one bounded result page and an exact count for the same filters."""
        limit = int(filters.pop("limit", 100))
        offset = int(filters.pop("offset", 0))
        sort = filters.pop("sort", None)
        order = filters.pop("order", "asc")
        tracks = self.search_tracks(limit=limit, offset=offset, sort=sort, order=order, **filters)

        count_query = select(Track.id)
        count_query = self._apply_search_conditions(
            query=count_query,
            already_joined_lyrics=False,
            **filters,
        ).order_by(None)
        total = self.session.exec(
            select(func.count()).select_from(count_query.subquery())
        ).one()
        return {
            "items": tracks,
            "total": int(total),
            "limit": limit,
            "offset": offset,
            "has_more": offset + len(tracks) < int(total),
        }

    def search_track_ids(
        self,
        status: str = "all",
        q: Optional[str] = None,
        title: Optional[str] = None,
        artist: Optional[str] = None,
        album: Optional[str] = None,
        genres: Optional[List[str]] = None,
        subgenres: Optional[List[str]] = None,
        key: Optional[str] = None,
        bpm: Optional[float] = None,
        bpm_range: float = 5.0,
        min_duration: Optional[float] = None,
        max_duration: Optional[float] = None,
        min_energy: Optional[float] = None,
        max_energy: Optional[float] = None,
        min_danceability: Optional[float] = None,
        max_danceability: Optional[float] = None,
        min_brightness: Optional[float] = None,
        max_brightness: Optional[float] = None,
        min_year: Optional[int] = None,
        max_year: Optional[int] = None,
        year_status: str = "all",
        lyrics_status: str = "all",
        lyrics: Optional[str] = None,
        target_params: Optional[Dict[str, float]] = None
    ) -> List[int]:
        """IDのみのリストを返す（一括操作用）"""
        
        query = select(Track.id)
        
        query = self._apply_search_conditions(
            query=query,
            status=status,
            q=q,
            title=title,
            artist=artist,
            album=album,
            genres=genres,
            subgenres=subgenres,
            key=key,
            bpm=bpm,
            bpm_range=bpm_range,
            min_duration=min_duration,
            max_duration=max_duration,
            min_energy=min_energy,
            max_energy=max_energy,
            min_danceability=min_danceability,
            max_danceability=max_danceability,
            min_brightness=min_brightness,
            max_brightness=max_brightness,
            min_year=min_year,
            max_year=max_year,
            year_status=year_status,
            lyrics_status=lyrics_status,
            lyrics=lyrics,
            target_params=target_params,
            already_joined_lyrics=False
        )
        
        return self.session.exec(query).all()
