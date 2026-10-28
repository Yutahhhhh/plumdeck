import json
import math
from typing import Dict, Any
from datetime import datetime
from sqlmodel import Session, select, text
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.models.lyrics import Lyrics
from utils.array_codec import pack_f32, pack_u8_waveform
from domain.services import value_sources
from utils.features_extra import persisted_features_extra
from utils.ingestion import TIMBRE_FIELDS, has_measured_timbre, is_protected_full_row
import infra.database.connection as db_connection

# A re-analysed duration further than this from the stored one means the file
# now holds different audio.
AUDIO_CHANGE_SECONDS = 1.0

class IngestionRepository:
    def __init__(self):
        pass

    def _prepare_track_models(self, session: Session, result: Dict[str, Any], update_metadata: bool = True) -> int:
        filepath = result["filepath"]
        extras = result.get("features_extra", {})
        sources: Dict[str, str] = {}
        own_bpm = None
        # PRAGMA foreign_keys は DuckDB で未サポートのため削除
        # 代わりに no_autoflush で ORM レベルの整合性チェックタイミングを調整
        with session.no_autoflush:
            existing_track = session.exec(select(Track).where(Track.filepath == filepath)).first()
            existing_analysis = session.get(TrackAnalysis, existing_track.id) if existing_track else None
            existing_fx = existing_analysis.features_extra if existing_analysis else {}
            preserve_full_analysis = bool(
                result.get("analysis_level") == "light" and is_protected_full_row(existing_track))
            # A protected row only takes the light timbre when its own is missing.
            repair_timbre = bool(preserve_full_analysis
                                 and not has_measured_timbre(existing_track, existing_fx))
            audio_changed = self._audio_changed(existing_track, result)
            # Only analysis results resolve BPM/key; tag and lyrics updates keep them.
            if isinstance(extras, dict) and "analysis_components" in extras and not preserve_full_analysis:
                result, sources, own_bpm = self._settle_values(session, result, existing_track, existing_fx)

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

        with session.no_autoflush:
            track_id = None

            if existing_track:
                track_id = existing_track.id
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
                        } and not (repair_timbre and k in TIMBRE_FIELDS):
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
            analysis = session.get(TrackAnalysis, track_id)
            analysis_changed = analysis is None
            if analysis is None:
                analysis = TrackAnalysis(track_id=track_id)

            if extras and not (existing_track and preserve_full_analysis):
                # Persist an allow-listed copy; callers still validate the full result.
                fx = persisted_features_extra(extras)
                recorded = {**value_sources.stored_sources(existing_fx), **sources}
                if recorded:
                    fx = value_sources.with_sources(fx, recorded)
                if existing_track and not audio_changed:
                    fx = self._keep_same_audio_components(fx, existing_fx, extras)
                if own_bpm is not None:
                    fx["analysis_bpm"] = own_bpm
                elif recorded.get("bpm") not in (None, value_sources.OWN) and "analysis_bpm" in existing_fx:
                    fx["analysis_bpm"] = existing_fx["analysis_bpm"]
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
            if existing_track and audio_changed and not extras.get("beat_positions") and analysis.beats_f32:
                # The old beats belong to the previous audio; the deck grid is
                # recomputed on demand instead of pairing them with a new BPM.
                analysis.beats_f32 = None
                analysis_changed = True
            if analysis_changed:
                session.add(analysis)
                if (existing_track and audio_changed) or (
                        extras.get("beat_positions") and not (existing_track and preserve_full_analysis)):
                    # A cached automatic grid must not mask freshly analyzed
                    # beats. Saved user cues/loops/grids live in a separate row.
                    session.exec(text("DELETE FROM track_grid_candidates WHERE track_id=:id AND source='analysis'"),
                                 params={"id": track_id})
                    from infra.repositories.analysis_grid_refresh import refresh_saved_analysis_grid
                    refresh_saved_analysis_grid(session, track_id, extras.get("playback_grid"))

            # A vector of replaced audio must not be compared with the new file.
            # Re-analysing the same audio keeps it (embeddings are optional and
            # filled in the background, so a deleted one may stay missing).
            if (existing_track and audio_changed and not preserve_full_analysis and "embedding" not in result
                    and isinstance(extras, dict) and "analysis_components" in extras):
                previous_embedding = session.get(TrackEmbedding, track_id)
                if previous_embedding is not None:
                    session.delete(previous_embedding)

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

    @staticmethod
    def _audio_changed(existing_track, result: Dict[str, Any]) -> bool:
        """Whether an analysis result describes different audio than the row.

        Duration is the cheap, tag-independent signal (size and mtime change
        when tags or lyrics are written). Unknown durations count as unchanged.
        """
        if existing_track is None or "analysis_components" not in (result.get("features_extra") or {}):
            return False
        before, after = getattr(existing_track, "duration", None), result.get("duration")
        valid = all(isinstance(value, (int, float)) and not isinstance(value, bool)
                    and math.isfinite(value) and value > 0 for value in (before, after))
        return bool(valid and abs(before - after) > AUDIO_CHANGE_SECONDS)

    @staticmethod
    def _keep_same_audio_components(fx: Dict[str, Any], existing_fx: Dict[str, Any], extras) -> Dict[str, Any]:
        """Carry over what this result did not recompute for the same audio:
        the kept embedding's version and, when rhythm was skipped (rekordbox
        has the BPM), the stored own grid with its rhythm version."""
        stored = existing_fx.get("analysis_components") if isinstance(existing_fx, dict) else None
        components = fx.get("analysis_components")
        if not isinstance(stored, dict) or not isinstance(components, dict):
            return fx
        carried = ["embedding"] + ([] if extras.get("beat_positions") else ["rhythm"])
        kept = dict(components)
        for name in carried:
            if name not in kept and stored.get(name):
                kept[name] = stored[name]
        fx = {**fx, "analysis_components": kept}
        if "rhythm" in carried and kept.get("rhythm") == stored.get("rhythm"):
            for key in ("playback_grid", "playback_grid_version"):
                if key in existing_fx and key not in fx:
                    fx[key] = existing_fx[key]
        return fx

    @staticmethod
    def _settle_values(session: Session, result: Dict[str, Any], existing_track, existing_fx):
        """Apply manual > rekordbox > own to bpm/key on a copy of ``result``.

        Returns the copy, the chosen source per field and the analyzer's own
        BPM when another source won (kept as features_extra.analysis_bpm).
        """
        fields = [field for field in value_sources.FIELDS if field in result]
        if not fields:
            return result, {}, None
        components = result["features_extra"].get("analysis_components") or {}
        incoming_sources = components.get("sources") if isinstance(components.get("sources"), dict) else {}
        stored = value_sources.stored_sources(existing_fx)
        rekordbox = value_sources.lookup(
            result["filepath"], keys=value_sources.key_source_enabled(session))
        settled, sources, own_bpm = dict(result), {}, None
        for field in fields:
            incoming_source = incoming_sources.get(field, value_sources.OWN)
            value, source = value_sources.choose(
                field, result.get(field), incoming_source,
                getattr(existing_track, field, None), stored.get(field), rekordbox)
            if source is None:
                continue
            sources[field] = source
            settled[field] = value
            if field == "key":
                settled["scale"] = value_sources.scale_of(value) or result.get("scale", "")
            elif (source != value_sources.OWN and incoming_source == value_sources.OWN
                  and value_sources.valid("bpm", result.get("bpm"))):
                own_bpm = result["bpm"]
        return settled, sources, own_bpm

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
