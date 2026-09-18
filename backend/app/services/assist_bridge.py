"""Assist bridge: lets the DJ's own AI agent drive the assist window over MCP.

plumdeck never calls an LLM. Instead the assist window reports what it is
showing (deck, preset, conditions, exclusions), and an agent connected to the
plumdeck MCP server — Claude Code, Codex, any agent app — can read that, change
the window's settings, or put its own picks on screen.

Everything here is in memory only: it is performance-time state, not library
data, so assist still never writes to either library.
"""
from __future__ import annotations

import threading
import time
from typing import Any, Optional

# The window reports on every change; older reports mean it is not open.
WINDOW_STALE_SECONDS = 30.0
MAX_TITLE = 120
MAX_NOTE = 1000


class AssistBridge:
    def __init__(self, clock=time.time):
        self._clock = clock
        self._lock = threading.Lock()
        self._revision = 0
        self._window: Optional[dict[str, Any]] = None
        self._window_seen_at: Optional[float] = None
        self._settings: Optional[dict[str, Any]] = None
        self._list: Optional[dict[str, Any]] = None

    # ------------------------------------------------------------ window side

    def report_window(self, state: dict[str, Any]) -> None:
        with self._lock:
            self._window = dict(state)
            self._window_seen_at = self._clock()

    def pending(self) -> dict[str, Any]:
        """What the window should show from the agent: settings and a list."""
        with self._lock:
            return {"revision": self._revision, "settings": self._settings, "list": self._list}

    def dismiss_list(self) -> None:
        with self._lock:
            if self._list is not None:
                self._list = None
                self._revision += 1

    # ------------------------------------------------------------- agent side

    def window(self) -> dict[str, Any]:
        with self._lock:
            seen = self._window_seen_at
            return {
                "open": seen is not None and self._clock() - seen <= WINDOW_STALE_SECONDS,
                "reported_seconds_ago": round(self._clock() - seen, 1) if seen is not None else None,
                "state": dict(self._window) if self._window else None,
                "agent_list": self._list,
            }

    def apply_settings(self, settings: dict[str, Any], agent_name: Optional[str] = None) -> dict[str, Any]:
        """Ask the window to switch preset/conditions; it then re-ranks itself."""
        with self._lock:
            self._revision += 1
            self._settings = {
                **{key: value for key, value in settings.items() if value is not None},
                "revision": self._revision,
                "agent_name": agent_name,
                "created_at": self._clock(),
            }
            return self._settings

    def show_list(
        self,
        title: str,
        tracks: list[dict[str, Any]],
        rejected: list[dict[str, Any]],
        note: Optional[str] = None,
        agent_name: Optional[str] = None,
    ) -> dict[str, Any]:
        with self._lock:
            self._revision += 1
            self._list = {
                "revision": self._revision,
                "title": (title or "").strip()[:MAX_TITLE] or "エージェントの提案",
                "note": (note or "").strip()[:MAX_NOTE] or None,
                "tracks": list(tracks),
                "rejected": list(rejected),
                "agent_name": agent_name,
                "created_at": self._clock(),
            }
            return self._list

    def clear_list(self) -> None:
        self.dismiss_list()


assist_bridge = AssistBridge()
