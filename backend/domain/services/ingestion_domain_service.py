import os
import asyncio
from typing import List, Dict, Any, Optional
from concurrent.futures import Executor
from unittest.mock import MagicMock
from sqlmodel import Session, select
from tinytag import TinyTag
from ingest import analyze_track_file
from domain.constants import SUPPORTED_EXTENSIONS
from utils.metadata import (
    extract_metadata_smart, check_metadata_changed, update_file_metadata,
    extract_full_metadata, has_valid_metadata,
)
from utils.ingestion import (
    has_completed_analysis_for_profile,
    has_completed_analysis_result,
)
import infra.database.connection as db_connection
from domain.models.track import Track, TrackEmbedding, TrackAnalysis
from domain.models.lyrics import Lyrics
from domain.services import value_sources
from infra.repositories.ingestion_repository import IngestionRepository

class IngestionDomainService:
    def __init__(self):
        self.repository = IngestionRepository()

    def _process_metadata_update(self, filepath: str, existing_data_cache: Dict[str, Any], lyrics_from_file: Optional[str] = None) -> Optional[Dict[str, Any]]:
        """ファイルのメタデータタグ情報のみを更新する高速パス。"""
        filename = os.path.basename(filepath)
        try:
            # モック環境での安全性を考慮し、タグ取得をガード
            try:
                full_meta = extract_full_metadata(filepath)
            except:
                full_meta = {}
            
            if existing_data_cache.get("genre") and str(existing_data_cache["genre"]).lower() != "unknown":
                new_genre = existing_data_cache["genre"]
            else:
                new_genre = full_meta.get("genre") or "Unknown"

            # 歌詞優先順位: .lrcファイル > DB既存
            final_lyrics = lyrics_from_file if lyrics_from_file else existing_data_cache.get("lyrics")

            result = {
                "filepath": filepath,
                **existing_data_cache,
                "title": full_meta.get("title") or os.path.splitext(filename)[0],
                "artist": full_meta.get("artist") or "Unknown",
                "album": full_meta.get("album") or "Unknown",
                "genre": new_genre,
                "year": existing_data_cache.get("year"),
                "features_extra": {},
                "lyrics": final_lyrics
            }
            return result
        except Exception as e:
            print(f"ERROR: Fast metadata update failed for {filename}: {e}")
            return None

    @staticmethod
    def _fill_from_rekordbox(result: Dict[str, Any], rekordbox, skipped: List[str]) -> None:
        """Put the rekordbox values of skipped components into the result."""
        sources = {}
        if "rhythm" in skipped:
            result["bpm"] = rekordbox.bpm
            sources["bpm"] = value_sources.REKORDBOX
        if "key" in skipped:
            result["key"] = rekordbox.key
            result["scale"] = value_sources.scale_of(rekordbox.key)
            sources["key"] = value_sources.REKORDBOX
        extra = result.get("features_extra")
        if not isinstance(extra, dict):
            extra = result["features_extra"] = {}
        components = extra.get("analysis_components")
        components = dict(components) if isinstance(components, dict) else {}
        components["sources"] = {**(components.get("sources") or {}), **sources}
        extra["analysis_components"] = components

    async def process_track_ingestion(
        self,
        filepath: str, 
        force_update: bool,
        loop: asyncio.AbstractEventLoop,
        executor: Optional[Executor] = None,
        timeout: float = 300.0,
        db_lock: Optional[asyncio.Lock] = None,
        save_to_db: bool = True,
        write_source_metadata: bool = True,
        analysis_profile: str = "full",
        on_progress=None,
    ) -> Optional[Dict[str, Any]]:
        """1曲のインポート処理のメインロジック。"""
        filename = os.path.basename(filepath)
        if on_progress:
            on_progress({"stage": "checking", "label": "登録済み情報・音源の更新を確認しています"})
        lyrics_content = None

        lrc_path = os.path.splitext(filepath)[0] + ".lrc"
        if os.path.exists(lrc_path):
            try:
                with open(lrc_path, 'r', encoding='utf-8') as f:
                    lyrics_content = f.read()
                print(f"DEBUG: Found .lrc file for {filename}, content length: {len(lyrics_content) if lyrics_content else 0}", flush=True)
                if lyrics_content and write_source_metadata:
                    await loop.run_in_executor(None, update_file_metadata, filepath, lyrics_content)
            except Exception as e:
                print(f"WARNING: Failed to import .lrc file for {filename}: {e}", flush=True)
        
        skip_basic = False
        skip_waveform = False
        metadata_update_only = False
        existing_data_cache = {}
        existing_embedding = None
        rekordbox_keys = value_sources.KEY_SOURCE_DEFAULT

        try:
            with Session(db_connection.engine) as session:
                rekordbox_keys = value_sources.key_source_enabled(session)
                track = session.exec(select(Track).where(Track.filepath == filepath)).first()
                if track:
                    # Read fallback values even for forced/incomplete imports.
                    # A failed metadata probe must not erase already curated data.
                    lyrics_from_db = None
                    if not isinstance(track, MagicMock):
                        lyrics_obj = session.get(Lyrics, track.id)
                        if lyrics_obj and hasattr(lyrics_obj, 'content') and not isinstance(lyrics_obj.content, MagicMock):
                            lyrics_from_db = lyrics_obj.content

                    embedding = session.get(TrackEmbedding, track.id)
                    existing_embedding = embedding
                    existing_data_cache = {
                        "title": track.title if hasattr(track, 'title') else "Unknown",
                        "artist": track.artist if hasattr(track, 'artist') else "Unknown",
                        "album": track.album if hasattr(track, 'album') else "Unknown",
                        "bpm": track.bpm if hasattr(track, 'bpm') else 0,
                        "key": track.key if hasattr(track, 'key') else "",
                        "scale": track.scale if hasattr(track, 'scale') else "",
                        "energy": track.energy if hasattr(track, 'energy') else 0.0,
                        "duration": track.duration if hasattr(track, 'duration') else 0.0,
                        "genre": track.genre if hasattr(track, 'genre') else "Unknown",
                        "year": track.year if hasattr(track, 'year') else None,
                        "lyrics": lyrics_from_db,
                    }

                    if not force_update:
                        is_metadata_incomplete = not has_valid_metadata(track)
                        if not has_completed_analysis_for_profile(track, embedding, analysis_profile, session.get(TrackAnalysis, track.id)):
                            if is_metadata_incomplete:
                                try:
                                    tag_check = TinyTag.get(filepath)
                                    meta_check = extract_metadata_smart(filepath, tag_check)
                                    if meta_check["artist"] != "Unknown" and meta_check["title"] != "Unknown":
                                        existing_data_cache.update(meta_check)
                                except Exception:
                                    pass
                            # An incomplete track is analyzed in full; the
                            # waveform is part of the analyzed rule.
                            skip_basic = False
                        elif is_metadata_incomplete or check_metadata_changed(filepath, track):
                            metadata_update_only = True
                        else:
                            # 完全に同一だが歌詞だけ新しく見つかった場合
                            if lyrics_content and lyrics_content != existing_data_cache.get("lyrics"):
                                print(f"DEBUG: Lyrics updated for {filename} (existing: {bool(existing_data_cache.get('lyrics'))}, new: {len(lyrics_content)} chars)", flush=True)
                                existing_data_cache["lyrics"] = lyrics_content
                                result = {**existing_data_cache, "filepath": filepath}
                                if save_to_db:
                                    await loop.run_in_executor(None, self.repository.save_track, result, True)
                                return result
                            print(f"DEBUG: Track {filename} skipped - no changes (lyrics_content: {bool(lyrics_content)}, existing: {bool(existing_data_cache.get('lyrics'))})", flush=True)
                            return None
        except Exception as e:
            print(f"WARNING: DB check failed for {filename}: {e}")

        if metadata_update_only:
            result = await loop.run_in_executor(None, self._process_metadata_update, filepath, existing_data_cache, lyrics_content)
            if not result:
                raise RuntimeError(f"Metadata update failed for {filename}")
            if result and save_to_db:
                if db_lock:
                    async with db_lock:
                        await loop.run_in_executor(None, self.repository.save_track, result, True)
                else:
                    await loop.run_in_executor(None, self.repository.save_track, result, True)
            return result

        rekordbox = value_sources.RekordboxValues()
        skip_components = []
        if analysis_profile == "full":
            # Registered tracks take BPM (and key) from rekordbox, so the worker
            # skips those components. The light path keeps its bounded grid.
            rekordbox = value_sources.lookup(filepath, keys=rekordbox_keys)
            skip_components = [name for name, value in (("rhythm", rekordbox.bpm), ("key", rekordbox.key)) if value]

        try:
            if analysis_profile not in {"light", "full"}:
                raise ValueError(f"Unknown analysis profile: {analysis_profile}")
            # Positional args are required by AnalysisExecutor's isolated worker.
            # Keep the established full-profile arity for integrations and
            # tests that wrap the legacy function.
            run_args = (filepath, force_update, skip_basic, skip_waveform)
            if lyrics_content is not None or analysis_profile == "light" or skip_components:
                run_args += (lyrics_content,)
            if analysis_profile == "light" or skip_components:
                run_args += (analysis_profile,)
            if skip_components:
                run_args += (skip_components,)

            if on_progress and hasattr(executor, "submit_with_progress"):
                future = asyncio.wrap_future(executor.submit_with_progress(
                    analyze_track_file, *run_args, on_progress=on_progress))
            else:
                future = loop.run_in_executor(executor, analyze_track_file, *run_args)
            result = await asyncio.wait_for(future, timeout=timeout)
        except Exception as exc:
            raise RuntimeError(f"Audio analysis failed for {filename}: {exc}") from exc

        # AudioAnalyzer reports decoder/native failures as no result. This is
        # different from the intentional unchanged-track return above and must
        # remain retryable instead of being counted as skipped.
        if not result:
            raise RuntimeError(f"Audio analysis produced no result for {filename}")
        if result:
            if skip_components:
                self._fill_from_rekordbox(result, rekordbox, skip_components)
            # 外部歌詞の反映
            if lyrics_content:
                result["lyrics"] = lyrics_content
            # 解析結果の埋め込み歌詞を保持
            elif result.get("lyrics"):
                # analyze_track_file から返された歌詞をそのまま使用
                pass
            # 解析結果になくても、キャッシュにある場合は保持
            elif existing_data_cache.get("lyrics"):
                result["lyrics"] = existing_data_cache["lyrics"]
            
            missing_artist = not result.get("artist") or str(result.get("artist")).lower() == "unknown"
            if missing_artist or not result.get("title") or str(result.get("title")).lower() == "unknown":
                meta_smart = extract_metadata_smart(filepath)
                smart_artist = meta_smart.get("artist")
                if missing_artist and isinstance(smart_artist, str) and smart_artist.strip():
                    result["artist"] = smart_artist
                missing_title = not result.get("title") or str(result.get("title")).lower() == "unknown"
                filename_title = result.get("title") in {os.path.basename(filepath), os.path.splitext(filename)[0]}
                smart_title = meta_smart.get("title")
                smart_has_artist = isinstance(smart_artist, str) and smart_artist.lower() != "unknown"
                if (missing_title or filename_title and smart_has_artist) and isinstance(smart_title, str) and smart_title.strip():
                    result["title"] = smart_title
                elif filename_title and existing_data_cache.get("title"):
                    result["title"] = existing_data_cache["title"]

            for key in ("title", "artist"):
                value = result.get(key)
                cached = existing_data_cache.get(key)
                if (not value or str(value).lower() == "unknown") and cached and str(cached).lower() != "unknown":
                    result[key] = cached
            duration = result.get("duration")
            cached_duration = existing_data_cache.get("duration")
            if (not isinstance(duration, (int, float)) or isinstance(duration, bool) or duration <= 0):
                if isinstance(cached_duration, (int, float)) and not isinstance(cached_duration, bool) and cached_duration > 0:
                    result["duration"] = cached_duration

            if skip_basic and existing_data_cache:
                for key, db_val in existing_data_cache.items():
                    if key != "lyrics" and db_val is not None and db_val != "" and db_val != 0:
                        result[key] = db_val

            if not has_completed_analysis_result(result, existing_embedding):
                requirement = "BPM, duration, timbre or playback data"
                raise RuntimeError(
                    f"Audio analysis returned incomplete {requirement} for {filename}"
                )
            
            if save_to_db:
                if on_progress:
                    on_progress({"stage": "saving", "label": "解析結果をライブラリに保存しています"})
                if db_lock:
                    async with db_lock:
                        await loop.run_in_executor(None, self.repository.save_track, result, True)
                else:
                    await loop.run_in_executor(None, self.repository.save_track, result, True)
            return result
        raise RuntimeError(f"Audio analysis produced no result for {filename}")
