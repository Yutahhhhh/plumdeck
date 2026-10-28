"""Selective, resumable reanalysis. Only the server thread writes the music DB."""
import json
import os
import threading
import time
import sys
from domain.services.analysis.process_runner import AnalysisExecutor, run_isolated
from concurrent.futures import ThreadPoolExecutor, wait, FIRST_COMPLETED
from datetime import datetime

import numpy as np
from sqlmodel import Session, select
from sqlalchemy import text, func

from config import settings
from domain.models.track import Track, TrackAnalysis, TrackEmbedding
from domain.models.setlist import SetlistTrack
from domain.services import value_sources
from domain.services.analysis.constants import COMPONENT_VERSIONS, EMBEDDING_MODEL, GRID_COMPONENT_VERSION
from infra import rekordbox_grid, rekordbox_library
from infra.database import connection
from infra.repositories.analysis_job_repository import AnalysisJobRepository
from app.services.analysis_coordinator import analysis_coordinator
from utils.features_extra import persisted_features_extra
from utils.ingestion import completed_analysis_condition

# Same per-track deadline as AnalysisExecutor.
BACKGROUND_TASK_TIMEOUT = 570


def analyze_components(filepath, features):
    # Each process owns its native audio state; Essentia has global native state too.
    before = os.stat(filepath)
    started = time.monotonic()
    if features == ["rhythm"]:
        # Already in an isolated job worker. Reuse the bounded grid-only path;
        # no embedding model, key/timbre work, or Windows Numba compilation.
        from domain.services.analysis.rhythm_grid import _extract
        from domain.services.analysis.beat_grid import playback_grid, VERSION
        grid = _extract(filepath)
        result = {"bpm": grid["bpm"], "features_extra": {
            "beat_positions": grid["ticks"], "bpm_confidence": grid["confidence"],
            "playback_grid": playback_grid(grid["ticks"], grid["bpm"], grid["confidence"]),
            "playback_grid_version": VERSION,
            "analysis_components": {"rhythm": GRID_COMPONENT_VERSION},
        }}
    else:
        from ingest import get_analyzer
        analyzer = get_analyzer()
        if analyzer is None:
            raise RuntimeError("Audio analyzer is unavailable")
        result = analyzer.analyze_selected(filepath, features)
    after = os.stat(filepath)
    if (before.st_mtime_ns, before.st_size) != (after.st_mtime_ns, after.st_size):
        raise RuntimeError("Audio file changed during analysis; retry this track")
    result["features_extra"]["analysis_source"] = {"size": after.st_size, "mtime_ns": after.st_mtime_ns}
    return result, time.monotonic() - started


def _rekordbox_grid_check():
    """Return path -> whether rekordbox has a readable PQTZ grid for it.

    Grid re-analysis skips those tracks: their PQTZ grid outranks plumdeck's
    own beats. When master.db cannot be read, no track is skipped.
    """
    try:
        index = rekordbox_library.collection_index()
        database = rekordbox_grid.master_db_path()
    except Exception:
        return lambda path: False

    def has_grid(path):
        entry = index.get(path)
        return bool(entry and entry.analysis_path and database
                    and rekordbox_grid.has_usable_grid(entry.analysis_path, database))
    return has_grid


class AnalysisJobService:
    def __init__(self, engine=None, queue_path=None, analyze_fn=None):
        self._engine = engine
        self._queue_path = queue_path
        self.analyze_fn = analyze_fn or analyze_components
        self._test_executor = analyze_fn is not None
        self._repo = None
        self._thread = None
        self._job_id = None
        self._stop = threading.Event()
        self._lock = threading.RLock()
        self._admission_token = None
        self._shutdown = False
        self._background = False
        self._running = False
        self._preempt_background = False
        self._background_cancel = None
        self._suspended = 0

    @property
    def engine(self):
        return self._engine or connection.engine

    @property
    def repository(self):
        with self._lock:
            if self._repo is None:
                # Bind the queue to this library, including when DB_PATH is overridden.
                self._repo = AnalysisJobRepository(self._queue_path or connection.DB_PATH + ".analysis-jobs.sqlite3")
                self._repo.recover()
            return self._repo

    @property
    def is_running(self):
        return self._running

    @property
    def is_background_running(self):
        return self.is_running and self._background

    def enqueue_embedding(self, *, track_ids=None, filepaths=None):
        """Durably enqueue only imported, ready tracks; never scan the library."""
        if not track_ids and not filepaths:
            return {"status": "idle"}
        with connection.database_activity, self._lock:
            # Project ids and vectors only; never load waveform/grid BLOBs here.
            query = select(Track.id, Track.filepath, TrackEmbedding).outerjoin(
                TrackEmbedding, Track.id == TrackEmbedding.track_id).where(completed_analysis_condition())
            if track_ids is not None:
                query = query.where(Track.id.in_(track_ids))
            if filepaths is not None:
                query = query.where(Track.filepath.in_(filepaths))
            with Session(self.engine) as session:
                tracks = [{"id": track_id, "filepath": filepath}
                          for track_id, filepath, embedding in session.exec(query).all()
                          if self.outdated(["embedding"], None, embedding)]
            if not tracks:
                return {"status": "idle"}
            # Keep re-imports idempotent while existing work is queued or
            # running. Items of failed or paused jobs may be queued again by a
            # later import; a moved path must not be blocked by an old item.
            with self.repository.connect() as queue:
                outstanding = set()
                rows = queue.execute(
                    "SELECT i.track_id, i.filepath, j.config FROM items i "
                    "JOIN jobs j ON j.id = i.job_id "
                    "WHERE j.status IN ('queued', 'running', 'pausing') "
                    "AND i.status IN ('pending', 'running')")
                for row in rows:
                    config = json.loads(row["config"])
                    if config.get("background") and "embedding" in config.get("features", []):
                        outstanding.add((row["track_id"], row["filepath"]))
            tracks = [track for track in tracks if (track["id"], track["filepath"]) not in outstanding]
            if not tracks:
                return {"status": "idle"}
            config = {"features": ["embedding"], "only_outdated": True,
                      "workers": 1, "background": True}
            job_id = self.repository.create(config, tracks, status="queued")
            self._launch_next_queued()
            return self.repository.get(job_id)

    def startup(self):
        with self._lock:
            if not self.is_running:
                self._shutdown = False
                if self._queue_path is None and self._repo is not None and self._repo.path != connection.DB_PATH + ".analysis-jobs.sqlite3":
                    self._repo = None
                self._launch_next_queued()

    def suspend_background(self, timeout=15):
        """Stop the automatic embedding job for backup/restore and keep queued
        jobs from starting until ``resume_background``. Call it before taking
        the database locks: the job thread needs them to put its track back."""
        with self._lock:
            self._suspended += 1
            thread = self._thread if self.is_background_running else None
            if thread is not None:
                self._yield_background()
        if thread is not None:
            thread.join(timeout)

    def resume_background(self):
        with self._lock:
            self._suspended = max(0, self._suspended - 1)
            self._launch_next_queued()

    def _launch_next_queued(self):
        with self._lock:
            if self._shutdown or self._suspended or self.is_running:
                return
            job_id = self.repository.next_queued()
            if job_id:
                self._launch(job_id, None)

    @staticmethod
    def validate_features(features):
        features = list(dict.fromkeys(features if features is not None else ["embedding"]))
        if not features or set(features) - COMPONENT_VERSIONS.keys():
            raise ValueError("features must contain embedding, rhythm, key, timbre or waveform")
        return features

    @staticmethod
    def outdated(features, analysis, embedding):
        extra = analysis.features_extra if analysis else {}
        versions = extra.get("analysis_components", {})
        needed = []
        for feature in features:
            if feature == "embedding":
                valid = embedding is not None and embedding.model_name == EMBEDDING_MODEL
                if valid:
                    try:
                        vec = np.asarray(json.loads(embedding.embedding_json), dtype=float)
                        valid = vec.shape == (200,) and np.isfinite(vec).all() and np.linalg.norm(vec) > 0
                    except (TypeError, ValueError):
                        valid = False
                if not valid:
                    needed.append(feature)
            elif feature == "rhythm" and versions.get(feature) in {COMPONENT_VERSIONS[feature], GRID_COMPONENT_VERSION}:
                continue
            elif versions.get(feature) != COMPONENT_VERSIONS[feature]:
                needed.append(feature)
        return needed

    def _select(self, track_ids=None, genres=None, features=None, only_outdated=True, limit=None):
        features = self.validate_features(features)
        if limit is not None and not 1 <= limit <= 100000:
            raise ValueError("limit must be between 1 and 100000")
        rhythm_only = features == ["rhythm"]
        if rhythm_only:
            # Planning a 25k-track grid update must not materialize every
            # waveform, dense beat grid and 200-D embedding in Python.
            query = select(Track.id, Track.filepath, Track.title, Track.genre,
                           func.json_extract_string(TrackAnalysis.features_extra_json, '$.analysis_components.rhythm')).outerjoin(TrackAnalysis, Track.id == TrackAnalysis.track_id)
        else:
            query = select(Track, TrackAnalysis, TrackEmbedding).outerjoin(TrackAnalysis, Track.id == TrackAnalysis.track_id).outerjoin(TrackEmbedding, Track.id == TrackEmbedding.track_id)
        if track_ids is not None:
            query = query.where(Track.id.in_(track_ids))
        if genres:
            query = query.where(Track.genre.in_(genres))
        rekordbox_grids = 0
        with Session(self.engine) as session:
            priority = set(session.exec(select(SetlistTrack.track_id)).all())
            rows = session.exec(query.order_by(Track.id)).all()
            if rhythm_only:
                has_grid = _rekordbox_grid_check()
                kept = [row for row in rows if not has_grid(row[1])]
                rekordbox_grids = len(rows) - len(kept)
                current_versions = {COMPONENT_VERSIONS["rhythm"], GRID_COMPONENT_VERSION}
                candidates = [{"id": id, "filepath": path, "title": title, "genre": genre,
                               "features": [] if only_outdated and version in current_versions else features}
                              for id, path, title, genre, version in kept]
            else:
                candidates = [{"id": t.id, "filepath": t.filepath, "title": t.title, "genre": t.genre,
                               "features": self.outdated(features, a, e) if only_outdated else features}
                              for t, a, e in rows]
        eligible = [t for t in candidates if t["features"]]
        eligible.sort(key=lambda t: (t["id"] not in priority, t["id"]))
        return features, candidates, eligible[:limit] if limit else eligible, rekordbox_grids

    def plan(self, track_ids=None, genres=None, features=None, only_outdated=True, limit=None):
        features, candidates, selected, rekordbox_grids = self._select(track_ids, genres, features, only_outdated, limit)
        grid_only = features == ["rhythm"]
        return {
            "features": features, "versions": {f: GRID_COMPONENT_VERSION if grid_only else COMPONENT_VERSIONS[f] for f in features},
            "matched_tracks": len(candidates), "already_current": sum(not t["features"] for t in candidates),
            "selected_tracks": len(selected), "sample": selected[:20],
            # Grid re-analysis leaves tracks with a rekordbox grid out entirely.
            "rekordbox_grid_tracks": rekordbox_grids,
            "preserves": ["metadata", "genres", "lyrics", "audio_files", "unselected_features"]
                         + (["bpm"] if grid_only else []),
            "priority": "tracks used in setlists first", "resumable": True,
        }

    def start(self, track_ids=None, genres=None, features=None, only_outdated=True, limit=None, workers=2):
        if sys.platform == "win32":
            workers = 1
        with connection.database_activity, self._lock:
            self._check_available(workers)
            if self.is_background_running:
                features, _, tracks, _ = self._select(track_ids, genres, features, only_outdated, limit)
                job_id = self.repository.create({"features": features, "only_outdated": only_outdated,
                                                 "workers": workers}, tracks, status="queued")
                self._yield_background()
                return self.repository.get(job_id)
            token = analysis_coordinator.acquire("選択解析")
            if token is None:
                raise ValueError(f"{analysis_coordinator.owner or '別の'}解析が実行中です")
            try:
                features, _, tracks, _ = self._select(track_ids, genres, features, only_outdated, limit)
                config = {"features": features, "only_outdated": only_outdated, "workers": workers}
                job_id = self.repository.create(config, tracks)
                self._launch(job_id, token)
            except Exception:
                analysis_coordinator.release(token)
                raise
            return self.repository.get(job_id)

    def _check_available(self, workers):
        if isinstance(workers, bool) or not 1 <= workers <= 4:
            raise ValueError("workers must be between 1 and 4")
        if self.is_running and not self.is_background_running:
            raise ValueError("An analysis job is already running; pause it first")
        from app.services.ingestion_app_service import ingestion_app_service
        if ingestion_app_service.is_running:
            raise ValueError("Library import is running; wait for it to finish")

    def _launch(self, job_id, token):
        self._background = bool(self.repository.get(job_id)["config"].get("background"))
        self._preempt_background = False
        self._running = True
        self._admission_token = token
        self._job_id = job_id
        self._stop.clear()
        self.repository.update_job(job_id, "running")
        self._thread = threading.Thread(target=self._run, args=(job_id,), name="plumdeck-analysis-job", daemon=True)
        self._thread.start()

    def pause(self, job_id):
        with self._lock:
            self.repository.get(job_id)
            if self.is_running and self._job_id == job_id:
                self._stop.set()
                self.repository.update_job(job_id, "pausing")
            elif self.repository.get(job_id)["status"] == "queued":
                self.repository.update_job(job_id, "paused")
            return self.repository.get(job_id)

    def resume(self, job_id, workers=None, retry_failed=False):
        with self._lock:
            job = self.repository.get(job_id)
            workers = workers if workers is not None else job["config"]["workers"]
            if sys.platform == "win32":
                workers = 1
            if self.is_running and self._job_id == job_id:
                raise ValueError("This analysis job is already running")
            self._check_available(workers)
            background = job["config"].get("background", False)
            if self.is_background_running:
                self.repository.prepare_resume(job_id, workers, retry_failed)
                self.repository.update_job(job_id, "queued")
                if not background:
                    self._yield_background()
                return self.repository.get(job_id)
            token = None if background else analysis_coordinator.acquire("選択解析")
            if token is None and not background:
                raise ValueError(f"{analysis_coordinator.owner or '別の'}解析が実行中です")
            try:
                self.repository.prepare_resume(job_id, workers, retry_failed)
                self._launch(job_id, token)
            except Exception:
                analysis_coordinator.release(token)
                raise
            return self.repository.get(job_id)

    def status(self, job_id=None):
        """Without an id: the running or queued user job, else the latest
        one. Automatic embedding jobs show only when no user job exists."""
        if job_id is None:
            if self.is_running and not self.is_background_running:
                job_id = self._job_id
            else:
                job_id = self.repository.next_queued(background=False) or self.repository.latest_id(background=False)
        return self.repository.get(job_id)

    def _yield_background(self):
        """Stop the background job now; its track in flight goes back to pending."""
        self._preempt_background = True
        self._stop.set()
        cancel = self._background_cancel
        if cancel is not None:
            cancel.set()

    def shutdown(self):
        self._shutdown = True
        self._stop.set()
        cancel = self._background_cancel
        if cancel is not None:
            cancel.set()
        if self._thread:
            self._thread.join(timeout=30)

    def _prepare_item(self, item, config):
        with Session(self.engine) as session:
            track = session.get(Track, item["track_id"])
            if track is None or track.filepath != item["filepath"]:
                raise ValueError("Track removed or path changed since this job was created")
            return self.outdated(config["features"], session.get(TrackAnalysis, track.id), session.get(TrackEmbedding, track.id)) if config["only_outdated"] else config["features"]

    def _save(self, item, result, features, grid_only=False):
        """Store one result. BPM/key follow manual > rekordbox > own; a grid
        re-analysis (``grid_only``) updates beats and never replaces a BPM."""
        # Deliberately bypass metadata ingestion: it may update tags and lyrics.
        with connection.db_lock, Session(self.engine) as session:
            track = session.get(Track, item["track_id"])
            if track is None or track.filepath != item["filepath"]:
                raise ValueError("Track removed or path changed during analysis")
            allowed = {
                "rhythm": ("bpm",), "key": ("key", "scale"),
                "timbre": ("energy", "danceability", "brightness", "noisiness"),
            }
            analysis = session.get(TrackAnalysis, track.id) or TrackAnalysis(track_id=track.id)
            extra = dict(analysis.features_extra)
            update = dict(result.get("features_extra", {}))
            stored = value_sources.stored_sources(extra)
            rekordbox = None
            sources = {}
            for feature in features:
                for name in allowed.get(feature, ()):
                    value = result[name]
                    if not isinstance(value, str) and not np.isfinite(value):
                        raise ValueError(f"Invalid {name} result")
                    if name == "scale":
                        continue  # follows the chosen key below
                    if name not in value_sources.FIELDS:
                        setattr(track, name, value)
                        continue
                    if name == "bpm" and grid_only:
                        update["analysis_bpm"] = value
                        continue
                    if rekordbox is None:
                        rekordbox = value_sources.lookup(
                            track.filepath, keys=value_sources.key_source_enabled(session))
                    chosen, source = value_sources.choose(
                        name, value, value_sources.OWN, getattr(track, name), stored.get(name), rekordbox)
                    if source is None:
                        continue
                    sources[name] = source
                    setattr(track, name, chosen)
                    if name == "key":
                        track.scale = value_sources.scale_of(chosen) or result.get("scale", track.scale)
                    elif source == value_sources.OWN:
                        extra.pop("analysis_bpm", None)
                    else:
                        # Keep the analyzer's estimate beside the winning value.
                        update["analysis_bpm"] = value
            components = {**extra.get("analysis_components", {}), **update.pop("analysis_components", {})}
            if "rhythm" in features:
                analysis.beat_positions = update.pop("beat_positions")
                # A new rhythm result invalidates only disposable analysis
                # candidates. Manual grids and rekordbox imports are intact.
                session.exec(text("DELETE FROM track_grid_candidates WHERE track_id=:id AND source='analysis'"), params={"id": track.id})
                from infra.repositories.analysis_grid_refresh import refresh_saved_analysis_grid
                refresh_saved_analysis_grid(session, track.id, update.get("playback_grid"))
            if "waveform" in features:
                analysis.waveform_peaks = update.pop("waveform_peaks")
            extra.update(persisted_features_extra(update))
            extra["analysis_components"] = components
            if sources:
                extra = value_sources.with_sources(extra, sources)
            analysis.features_extra_json = json.dumps(extra, allow_nan=False)
            if "embedding" in features:
                vec = np.asarray(result["embedding"], dtype=float)
                if vec.shape != (200,) or not np.isfinite(vec).all() or np.linalg.norm(vec) == 0:
                    raise ValueError("Expected a finite, nonzero 200-dimensional embedding")
                embedding = session.get(TrackEmbedding, track.id) or TrackEmbedding(track_id=track.id)
                embedding.embedding_json = json.dumps(vec.tolist(), allow_nan=False)
                embedding.model_name = result["embedding_model"]
                embedding.updated_at = datetime.now()
                session.add(embedding)
            session.add(analysis)
            session.commit()

    def _run_background(self, job_id, config):
        """Hold the native slot one track at a time; foreground work cancels
        the track in flight, which goes back to pending and runs again later."""
        started = time.monotonic()
        elapsed = self.repository.get(job_id)["elapsed"]
        while not self._stop.is_set():
            token = analysis_coordinator.acquire("音色比較の計算", background=True)
            if token is None:
                self._stop.wait(0.25)
                continue
            cancel = analysis_coordinator.preemption(token)
            self._background_cancel = cancel
            try:
                if self._stop.is_set():
                    break
                item = self.repository.claim(job_id)
                if item is None:
                    break
                try:
                    features = self._prepare_item(item, config)
                    if not features:
                        self.repository.finish(job_id, item["track_id"], "skipped")
                        continue
                    if self._test_executor:
                        result, seconds = self.analyze_fn(item["filepath"], features)
                    else:
                        result, seconds = run_isolated(self.analyze_fn, (item["filepath"], features),
                                                       BACKGROUND_TASK_TIMEOUT, cancel)
                    self._save(item, result, features)
                    self.repository.finish(job_id, item["track_id"], "completed", seconds)
                except Exception as exc:
                    if cancel.is_set():
                        self.repository.finish(job_id, item["track_id"], "pending")
                    else:
                        self.repository.finish(job_id, item["track_id"], "failed", error=str(exc))
            finally:
                self._background_cancel = None
                analysis_coordinator.release(token)
            self.repository.update_job(job_id, "pausing" if self._stop.is_set() else "running",
                                       elapsed + time.monotonic() - started)
        latest = self.repository.get(job_id)
        status = "paused" if latest["remaining"] else ("completed_with_errors" if latest["counts"]["failed"] else "completed")
        # Work stopped by shutdown or by a foreground job resumes by itself;
        # an explicit pause does not.
        if (self._shutdown or self._preempt_background) and latest["remaining"]:
            status = "queued"
        self.repository.update_job(job_id, status, elapsed + time.monotonic() - started)

    def _run(self, job_id):
        started = time.monotonic()
        initial_elapsed = 0
        futures = {}
        try:
            job = self.repository.get(job_id)
            config = job["config"]
            if config.get("background"):
                self._run_background(job_id, config)
                return
            initial_elapsed = job["elapsed"]
            if self._admission_token is None:
                with analysis_coordinator.foreground_request():
                    while not self._stop.is_set() and self._admission_token is None:
                        self._admission_token = analysis_coordinator.acquire("選択解析")
                        if self._admission_token is None:
                            self._stop.wait(0.05)
                if self._admission_token is None:
                    self.repository.update_job(job_id, "paused")
                    return
            executor = (ThreadPoolExecutor(max_workers=config["workers"]) if self._test_executor else
                        AnalysisExecutor(max_workers=1 if sys.platform == "win32" else config["workers"]))
            with executor as pool:
                exhausted = False
                while futures or (not exhausted and not self._stop.is_set()):
                    while len(futures) < config["workers"] and not exhausted and not self._stop.is_set():
                        item = self.repository.claim(job_id)
                        if item is None:
                            exhausted = True
                            break
                        try:
                            features = self._prepare_item(item, config)
                            if not features:
                                self.repository.finish(job_id, item["track_id"], "skipped")
                                continue
                            future = pool.submit(self.analyze_fn, item["filepath"], features)
                            futures[future] = (item, features)
                        except Exception as exc:
                            self.repository.finish(job_id, item["track_id"], "failed", error=str(exc))
                    if futures:
                        done, _ = wait(futures, timeout=1, return_when=FIRST_COMPLETED)
                        for future in done:
                            item, features = futures.pop(future)
                            try:
                                result, seconds = future.result()
                                self._save(item, result, features, grid_only=config["features"] == ["rhythm"])
                                self.repository.finish(job_id, item["track_id"], "completed", seconds)
                            except Exception as exc:
                                self.repository.finish(job_id, item["track_id"], "failed", error=str(exc))
                    self.repository.update_job(job_id, "pausing" if self._stop.is_set() else "running",
                                               initial_elapsed + time.monotonic() - started)
            latest = self.repository.get(job_id)
            status = "paused" if latest["remaining"] else ("completed_with_errors" if latest["counts"]["failed"] else "completed")
            self.repository.update_job(job_id, status, initial_elapsed + time.monotonic() - started)
        except Exception as exc:
            try:
                self.repository.update_job(job_id, "failed", initial_elapsed + time.monotonic() - started, str(exc))
            except Exception as update_error:
                print(f"CRITICAL: analysis job {job_id} could not be finalized: {update_error}", flush=True)
        finally:
            token, self._admission_token = self._admission_token, None
            if token:
                analysis_coordinator.release(token)
            with self._lock:
                self._running = False
                self._launch_next_queued()


analysis_job_service = AnalysisJobService()
