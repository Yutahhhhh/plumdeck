"""Run the Python backend as the parity oracle (side A).

    DB_PATH=/sandbox/a/ud/plumdeck.duckdb USER_DATA_DIR=/sandbox/a/ud \\
    PLUMDECK_REKORDBOX_DB=/sandbox/a/rb/master.db \\
        python tools/parity/oracle_server.py --port 18001

Starts backend/main.py's FastAPI app under uvicorn exactly as server.py does
(127.0.0.1, one worker, 5 s graceful shutdown; SIGINT/SIGTERM run the lifespan
shutdown, which CHECKPOINTs the DB), without server.py's sidecar stdin handling.

Refuses to start unless DB_PATH, USER_DATA_DIR and PLUMDECK_REKORDBOX_DB are
set and every location the backend resolves is outside the live data dirs
(parity_env). PLUMDECK_REKORDBOX_DB is this side's copy of master.db, or a
missing file for "no rekordbox". The waveform cache moves to
XDG_CACHE_HOME (default: <USER_DATA_DIR>/../cache).
Later slices add their monkeypatches (fixture analyzer, stub URLs, fake
command runner) in `install_patches`.
"""
from __future__ import annotations

import argparse
import os
import signal
import sys
from pathlib import Path

import parity_env


def install_patches(cache_dir: Path) -> None:
    """Determinism and sandbox hooks; S0 only moves the waveform cache."""
    from app.services import waveform_detail_service

    target = cache_dir / "waveform-detail-v1"
    # platformdirs ignores XDG_CACHE_HOME on Windows; patch the one caller.
    waveform_detail_service._cache_dir = lambda: target
    for live in parity_env.live_cache_dirs():
        if parity_env._inside(waveform_detail_service._cache_dir(), live):
            sys.exit(f"refusing to start: waveform cache {target} is inside the live cache {live}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()

    parity_env.require_sandbox_env(required_rekordbox=True)
    # Absolute paths, so the chdir below cannot move them.
    for name in ("DB_PATH", "USER_DATA_DIR", "PLUMDECK_LOG_DIR", "PLUMDECK_REKORDBOX_DB",
                 "XDG_CACHE_HOME"):
        if os.environ.get(name):
            os.environ[name] = str(Path(os.environ[name]).expanduser().resolve())
    user_data = Path(os.environ["USER_DATA_DIR"])
    os.environ.setdefault("PLUMDECK_LOG_DIR", str(user_data / "logs"))
    parity_env.require_sandbox_env(required_rekordbox=True)  # re-check the final paths
    cache_dir = parity_env.sandbox_cache(user_data.parent / "cache")
    # main.py builds its CORS list from the port, as the sidecar would.
    os.environ["PLUMDECK_PORT"] = str(args.port)
    os.environ.pop("PLUMDECK_MANAGED_SIDECAR", None)
    parity_env.use_backend()
    # config.py reads ./.env when not frozen; never pick up a developer's file.
    os.chdir(user_data.parent if user_data.parent.exists() else "/")

    from startup_progress import report as startup_report

    startup_report("settings", "設定・保存先を読み込んでいます", 1)
    # The values config.py actually resolved, before anything opens them.
    settings = parity_env.require_sandbox_settings()

    settings.setup_environment()
    os.makedirs(settings.USER_DATA_DIR, exist_ok=True)
    startup_report("services", "ライブラリ・解析・再生の機能を読み込んでいます", 2)
    import uvicorn
    from main import app

    install_patches(cache_dir)
    print(f"parity oracle on 127.0.0.1:{args.port} DB_PATH={settings.DB_PATH} "
          f"cache={cache_dir} rekordbox={os.environ['PLUMDECK_REKORDBOX_DB']}", flush=True)
    server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=args.port, reload=False,
                                           workers=1, timeout_graceful_shutdown=5))
    # uvicorn re-raises the stop signal after its graceful shutdown; the
    # lifespan (CHECKPOINT) has already run by then, so exit normally.
    for stop in (signal.SIGINT, signal.SIGTERM):
        signal.signal(stop, lambda *_: None)
    server.run()
    sys.exit(0 if server.started else 1)


if __name__ == "__main__":
    main()
