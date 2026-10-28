"""Source candidates are disposable and never share the user's saved metadata row."""
from __future__ import annotations

import hashlib
import json
import logging
import math
from pathlib import Path
import statistics
import sys
import threading
import time

from sqlalchemy import text
from sqlmodel import Session

from api.schemas.performance_metadata import BeatGrid
from domain.models.track import Track, TrackAnalysis
from domain.services.analysis.rhythm_grid import GridAnalysisError, analyze_grid
from domain.services.analysis.beat_grid import playback_grid, VERSION as GRID_VERSION
from infra.database.connection import db_lock
from infra import rekordbox_grid
from infra import rekordbox_timing

logger = logging.getLogger(__name__)

# On-demand analysis at deck load: how long to wait for the analysis slot
# (the background embedding job yields at once; an import may not).
ON_DEMAND_WAIT_SECONDS = 3.0
# track id -> (audio fingerprint, error) of a failed on-demand analysis. A GET
# does not retry it until the file changes (or the app restarts); explicit
# re-analysis still runs.
_failed_on_demand: dict[int, tuple[str, str]] = {}
_failed_lock = threading.Lock()


def audio_fingerprint(filepath: str) -> str:
    path = Path(filepath)
    try:
        info = path.stat()
        value = [str(path), info.st_size, info.st_mtime_ns]
    except OSError:
        value = [str(path), "missing"]
    return hashlib.sha256(json.dumps(value).encode()).hexdigest()


def analysis_candidate(ticks: list[float], bpm: float | None, confidence=None) -> BeatGrid | None:
    if len(ticks) < 2:
        return None
    # Raw detections remain stored; playback uses a whole-track tempo fit only
    # when its residuals demonstrate a constant tempo.
    if bpm is None or not math.isfinite(bpm) or not 20 <= bpm <= 300:
        intervals = [b - a for a, b in zip(ticks, ticks[1:])]
        if not all(math.isfinite(d) and d > 0 for d in intervals):
            raise GridAnalysisError("Stored analysis has invalid beat timestamps")
        bpm = 60 / statistics.median(intervals)
    try:
        return BeatGrid.model_validate(playback_grid(ticks, bpm, confidence))
    except ValueError as exc:
        raise GridAnalysisError("Analysis returned invalid beat values") from exc


class GridCandidateService:
    def __init__(self, session: Session):
        self.session = session

    @staticmethod
    def _within_duration(track: Track, grid: BeatGrid) -> BeatGrid:
        duration_ms = float(track.duration or 0) * 1000
        last_ms = grid.beat_times_ms[-1] if grid.beat_times_ms else grid.first_beat_ms
        if duration_ms > 0 and last_ms >= duration_ms:
            raise GridAnalysisError(
                "The source grid extends beyond the catalog track duration; "
                "verify the audio duration before saving"
            )
        return grid

    def _cached(self, track: Track, source: str, fingerprint: str) -> BeatGrid | None:
        row = self.session.exec(text(
            "SELECT grid_json FROM track_grid_candidates "
            "WHERE track_id=:id AND source=:source AND fingerprint=:fingerprint"
        ), params={"id": track.id, "source": source, "fingerprint": fingerprint}).first()
        if row:
            try:
                grid = BeatGrid.model_validate_json(row[0])
                if grid.source == source:
                    return grid
            except ValueError:
                pass
        return None

    def _cache(self, track: Track, grid: BeatGrid, fingerprint: str, provenance: dict) -> BeatGrid:
        track_id = track.id
        with db_lock:
            # Candidate routes only read ORM objects. End their old snapshot
            # before serializing this upsert, so concurrent initial GETs cannot
            # insert/update against a stale DuckDB transaction snapshot.
            self.session.rollback()
            self.session.exec(text("""
                INSERT INTO track_grid_candidates
                    (track_id, source, grid_json, fingerprint, provenance_json, updated_at)
                VALUES (:id, :source, :grid, :fingerprint, :provenance, CURRENT_TIMESTAMP)
                ON CONFLICT (track_id, source) DO UPDATE SET
                    grid_json=excluded.grid_json, fingerprint=excluded.fingerprint,
                    provenance_json=excluded.provenance_json, updated_at=excluded.updated_at
            """), params={"id": track_id, "source": grid.source,
                           "grid": grid.model_dump_json(), "fingerprint": fingerprint,
                           "provenance": json.dumps(provenance)})
            self.session.commit()
        return grid

    def rekordbox(self, track: Track, *, refresh: bool = False) -> BeatGrid | None:
        found = rekordbox_grid.find_analysis(track.filepath)
        if found is None:
            return None
        path, external_id = found
        stat = path.stat()
        fingerprint = hashlib.sha256(json.dumps([
            str(path), stat.st_size, stat.st_mtime_ns, audio_fingerprint(track.filepath),
            rekordbox_timing.VERSION, sys.platform,
        ]).encode()).hexdigest()
        if not refresh:
            cached = self._cached(track, "rekordbox", fingerprint)
            if cached:
                return self._within_duration(track, cached)
        grid = rekordbox_grid.read_grid(path)
        if grid:
            offset = rekordbox_timing.timing_offset_ms(track.filepath)
            grid = rekordbox_timing.adjust_grid(grid, offset)
            self._within_duration(track, grid)
            return self._cache(track, grid, fingerprint, {
                "database": str(rekordbox_grid.master_db_path()), "analysis_path": str(path),
                "external_track_id": external_id, "format": "PQTZ", "read_only": True,
                "timing_offset_ms": offset, "timing_version": rekordbox_timing.VERSION,
            })
        return None

    def existing_analysis(self, track: Track) -> BeatGrid | None:
        cached = self._cached(track, "analysis", audio_fingerprint(track.filepath) + f":grid-v{GRID_VERSION}")
        if cached:
            return self._within_duration(track, cached)
        row = self.session.get(TrackAnalysis, track.id)
        if row is None:
            return None
        prepared = row.features_extra.get("playback_grid")
        if row.features_extra.get("playback_grid_version") == GRID_VERSION and prepared:
            try:
                grid = BeatGrid.model_validate(prepared)
            except ValueError as exc:
                raise GridAnalysisError("Stored analysis has invalid playback grid") from exc
            return self._within_duration(track, grid)
        grid = analysis_candidate(row.beat_positions, track.bpm,
                                  row.features_extra.get("bpm_confidence"))
        return self._within_duration(track, grid) if grid else None

    def automatic(self, track: Track) -> tuple[BeatGrid | None, str | None]:
        warnings = []
        try:
            imported = self.rekordbox(track)
            if imported:
                return imported, None
        except (rekordbox_grid.RekordboxGridError, GridAnalysisError, OSError) as exc:
            # A metadata GET must stay usable if a removable library is offline.
            # Explicit restore reports the failure; auto resolution logs it.
            logger.warning("Rekordbox grid unavailable for track %s: %s", track.id, exc)
            warnings.append("rekordbox: " + str(exc))
        try:
            grid = self.existing_analysis(track)
        except GridAnalysisError as exc:
            logger.warning("Analysis grid unavailable for track %s: %s", track.id, exc)
            warnings.append("analysis: " + str(exc))
            return None, "; ".join(warnings)
        if grid is None and Path(track.filepath).is_file():
            # Tracks registered in rekordbox skip rhythm analysis at import, so
            # without a readable PQTZ they have no beats yet: analyze them now.
            try:
                grid = self._on_demand(track)
            except GridAnalysisError as exc:
                logger.warning("On-demand grid analysis failed for track %s: %s", track.id, exc)
                warnings.append("analysis: " + str(exc))
        return grid, "; ".join(warnings) or None

    def _on_demand(self, track: Track) -> BeatGrid:
        fingerprint = audio_fingerprint(track.filepath)
        with _failed_lock:
            failed = _failed_on_demand.get(track.id)
        if failed and failed[0] == fingerprint:
            raise GridAnalysisError(failed[1])
        # Share the native analysis slot with imports and the background job
        # instead of overlapping them while the DJ is playing.
        from app.services.analysis_coordinator import analysis_coordinator
        token = None
        with analysis_coordinator.foreground_request():
            deadline = time.monotonic() + ON_DEMAND_WAIT_SECONDS
            while (token := analysis_coordinator.acquire("グリッド解析")) is None:
                if time.monotonic() >= deadline:
                    raise GridAnalysisError("Another analysis is running; the grid is analyzed on a later load", 409)
                time.sleep(0.05)
        try:
            return self.analysis(track, force=False)
        except GridAnalysisError as exc:
            if exc.status_code not in (409, 503):  # busy or changed file: retry later
                with _failed_lock:
                    _failed_on_demand[track.id] = (fingerprint, str(exc))
            raise
        finally:
            analysis_coordinator.release(token)

    def analysis(self, track: Track, *, force: bool) -> BeatGrid:
        if not force:
            existing = self.existing_analysis(track)
            if existing:
                return existing
        if not Path(track.filepath).is_file():
            raise GridAnalysisError("The audio file is unavailable", 404)
        fingerprint = audio_fingerprint(track.filepath)
        filepath = track.filepath
        # Release the read transaction before potentially long computation. This
        # endpoint never changes Track/TrackAnalysis or performance metadata.
        self.session.commit()
        result = analyze_grid(filepath)
        if audio_fingerprint(filepath) != fingerprint:
            raise GridAnalysisError("The audio file changed during analysis; retry", 409)
        grid = analysis_candidate(result["ticks"], result["bpm"], result["confidence"])
        if grid is None:
            raise GridAnalysisError("The rhythm analyzer found no usable beats")
        self._within_duration(track, grid)
        return self._cache(track, grid, fingerprint + f":grid-v{GRID_VERSION}", {
            "algorithm": "numpy-attacks-v2" if sys.platform == "win32" else "Essentia multifeature + attack alignment v4",
            "sample_rate": 11025 if sys.platform == "win32" else 44100,
            "confidence": "raw algorithm score, not a probability", "forced": force,
        })
