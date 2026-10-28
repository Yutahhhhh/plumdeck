import os
import json
import math
import unicodedata
from typing import List, Dict, Any
from sqlmodel import Session, select
from models import Track, TrackAnalysis
import infra.database.connection as db_connection
from utils.filesystem import resolve_path
from utils.metadata import check_metadata_changed
from domain.constants import SUPPORTED_EXTENSIONS
from domain.constants import EMBEDDING_DIM


def has_valid_embedding(embedding: Any) -> bool:
    """Reject missing, malformed, non-finite and placeholder embeddings."""
    value = getattr(embedding, "embedding_json", embedding)
    try:
        vector = json.loads(value) if isinstance(value, str) else list(value)
        return (
            len(vector) == EMBEDDING_DIM
            and all(
                isinstance(item, (int, float))
                and not isinstance(item, bool)
                and math.isfinite(item)
                for item in vector
            )
            and any(item != 0 for item in vector)
        )
    except (TypeError, ValueError, json.JSONDecodeError):
        return False


def _has_audio_analysis_core(track: Any) -> bool:
    """Validate audio-derived fields without conflating missing ID3 metadata."""
    return bool(
        track
        and isinstance(getattr(track, "bpm", None), (int, float))
        and not isinstance(track.bpm, bool)
        and math.isfinite(track.bpm)
        and track.bpm > 0
        and isinstance(getattr(track, "duration", None), (int, float))
        and not isinstance(track.duration, bool)
        and math.isfinite(track.duration)
        and track.duration > 0
    )


TIMBRE_FIELDS = ("energy", "danceability", "brightness", "noisiness")


def _is_unit(value: Any) -> bool:
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and math.isfinite(value) and 0 <= value <= 1)


def _has_timbre(track: Any) -> bool:
    """All four descriptors are measured, not left at the column default 0.0.

    One descriptor can round to 0.00 in real audio (brightness does), so a
    track counts as unmeasured only when three or more are exactly 0.0.
    """
    values = [track.get(field) if isinstance(track, dict) else getattr(track, field, None)
              for field in TIMBRE_FIELDS]
    return all(_is_unit(value) for value in values) and sum(value == 0 for value in values) < 3


def _has_waveform(analysis: Any) -> bool:
    return bool(getattr(analysis, "waveform_u8", None))


def _current_light_analysis(analysis: Any) -> bool:
    extra = getattr(analysis, "features_extra", {})
    components = extra.get("analysis_components", {}) if isinstance(extra, dict) else {}
    return bool(isinstance(components, dict)
                and components.get("rhythm") == "numpy-attacks-v2"
                and components.get("timbre") == "bounded-summary-v2"
                and getattr(analysis, "beats_f32", None))


def has_full_analysis(track: Any, embedding: Any = None, analysis: Any = None) -> bool:
    """A detailed result is reusable even while optional embeddings are pending."""
    return (has_completed_analysis(track, embedding, analysis)
            and getattr(track, "analysis_level", None) != "light")


def has_completed_analysis(track: Any, embedding: Any = None, analysis: Any = None) -> bool:
    """The one "analyzed" rule for Dashboard, Library, Explorer and Play.

    duration > 0, bpm > 0, measured timbre and a stored waveform. Light rows
    also need the whole-track grid of the current light profile; older
    excerpt-only light rows are upgraded on re-import. ``embedding`` is kept
    for callers: similarity vectors are optional and filled in the background.
    """
    return bool(_has_audio_analysis_core(track) and _has_timbre(track) and _has_waveform(analysis)
                and (getattr(track, "analysis_level", None) != "light"
                     or _current_light_analysis(analysis)))


def completed_analysis_condition():
    """SQL form of has_completed_analysis for Track queries.

    Only light rows read features_extra_json; the CASE keeps that JSON work
    off the detailed rows that make up most libraries.
    """
    from sqlalchemy import and_, case, func, select as sql_select
    values = [getattr(Track, field) for field in TIMBRE_FIELDS]
    raw = case((func.json_valid(TrackAnalysis.features_extra_json), TrackAnalysis.features_extra_json))
    current_light = and_(
        func.json_extract_string(raw, "$.analysis_components.rhythm") == "numpy-attacks-v2",
        func.json_extract_string(raw, "$.analysis_components.timbre") == "bounded-summary-v2",
        func.octet_length(TrackAnalysis.beats_f32) > 0,
    )
    stored = sql_select(TrackAnalysis.track_id).where(
        TrackAnalysis.track_id == Track.id,
        func.octet_length(TrackAnalysis.waveform_u8) > 0,
        case((Track.analysis_level == "light", func.coalesce(current_light, False)), else_=True),
    ).exists()
    zeros = sum(case((value == 0, 1), else_=0) for value in values)
    valid = [Track.bpm > 0, func.isfinite(Track.bpm), Track.duration > 0, func.isfinite(Track.duration),
             *(and_(value >= 0, value <= 1, func.isfinite(value)) for value in values),
             zeros < 3, stored]
    return func.coalesce(and_(*valid), False)


def analyzed_track_ids(session: Session, track_ids: List[int]) -> set:
    """Ids among ``track_ids`` that meet the analyzed rule (one small query)."""
    ids = [track_id for track_id in track_ids if track_id is not None]
    if not ids:
        return set()
    return set(session.exec(select(Track.id).where(Track.id.in_(ids), completed_analysis_condition())).all())


def has_completed_analysis_for_profile(track: Any, embedding: Any, profile: str, analysis: Any = None) -> bool:
    if profile == "full":
        return has_full_analysis(track, embedding, analysis)
    return has_completed_analysis(track, embedding, analysis)


def has_completed_analysis_result(result: Dict[str, Any], existing_embedding: Any = None) -> bool:
    """Validate a fresh foreground result without requiring similarity work."""
    bpm = result.get("bpm")
    duration = result.get("duration")
    return bool(
        isinstance(bpm, (int, float)) and not isinstance(bpm, bool) and math.isfinite(bpm) and bpm > 0
        and isinstance(duration, (int, float)) and not isinstance(duration, bool) and math.isfinite(duration) and duration > 0
        and _has_timbre(result) and _has_result_waveform(result)
        and (result.get("analysis_level") != "light" or _has_light_playback_data(result))
    )


def _has_result_waveform(result: Dict[str, Any]) -> bool:
    extra = result.get("features_extra") or {}
    peaks = extra.get("waveform_peaks") if isinstance(extra, dict) else None
    # Decoded amplitude may exceed 1.0 on clipped masters; storage clamps it.
    return bool(isinstance(peaks, list) and peaks and all(
        isinstance(peak, (int, float)) and not isinstance(peak, bool) and math.isfinite(peak) and peak >= 0
        for peak in peaks))


def _has_light_playback_data(result: Dict[str, Any]) -> bool:
    from domain.services.analysis.beat_grid import playback_grid
    extra = result.get("features_extra") or {}
    if not isinstance(extra, dict):
        return False
    try:
        ticks = extra.get("beat_positions", [])
        peaks = extra.get("waveform_peaks", [])
        return bool(
            playback_grid(ticks, result["bpm"])
            and ticks[-1] < result["duration"]
            and peaks and all(isinstance(p, (int, float)) and math.isfinite(p) and 0 <= p <= 1 for p in peaks)
        )
    except (ValueError, TypeError, KeyError, IndexError):
        return False

def normalize_path(path: str) -> str:
    """
    MacOS(NFD)とDB/Linux(NFC)のパスの差異を吸収するため、
    一貫してNFCに正規化して比較を行う。
    """
    return unicodedata.normalize('NFC', path)

def _collect_files_from_directory(directory: str) -> List[str]:
    files_list = []
    for root, _, files in os.walk(directory):
        for file in files:
            if file.lower().endswith(SUPPORTED_EXTENSIONS):
                files_list.append(os.path.join(root, file))
    return files_list

def expand_targets(targets: List[str]) -> List[str]:
    all_files = []
    for target in targets:
        resolved_target = resolve_path(target)
        if not resolved_target:
            continue
        if os.path.isfile(resolved_target):
            if resolved_target.lower().endswith(SUPPORTED_EXTENSIONS):
                all_files.append(resolved_target)
        elif os.path.isdir(resolved_target):
            files = _collect_files_from_directory(resolved_target)
            all_files.extend(files)
    return all_files

def filter_and_prioritize_files(targets: List[str], force_update: bool,
                                analysis_profile: str = "auto",
                                skipped_db_paths: List[str] | None = None) -> tuple[List[str], int]:
    """
    ターゲットファイルリストを展開し、DBの状態に基づいてフィルタリングと優先順位付けを行う。
    """
    all_files = expand_targets(targets)
    
    track_map = {}
    completed_ids = set()
    lyrics_map = {}
    
    # 読み取り専用セッション。DuckDBの並列読み取りを阻害しないよう短く閉じる。
    with Session(db_connection.engine) as session:
        # パスをNFCで正規化してマップを作成
        existing_tracks_query = session.exec(select(Track)).all()
        track_map = {normalize_path(t.filepath): t for t in existing_tracks_query}
        
        # Project IDs only: never load every waveform/grid BLOB to filter imports.
        completed_ids = set(session.exec(select(Track.id).where(completed_analysis_condition())).all())
        
        # Lyricsの存在確認
        from domain.models.lyrics import Lyrics
        existing_lyrics = session.exec(select(Lyrics)).all()
        lyrics_map = {ly.track_id: True for ly in existing_lyrics}
    
    files_to_process = []
    skipped_count = 0
    
    for fp in all_files:
        norm_fp = normalize_path(fp)
        should_skip = False
        
        if not force_update:
            existing_track = track_map.get(norm_fp)
            if existing_track:
                # .lrcファイルが存在するか（内容の差分チェックは後段で行う）
                lrc_path = os.path.splitext(fp)[0] + ".lrc"
                has_lrc_file = os.path.exists(lrc_path)
                
                if (existing_track.id in completed_ids and (analysis_profile != "full" or existing_track.analysis_level != "light")):
                    # .lrcファイルがある場合、内容が変更されている可能性があるため処理する
                    # （実際の差分チェックはingestion_domain_serviceで行われる）
                    if has_lrc_file:
                        should_skip = False
                    # ファイルタグ自体に変更がないか
                    elif not check_metadata_changed(fp, existing_track):
                        should_skip = True
        
        if should_skip:
            skipped_count += 1
            if skipped_db_paths is not None:
                skipped_db_paths.append(existing_track.filepath)
        else:
            files_to_process.append(fp)
    
    # 優先順位付け: 解析が不完全なものを先に処理する
    def get_priority(filepath):
        norm_fp = normalize_path(filepath)
        track = track_map.get(norm_fp)
        if track:
            if not (track.id in completed_ids and (analysis_profile != "full" or track.analysis_level != "light")):
                return 0 # 高優先
            return 1 # メタデータ更新のみ等
        return 1 # 完全な新規ファイル

    files_to_process.sort(key=get_priority)
    
    return files_to_process, skipped_count
