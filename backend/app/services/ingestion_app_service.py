import asyncio
import os
import multiprocessing
from domain.services.analysis.process_runner import AnalysisExecutor
from domain.services.analysis.light_dsp import WORKER_TIMEOUT as LIGHT_WORKER_TIMEOUT
from typing import List, Dict, Any, Optional
from sqlmodel import Session, select
from config import settings
from domain.models.track import Track, TrackEmbedding
from domain.services.ingestion_domain_service import IngestionDomainService
from infra.repositories.ingestion_repository import IngestionRepository
from utils.ingestion import expand_targets, filter_and_prioritize_files
from app.services.background_task_service import BackgroundTaskService
from app.services.analysis_coordinator import analysis_coordinator
import sys
import time

# Windows decoder/native hangs must become visible failures in a useful amount
# of time. Keep the established macOS allowance unchanged.
WORKER_TIMEOUT = 165.0 if sys.platform == "win32" else 570.0


def _is_timeout(exc: BaseException) -> bool:
    seen: set[int] = set()
    current: BaseException | None = exc
    while current is not None and id(current) not in seen:
        if isinstance(current, TimeoutError):
            return True
        seen.add(id(current))
        current = current.__cause__ or current.__context__
    return "timed out" in str(exc).lower()

class IngestionAppService(BackgroundTaskService):
    def __init__(self):
        super().__init__()
        self.state.update({
            "file": ""
        })
        self.executor = None
        self.db_lock = asyncio.Lock()
        self.domain_service = IngestionDomainService()
        self.repository = IngestionRepository()

    async def start_ingestion(self, targets: List[str], force_update: bool = False,
                              analysis_profile: str = "auto") -> bool:
        if analysis_profile not in {"auto", "light", "full"}:
            raise ValueError("Unknown analysis profile")
        token = None

        async def run_with_token():
            nonlocal token
            request = analysis_coordinator.foreground_request()
            request.__enter__()
            try:
                # Acquire only once this coroutine is running. Cancelling the
                # task before its first step otherwise bypasses this finally
                # and leaks the global slot forever.
                token = analysis_coordinator.acquire("Explorer解析")
                if token is None:
                    # Queue behind a Play import (or other analysis) instead of
                    # rejecting the request. Non-blocking polls keep cancel safe:
                    # a cancelled wait never leaves an acquired slot behind.
                    self.update_state(
                        type="start", total=0, current=0, file="",
                        processed=0, skipped=0, errors=0,
                        stage=f"{analysis_coordinator.owner or '別の解析'}の完了を待っています",
                        active_files={}, failed_files=[], last_error="",
                        analysis_profile=analysis_profile, queued=True,
                    )
                    await self.emit_state()
                    while token is None:
                        await asyncio.sleep(1.0)
                        token = analysis_coordinator.acquire("Explorer解析")
                    self.update_state(start_time=time.time(), queued=False)
                await self._run_ingestion(targets, force_update, analysis_profile)
            finally:
                request.__exit__(None, None, None)
                if token is not None:
                    analysis_coordinator.release(token)

        started = await self.start_task(run_with_token())
        if not started:
            if token is not None:
                analysis_coordinator.release(token)
            return False
        return True

    async def cancel_ingestion(self):
        await self.cancel_task()

    async def _run_ingestion(self, targets: List[str], force_update: bool,
                             analysis_profile: str = "auto"):
        async def heartbeat():
            while True:
                await asyncio.sleep(3)
                await self.emit_state()

        embedding_paths = []
        pulse = asyncio.create_task(heartbeat())
        try:
            # Notify start immediately to show loading state
            self.update_state(
                type="start",
                total=0,
                current=0,
                file="",
                processed=0,
                skipped=0,
                errors=0,
                stage="音源ファイルを検索中",
                active_files={},
                active_progress={},
                failed_files=[],
                last_error="",
                queued=False,
                analysis_profile=analysis_profile,
                effective_analysis_profile=(
                    "full" if sys.platform != "win32" or analysis_profile == "auto"
                    else analysis_profile
                ),
            )
            await self.emit_state()

            expanded_files = await asyncio.to_thread(expand_targets, targets)
            self.update_state(stage="解析が必要な音源を確認中")
            await self.emit_state()
            filter_profile = (
                "full" if sys.platform != "win32" or analysis_profile == "full"
                else analysis_profile
            )
            skipped_db_paths = []
            files_to_process, _ = await asyncio.to_thread(
                filter_and_prioritize_files, expanded_files, force_update, filter_profile, skipped_db_paths
            )
            
            # An explicit re-import can repair an optional embedding whose
            # earlier enqueue failed or whose background job failed. Playback
            # readiness deliberately does not require that embedding.
            # Use the saved DB spelling selected by the same NFC comparison
            # as the filter, including mixed NFC/NFD path components.
            embedding_paths.extend(skipped_db_paths)

            total_files = len(files_to_process)
            if total_files == 0:
                self.update_state(type="complete", total=0)
                await self.emit_state()
                return

            self.update_state(
                type="processing",
                total=total_files,
                current=0,
                file="",
                processed=0,
                skipped=0,
                errors=0,
                start_time=time.time(),
                estimated_remaining=0,
                stage="音源を解析中"
            )
            await self.emit_state()

            max_workers = 1 if sys.platform == "win32" else min(4, max(1, multiprocessing.cpu_count() - 1))
            loop = asyncio.get_running_loop()
            effective_profile = (
                "full" if sys.platform != "win32" or analysis_profile == "auto"
                else analysis_profile
            )
            
            # Concurrency control
            sem = asyncio.Semaphore(max_workers)

            async def process_single_file(filepath: str):
                nonlocal effective_profile
                async with sem:
                    # Update UI state (Best effort)
                    current_count = self.state["processed"] + self.state["skipped"] + self.state["errors"] + 1
                    self.update_state(
                        file=os.path.basename(filepath),
                        current=current_count,
                        type="processing"
                    )
                    self.state["details"]["active_files"][filepath] = time.time()
                    self.state["details"]["active_progress"][filepath] = "解析ワーカーを起動しています"
                    await self.emit_state()

                    def report_progress(event):
                        def update():
                            if filepath not in self.state["details"].get("active_files", {}):
                                return
                            stages = dict(self.state["details"].get("active_progress", {}))
                            stages[filepath] = event["label"]
                            self.update_state(active_progress=stages)
                            asyncio.create_task(self.emit_state())
                        loop.call_soon_threadsafe(update)

                    try:
                        async def analyze(profile: str):
                            timeout = 600.0 if profile == "full" else LIGHT_WORKER_TIMEOUT + 30
                            if profile == "light":
                                executor.task_timeout = LIGHT_WORKER_TIMEOUT
                            return await asyncio.wait_for(
                                self.domain_service.process_track_ingestion(
                                    filepath, force_update, loop, executor, timeout,
                                    self.db_lock, save_to_db=True,
                                    analysis_profile=profile,
                                    on_progress=report_progress,
                                ),
                                timeout=timeout + 30,
                            )

                        try:
                            result = await analyze(effective_profile)
                        except Exception as exc:
                            if not (
                                sys.platform == "win32"
                                and analysis_profile == "auto"
                                and effective_profile == "full"
                                and _is_timeout(exc)
                            ):
                                raise
                            effective_profile = "light"
                            self.update_state(
                                stage="詳細解析に時間がかかったため軽量解析へ切り替えています",
                                effective_analysis_profile="light",
                            )
                            await self.emit_state()
                            result = await analyze("light")
                        
                        embedding_paths.append(filepath)
                        if result:
                            self.state["processed"] += 1
                        else:
                            self.state["skipped"] += 1
                            
                    except Exception as e:
                        print(f"ERROR: Ingestion failed for {filepath}: {e}")
                        self.state["errors"] += 1
                        self.state["details"]["failed_files"].append(filepath)
                        self.update_state(last_error=f"{os.path.basename(filepath)}: {type(e).__name__}: {e}")
                    finally:
                        self.state["details"]["active_files"].pop(filepath, None)
                        self.state["details"]["active_progress"].pop(filepath, None)
                    
                    # Update progress estimation
                    self.update_state(type="progress", processed=self.state["processed"],
                                      current=self.state["processed"] + self.state["skipped"] + self.state["errors"])
                    await self.emit_state()

            worker_timeout = (LIGHT_WORKER_TIMEOUT if effective_profile == "light"
                              else 570.0 if analysis_profile == "full" else WORKER_TIMEOUT)
            executor = AnalysisExecutor(max_workers=max_workers, task_timeout=worker_timeout)
            try:
                self.executor = executor
                
                # Create tasks for all files
                tasks = [process_single_file(fp) for fp in files_to_process]
                
                # Run tasks concurrently
                try:
                    await asyncio.gather(*tasks)
                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    raise
            finally:
                # Waiting for a native worker must not freeze HTTP/WebSocket.
                await asyncio.to_thread(executor.shutdown, wait=True, cancel_futures=True)

            self.update_state(type="complete", file="")
            await self.emit_state()
            
        except asyncio.CancelledError:
            print("Ingestion cancelled.")
            self.update_state(type="cancelled")
            await self.emit_state()
        except Exception as e:
            print(f"CRITICAL ERROR in ingestion loop: {e}")
            self.update_state(type="error", message=str(e))
            await self.emit_state()
        finally:
            pulse.cancel()
            await asyncio.gather(pulse, return_exceptions=True)
            self.executor = None
            if embedding_paths:
                from app.services.analysis_job_service import analysis_job_service
                # Limit waveform/grid rows materialized by the queue's query
                # when a large directory of already-ready tracks is re-imported.
                paths = list(dict.fromkeys(embedding_paths))
                for offset in range(0, len(paths), 200):
                    try:
                        await asyncio.to_thread(
                            analysis_job_service.enqueue_embedding,
                            filepaths=paths[offset:offset + 200],
                        )
                    except Exception as exc:
                        print(f"WARNING: could not queue optional embeddings: {exc}", flush=True)

# Global Instance
ingestion_app_service = IngestionAppService()
