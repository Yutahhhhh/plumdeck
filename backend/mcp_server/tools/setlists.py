import json
from typing import Any, Dict, List, Optional

from mcp_server.instance import mcp, db_session, serialize, track_list_payload
from app.services.setlist_app_service import SetlistAppService


@mcp.tool()
def list_setlists() -> Dict[str, Any]:
    """全セットリストの一覧を返す。"""
    with db_session() as session:
        service = SetlistAppService(session)
        return {"setlists": serialize(service.get_setlists())}


@mcp.tool()
def create_setlist(name: str) -> Dict[str, Any]:
    """新しい空のセットリストを作成する。"""
    with db_session() as session:
        service = SetlistAppService(session)
        return serialize(service.create_setlist(name))


@mcp.tool()
def rename_setlist(setlist_id: int, name: str) -> Dict[str, Any]:
    """セットリストの名前を変更する。"""
    with db_session() as session:
        service = SetlistAppService(session)
        result = service.update_setlist(setlist_id, {"name": name})
        if not result:
            raise ValueError(f"Setlist {setlist_id} not found")
        return serialize(result)


@mcp.tool()
def delete_setlist(setlist_id: int) -> Dict[str, Any]:
    """セットリストを削除する（曲データも含めて完全に削除）。"""
    with db_session() as session:
        service = SetlistAppService(session)
        ok = service.delete_setlist(setlist_id)
        if not ok:
            raise ValueError(f"Setlist {setlist_id} not found")
        return {"ok": True}

@mcp.tool()
def get_setlist_tracks(setlist_id: int) -> Dict[str, Any]:
    """セットリストに含まれる楽曲を、並び順どおりに返す。"""
    with db_session() as session:
        service = SetlistAppService(session)
        return track_list_payload(service.get_setlist_tracks(setlist_id))


@mcp.tool()
def set_setlist_tracks(
    setlist_id: int,
    track_ids: Optional[List[int]] = None,
    track_data: Optional[List[Dict[str, Any]]] = None,
) -> Dict[str, Any]:
    """曲構成を一括で置き換える。track_ids または track_data の一方を指定する。
    生成結果の承認済みワードプレイを保存する場合は、返された tracks を
    track_data に渡すと id と wordplay_json を保持する。既存の曲構成は置換される。
    """
    if (track_ids is None) == (track_data is None):
        raise ValueError("Specify exactly one of track_ids or track_data")
    entries: List[Any] = track_ids if track_ids is not None else []
    if track_data is not None:
        entries = []
        for track in track_data:
            if type(track.get("id")) is not int:
                raise ValueError("Each track_data entry requires an integer id")
            annotation = track.get("wordplay_json")
            if annotation is not None:
                if not isinstance(annotation, str) or not isinstance(json.loads(annotation), dict):
                    raise ValueError("wordplay_json must be a JSON object encoded as a string")
            entries.append({"id": track["id"], "wordplay_json": annotation})
    with db_session() as session:
        service = SetlistAppService(session)
        ok = service.update_setlist_tracks(setlist_id, entries)
        if not ok:
            raise ValueError(f"Setlist {setlist_id} not found")
        return track_list_payload(service.get_setlist_tracks(setlist_id))


@mcp.tool()
def add_track_to_setlist(setlist_id: int, track_id: int, position: Optional[int] = None) -> Dict[str, Any]:
    """セットリストに1曲追加する。position を指定しない場合は末尾に追加。"""
    with db_session() as session:
        service = SetlistAppService(session)
        if service.add_setlist_track(setlist_id, track_id, position) is None:
            raise ValueError(f"Setlist {setlist_id} not found")
        return track_list_payload(service.get_setlist_tracks(setlist_id))


@mcp.tool()
def remove_track_from_setlist(setlist_id: int, track_id: int) -> Dict[str, Any]:
    """セットリストから指定した楽曲を1件削除する（同じ曲が複数あれば最初の1件のみ）。"""
    with db_session() as session:
        service = SetlistAppService(session)
        current = service.get_setlist_tracks(setlist_id)
        entry = next((item for item in current if item["id"] == track_id), None)
        if entry:
            service.remove_setlist_track(setlist_id, int(entry["setlist_track_id"]))
        return track_list_payload(service.get_setlist_tracks(setlist_id))


@mcp.tool()
def recommend_next_track(
    track_id: int,
    limit: int = 10,
    genres: Optional[List[str]] = None,
    subgenres: Optional[List[str]] = None,
    target_bpm: Optional[float] = None,
    target_energy: Optional[float] = None,
    target_danceability: Optional[float] = None,
    target_brightness: Optional[float] = None,
    target_noisiness: Optional[float] = None,
    year_min: Optional[int] = None,
    year_max: Optional[int] = None,
) -> Dict[str, Any]:
    """指定した曲の次に繋ぐのに適した楽曲を、BPMと音色の類似度（埋め込み）からスコアリングして提案する。
    キーは採点に使わない（重み0）。埋め込みが計算待ちの曲は類似度を除き、残りの重みで採点する。
    ユーザーの自然言語の方向性はMCPクライアント側で target_* 値へ解釈する。
    target未指定なら BPM と音色の類似度だけで評価する。
    year_min/year_max は候補曲のリリース年の範囲（両端を含む）。指定時は年不明の曲を除く。
    """
    with db_session() as session:
        service = SetlistAppService(session)
        results = service.recommend_next_track(
            track_id,
            limit=limit,
            target_params={
                "bpm": target_bpm,
                "energy": target_energy,
                "danceability": target_danceability,
                "brightness": target_brightness,
                "noisiness": target_noisiness,
                "year_min": year_min,
                "year_max": year_max,
            },
            genres=genres,
            subgenres=subgenres,
        )
        return track_list_payload(results)


@mcp.tool()
def recommend_next_track_page(
    track_id: int,
    limit: int = 100,
    offset: int = 0,
    genres: Optional[List[str]] = None,
    subgenres: Optional[List[str]] = None,
    target_bpm: Optional[float] = None,
    target_energy: Optional[float] = None,
    target_danceability: Optional[float] = None,
    target_brightness: Optional[float] = None,
    target_noisiness: Optional[float] = None,
    year_min: Optional[int] = None,
    year_max: Optional[int] = None,
) -> Dict[str, Any]:
    """全候補をスコア順に並べてからページングし、正確な総件数を返す。"""
    if not 1 <= limit <= 500 or offset < 0:
        raise ValueError("limit must be 1..500 and offset must be non-negative")
    with db_session() as session:
        return serialize(SetlistAppService(session).recommend_next_track_page(
            track_id, limit=limit, offset=offset,
            target_params={
                "bpm": target_bpm, "energy": target_energy,
                "danceability": target_danceability, "brightness": target_brightness,
                "noisiness": target_noisiness, "year_min": year_min, "year_max": year_max,
            },
            genres=genres, subgenres=subgenres,
        ))


@mcp.tool()
def generate_auto_setlist(
    length: Optional[int] = None,
    min_length: Optional[int] = None,
    max_length: Optional[int] = None,
    seed_track_ids: Optional[List[int]] = None,
    genres: Optional[List[str]] = None,
    subgenres: Optional[List[str]] = None,
    target_bpm: Optional[float] = None,
    target_energy: Optional[float] = None,
    target_danceability: Optional[float] = None,
    target_brightness: Optional[float] = None,
    target_noisiness: Optional[float] = None,
    year_min: Optional[int] = None,
    year_max: Optional[int] = None,
) -> Dict[str, Any]:
    """構造化された音響特徴量ターゲットに基づき、セットリスト候補を自動生成する
    （DBには保存しない、結果一覧を返すのみ。保存するには set_setlist_tracks を別途呼ぶこと）。
    ユーザーの自然言語のvibeは、このツールを呼ぶMCPクライアント自身が target_* と
    genres/subgenres に解釈する。
    曲数は length で指定、min_length/max_length を渡すとその範囲でランダムに決定する。
    length 未指定時は UI 設定のデフォルト曲数（setlist_default_length）を使用する。
    seed_track_ids を渡すとその曲群の流れを引き継いで生成する。
    year_min/year_max は追加候補曲のリリース年の範囲（両端を含む）。指定時は年不明の曲を除く。
    明示したシード曲は指定順で優先し、年代・ジャンルの候補条件からは除外する。
    """
    with db_session() as session:
        service = SetlistAppService(session)
        results = service.generate_auto_setlist(
            target_params={
                "bpm": target_bpm,
                "energy": target_energy,
                "danceability": target_danceability,
                "brightness": target_brightness,
                "noisiness": target_noisiness,
                "year_min": year_min,
                "year_max": year_max,
            },
            limit=length,
            min_length=min_length,
            max_length=max_length,
            seed_track_ids=seed_track_ids,
            genres=genres,
            subgenres=subgenres,
        )
        return track_list_payload(results)


@mcp.tool()
def generate_bridge_setlist(
    start_track_id: int,
    end_track_id: int,
    length: int = 5,
    genres: Optional[List[str]] = None,
    subgenres: Optional[List[str]] = None,
) -> Dict[str, Any]:
    """開始曲と終了曲を指定し、その間を自然に繋ぐ中間曲を自動生成する（結果は保存されない）。length は開始・終了を含む合計曲数。"""
    with db_session() as session:
        service = SetlistAppService(session)
        results = service.generate_path_setlist(
            start_track_id, end_track_id, length, genres=genres, subgenres=subgenres
        )
        return track_list_payload(results)


@mcp.tool()
def export_setlist_m3u8(setlist_id: int) -> Dict[str, Any]:
    """セットリストを M3U8 プレイリスト形式のテキストとしてエクスポートする（Rekordbox等に読み込み可能）。
    移動/削除済みのファイルは '# MISSING:' 行として出力される。呼び出し前に validate_setlist_export での確認を推奨。
    """
    with db_session() as session:
        service = SetlistAppService(session)
        content = service.export_as_m3u8(setlist_id)
        return {"setlist_id": setlist_id, "m3u8": content}


@mcp.tool()
def validate_setlist_export(setlist_id: int) -> Dict[str, Any]:
    """エクスポート前にセットリスト内の楽曲ファイルが実際にディスク上に存在するか検証する。"""
    with db_session() as session:
        service = SetlistAppService(session)
        return serialize(service.validate_export(setlist_id))


@mcp.tool()
def add_track_to_setlist_with_wordplay(
    setlist_id: int,
    track_id: int,
    position: Optional[int] = None,
    keyword: Optional[str] = None,
    source_phrase: Optional[str] = None,
    target_phrase: Optional[str] = None,
    source_cue_mode: Optional[str] = None,
    from_timestamp: Optional[float] = None,
    source_cue_end_timestamp: Optional[float] = None,
    to_timestamp: Optional[float] = None,
    target_intro_timestamp: Optional[float] = None,
    target_landing_timestamp: Optional[float] = None,
) -> Dict[str, Any]:
    """セットリストに1曲をCue打ち・イントロ・着地位置付きで追加する。"""
    with db_session() as session:
        service = SetlistAppService(session)
        current = service.get_setlist_tracks(setlist_id)
        # 既存曲のワードプレイ情報を保持したまま受け渡す
        track_data: List[Dict[str, Any]] = [
            {"id": t["id"], "wordplay_json": t.get("wordplay_json")} for t in current
        ]

        wp: Dict[str, Any] = {}
        if keyword is not None:
            wp["keyword"] = keyword
        if source_phrase is not None:
            wp["source_phrase"] = source_phrase
        if target_phrase is not None:
            wp["target_phrase"] = target_phrase
        if source_cue_mode is not None:
            wp["source_cue_mode"] = source_cue_mode
        if from_timestamp is not None:
            wp["from_timestamp"] = from_timestamp
        if source_cue_end_timestamp is not None:
            wp["source_cue_end_timestamp"] = source_cue_end_timestamp
        if to_timestamp is not None:
            wp["to_timestamp"] = to_timestamp
        if target_intro_timestamp is not None:
            wp["target_intro_timestamp"] = target_intro_timestamp
        if target_landing_timestamp is not None:
            wp["target_landing_timestamp"] = target_landing_timestamp

        entry: Dict[str, Any] = {"id": track_id}
        if wp:
            entry["wordplay_json"] = json.dumps(wp, ensure_ascii=False)

        if position is None:
            track_data.append(entry)
        else:
            track_data.insert(max(0, min(position, len(track_data))), entry)

        service.update_setlist_tracks(setlist_id, track_data)
        return track_list_payload(service.get_setlist_tracks(setlist_id))


@mcp.tool()
def update_setlist_track_wordplay(setlist_track_id: int, wordplay: Dict[str, Any]) -> Dict[str, Any]:
    """セットリスト内の曲にCue打ち・イントロ・着地を含むワードプレイ情報を設定する。"""
    from domain.models.setlist import SetlistTrack

    with db_session() as session:
        st = session.get(SetlistTrack, setlist_track_id)
        if not st:
            raise ValueError(f"Setlist track {setlist_track_id} not found")
        st.wordplay_json = json.dumps(wordplay, ensure_ascii=False)
        session.add(st)
        session.commit()
        session.refresh(st)
        return serialize(st)


@mcp.tool()
def clear_setlist_track_wordplay(setlist_track_id: int) -> Dict[str, Any]:
    """セットリスト内の曲のワードプレイ情報をクリアする。"""
    from domain.models.setlist import SetlistTrack

    with db_session() as session:
        st = session.get(SetlistTrack, setlist_track_id)
        if not st:
            raise ValueError(f"Setlist track {setlist_track_id} not found")
        st.wordplay_json = None
        session.add(st)
        session.commit()
        return {"ok": True}
