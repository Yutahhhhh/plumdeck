"""Run the Python backend as the parity oracle (side A).

    DB_PATH=/sandbox/a/plumdeck.duckdb USER_DATA_DIR=/sandbox/a/ud \\
        python tools/parity/oracle_server.py --port 18001

Starts backend/main.py's FastAPI app under uvicorn exactly as server.py does
(127.0.0.1, one worker, 5 s graceful shutdown; SIGINT/SIGTERM run the lifespan
shutdown, which CHECKPOINTs the DB), without server.py's sidecar stdin handling.

Refuses to start unless DB_PATH and USER_DATA_DIR are set and resolve outside
the live data dir (~/Library/Application Support/plumdeck on macOS).
Later slices add their monkeypatches (fixture analyzer, stub URLs, fake
command runner) in `install_patches` before the app is imported.
"""
from __future__ import annotations

import argparse
import os
import signal
import sys
from pathlib import Path

import parity_env


def install_patches() -> None:
    """Determinism hooks for later slices; S0 compares only `/` and 404s."""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()

    parity_env.require_sandbox_env()
    # Absolute paths, so the chdir below cannot move them.
    for name in ("DB_PATH", "USER_DATA_DIR", "PLUMDECK_LOG_DIR"):
        if os.environ.get(name):
            os.environ[name] = str(Path(os.environ[name]).expanduser().resolve())
    user_data = Path(os.environ["USER_DATA_DIR"])
    os.environ.setdefault("PLUMDECK_LOG_DIR", str(user_data / "logs"))
    parity_env.require_sandbox_env()  # re-check the final paths
    # main.py builds its CORS list from the port, as the sidecar would.
    os.environ["PLUMDECK_PORT"] = str(args.port)
    os.environ.pop("PLUMDECK_MANAGED_SIDECAR", None)
    parity_env.use_backend()
    # config.py reads ./.env when not frozen; never pick up a developer's file.
    os.chdir(user_data.parent if user_data.parent.exists() else "/")

    from startup_progress import report as startup_report

    startup_report("settings", "設定・保存先を読み込んでいます", 1)
    from config import settings

    settings.setup_environment()
    os.makedirs(settings.USER_DATA_DIR, exist_ok=True)
    install_patches()
    startup_report("services", "ライブラリ・解析・再生の機能を読み込んでいます", 2)
    import uvicorn
    from main import app

    print(f"parity oracle on 127.0.0.1:{args.port} DB_PATH={settings.DB_PATH}", flush=True)
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
