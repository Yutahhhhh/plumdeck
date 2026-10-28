import json
import math
from typing import Dict, Any
from datetime import datetime
from sqlmodel import Session, select, text
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.models.lyrics import Lyrics
from utils.array_codec import pack_f32, pack_u8_waveform
from utils.features_extra import persisted_features_extra
from utils.ingestion import has_valid_embedding
import infra.database.connection as db_connection

class IngestionRepository:
    def __init__(self):
        pass

    def _prepare_track_models(self, session: Session, result: Dict[str, Any], update_metadata: bool = True) -> int:
        filepath = result["filepath"]
        
        track_update_data = {
            "title": result.get("title"), "artist": result.get("artist"),
            "album": result.get("album"), "genre": result.get("genre"), "year": result.get("year"),
            "bpm": result.get("bpm", 0), "key": result.get("key", ""), "scale": result.get("scale", ""),
            "duration": result.get("duration", 0), "analysis_level": result.get("analysis_level"),
            "energy": result.get("energy", 0.0),
            "danceability": result.get("danceability", 0.0), "brightness": result.get("brightness", 0.0),
            "contrast": result.get("contrast", 0.0), "noisiness": result.get("noisiness", 0.0),
            "loudness": result.get("loudness", -60.0),
            "loudness_range": float(result.get("loudness_range", 0.0)),
            "spectral_flux": float(result.get("spectral_flux", 0.0)),
            "spectral_rolloff": float(result.get("spectral_rolloff", 0.0)),
        }

        # PRAGMA foreign_keys は DuckDB で未サポートのため削除
        # 代わりに no_autoflush で ORM レベルの整合性チェックタイミングを調整
        with session.no_autoflush:
            existing_track = session.exec(select(Track).where(Track.filepath == filepath)).first()
            track_id = None
            bpm_authority = None

            if existing_track:
                track_id = existing_track.id
                existing_embedding = session.get(TrackEmbedding, track_id)
                existing_analysis = session.get(TrackAnalysis, track_id)
                existing_fx = existing_analysis.features_extra if existing_analysis else {}
                # A rekordbox-synced BPM outranks re-analysis, including forced re-imports.
                bpm_authority = existing_fx.get("bpm_authority") if isinstance(existing_fx, dict) else None
                preserve_full_analysis = bool(
                    result.get("analysis_level") == "light"
                    and existing_track.analysis_level != "light"
                    and isinstance(existing_track.bpm, (int, float))
                    and existing_track.bpm > 0
                    and has_valid_embedding(existing_embedding)
                )
                if update_metadata:
                    for k, v in track_update_data.items():
                        # Partial metadata updates must not erase absent audio features.
                        if k not in result:
                            continue
                        if preserve_full_analysis and k in {
                            "bpm", "key", "scale", "analysis_level", "energy",
                            "danceability", "brightness", "contrast", "noisiness",
                            "loudness", "loudness_range", "spectral_flux",
                            "spectral_rolloff",
                        }:
                            continue
                        if k == "bpm" and bpm_authority:
                            continue
                        if isinstance(v, str) and v and v.lower() != "unknown":
                            setattr(existing_track, k, v)
                        elif k == "year" and isinstance(v, int) and v > 0:
                            setattr(existing_track, k, v)
                        elif k == "analysis_level" and v in {"light", "full"}:
                            setattr(existing_track, k, v)
                        elif k in {
                            "bpm", "duration", "energy", "danceability", "brightness",
                            "contrast", "noisiness", "loudness", "loudness_range",
                            "spectral_flux", "spectral_rolloff",
                        } and isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v):
                            if k in {"bpm", "duration"} and v <= 0:
                                continue
                            setattr(existing_track, k, v)
            else:
                final_data = {}
                for k, v in track_update_data.items():
                    if k == "year":
                        final_data[k] = v if v is not None else None
                    else:
                        final_data[k] = v if v is not None else ""
                if not final_data.get("title"): final_data["title"] = "Unknown"
                if not final_data.get("artist"): final_data["artist"] = "Unknown"
                if not isinstance(final_data.get("bpm"), (int, float)) or final_data.get("bpm", 0) <= 0:
                    final_data["bpm"] = None
                new_track = Track(filepath=filepath, **final_data)
                session.add(new_track)
                session.flush()
                track_id = new_track.id

            # DuckDB は UPDATE で空き領域を OS に返さないため、値が実際に変わった時だけ
            # 行を書き換える (再解析/一括処理での無駄な UPDATE によるファイル肥大化を防ぐ)。
            extras = result.get("features_extra", {})
            analysis = session.get(TrackAnalysis, track_id)
            analysis_changed = analysis is None
            if analysis is None:
                analysis = TrackAnalysis(track_id=track_id)

            if extras and not (existing_track and preserve_full_analysis):
                # Persist an allow-listed copy; callers still validate the full result.
                fx = persisted_features_extra(extras)
                if bpm_authority:
                    fx["bpm_authority"] = bpm_authority
                    if "analysis_bpm" in existing_fx:
                        fx["analysis_bpm"] = existing_fx["analysis_bpm"]
                    bpm = result.get("bpm")
                    if isinstance(bpm, (int, float)) and not isinstance(bpm, bool) and math.isfinite(bpm) and bpm > 0:
                        fx["analysis_bpm"] = bpm
                new_fx = json.dumps(fx)
                if new_fx != (analysis.features_extra_json or "{}"):
                    analysis.features_extra_json = new_fx
                    analysis_changed = True
            if extras.get("waveform_peaks") and not (existing_track and preserve_full_analysis):
                new_wave = pack_u8_waveform(extras["waveform_peaks"])
                if new_wave != analysis.waveform_u8:
                    analysis.waveform_u8 = new_wave
                    analysis_changed = True
            if extras.get("beat_positions") and not (existing_track and preserve_full_analysis):
                new_beats = pack_f32(extras["beat_positions"])
                if new_beats != analysis.beats_f32:
                    analysis.beats_f32 = new_beats
                    analysis_changed = True
            if analysis_changed:
                session.add(analysis)
                if extras.get("beat_positions") and not (existing_track and preserve_full_analysis):
                    # A cached automatic grid must not mask freshly analyzed
                    # beats. Saved user cues/loops/grids live in a separate row.
                    session.exec(text("DELETE FROM track_grid_candidates WHERE track_id=:id AND source='analysis'"),
                                 params={"id": track_id})
                    from infra.repositories.analysis_grid_refresh import refresh_saved_analysis_grid
                    refresh_saved_analysis_grid(session, track_id, extras.get("playback_grid"))

            if "embedding" in result and result["embedding"] and not (existing_track and preserve_full_analysis):
                emb = session.get(TrackEmbedding, track_id)
                new_emb_json = json.dumps(result["embedding"])
                new_model = result.get("embedding_model") or (emb.model_name if emb else "musicnn")
                if emb is None:
                    emb = TrackEmbedding(track_id=track_id, embedding_json=new_emb_json, model_name=new_model)
                    emb.updated_at = datetime.now()
                    session.add(emb)
                elif emb.embedding_json != new_emb_json or emb.model_name != new_model:
                    emb.embedding_json = new_emb_json
                    emb.model_name = new_model
                    emb.updated_at = datetime.now()
                    session.add(emb)

            if "lyrics" in result and result["lyrics"]:
                ly = session.get(Lyrics, track_id) or Lyrics(track_id=track_id)
                if result["lyrics"].strip():
                    ly.content = result["lyrics"]
                    ly.updated_at = datetime.now()
                    session.add(ly)
            return int(track_id)

    def save_track(self, result: Dict[str, Any], update_metadata: bool = True):
        # Callers use success to advance the progress counter. Propagate a DB
        # failure so a track cannot be reported as analyzed without being saved.
        with Session(db_connection.engine) as session:
            track_id = self._prepare_track_models(session, result, update_metadata)
            session.commit()
            return track_id

    def save_track_result(self, session: Session, result: Dict[str, Any], update_metadata: bool = True) -> Dict[str, Any]:
        """Structured writer used by persistent imports; commit failures propagate."""
        existing = session.exec(select(Track).where(Track.filepath == result["filepath"])).first()
        track_id = self._prepare_track_models(session, result, update_metadata)
        session.commit()
        return {"status": "updated" if existing else "created", "track_id": track_id}
