"""Process-wide admission control for native audio analysis.

One analysis holds the slot at a time. Foreground work (imports, selected
re-analysis) always wins over the background embedding job: a foreground
request cancels the background track in flight, and background work does not
start while a foreground request is waiting.
"""
from __future__ import annotations

from contextlib import contextmanager
import threading
import uuid


class AnalysisCoordinator:
    def __init__(self):
        self._state = threading.Condition()
        self._token: str | None = None
        self._owner: str | None = None
        self._background = False
        self._preempt = threading.Event()
        self._foreground_waiters = 0

    @contextmanager
    def foreground_request(self):
        """Mark a foreground caller that polls ``acquire`` without blocking."""
        with self._state:
            self._foreground_waiters += 1
            self._preempt_background()
        try:
            yield
        finally:
            with self._state:
                self._foreground_waiters -= 1
                self._state.notify_all()

    def _preempt_background(self) -> None:
        # Caller holds _state.
        if self._token is not None and self._background:
            self._preempt.set()

    def acquire(self, owner: str, *, wait: bool = False, background: bool = False) -> str | None:
        with self._state:
            foreground_wait = wait and not background
            if foreground_wait:
                self._foreground_waiters += 1
            try:
                if not background:
                    self._preempt_background()
                while self._token is not None or (background and self._foreground_waiters):
                    if not wait:
                        return None
                    self._state.wait()
                self._token = uuid.uuid4().hex
                self._owner = owner
                self._background = background
                if background:
                    self._preempt = threading.Event()
                return self._token
            finally:
                if foreground_wait:
                    self._foreground_waiters -= 1

    def preemption(self, token: str) -> threading.Event:
        """Set when foreground work wants the slot held by this background token."""
        with self._state:
            if token != self._token or not self._background:
                raise ValueError("Not the current background analysis")
            return self._preempt

    def release(self, token: str) -> None:
        with self._state:
            if token != self._token:
                return
            self._token = None
            self._owner = None
            self._background = False
            self._state.notify_all()

    @property
    def owner(self) -> str | None:
        with self._state:
            return self._owner


analysis_coordinator = AnalysisCoordinator()
