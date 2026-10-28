from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from contextlib import asynccontextmanager
from infra.database.connection import init_db, close_db, checkpoint_db
from api.routers import (
    assist,
    filesystem,
    genres,
    ingest,
    setlists,
    settings as settings_router,
    system,
    tracks,
    lyrics,
    metadata,
    mcp_info,
    wordplay,
    performance_metadata,
    play,
    waveform_detail,
    workflows,
)
from mcp_server.server import mcp_app_holder
from mcp_server.instance import mcp as mcp_server

from config import settings
from app.services.analysis_job_service import analysis_job_service
import asyncio
import os
from startup_progress import report as startup_report

# Lifespan event to handle startup/shutdown
@asynccontextmanager
async def lifespan(app: FastAPI):
    startup_report("database", "楽曲データベースを準備しています", 3)
    init_db()
    startup_report("queue", "前回の解析・取り込み状態を復元しています", 4)
    # A process crash cannot resume an in-flight inference. Keep the target
    # intent and offer an explicit resume instead of leaving a permanent spinner.
    from sqlmodel import Session
    from sqlalchemy import text
    import infra.database.connection as db_connection
    with Session(db_connection.engine) as session:
        session.exec(text("UPDATE import_items SET state='queued' WHERE state IN ('probing','analyzing')"))
        session.exec(text("UPDATE import_batches SET state='paused',paused=true WHERE state IN ('queued','processing','pausing')"))
        session.commit()
    # DuckDBの初期化 (Raw SQLによるSequence/Table作成)
    # マウントされたサブアプリは lifespan イベントを直接受け取らないため、
    # MCP の session_manager は親アプリの lifespan 内で明示的に起動する。
    # session_manager は run() 完了後に再利用できないため、lifespan のたびに作り直す。
    analysis_job_service.startup()
    analysis_job_service.enqueue_missing_embeddings()
    mcp_app_holder.refresh()
    async with mcp_server.session_manager.run():
        startup_report("ready", "起動処理が完了しました。接続を確認しています", 5)
        yield
    await asyncio.to_thread(analysis_job_service.shutdown)
    checkpoint_db()  # WAL を本体へ畳み込む (肥大抑制の補助)
    close_db() # 終了時にDB接続を閉じる

app = FastAPI(title="plumdeck Backend API", lifespan=lifespan)

# CORS Configuration
origins = [
    f"http://localhost:{settings.FRONTEND_PORT}", # Tauri Dev Server
    f"http://127.0.0.1:{settings.FRONTEND_PORT}", # Tauri Dev Server (IP)
    f"http://localhost:{settings.PLUMDECK_PORT}",    # Dynamic Port
    f"http://127.0.0.1:{settings.PLUMDECK_PORT}",    # Dynamic Port
    "tauri://localhost",                          # Tauri Production (macOS)
    "http://tauri.localhost",                     # Tauri default (Windows)
    "https://tauri.localhost",                    # Tauri Production (Windows/Linux)
]

app.add_middleware(
    CORSMiddleware,
    allow_origins=origins,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Root endpoint for health check
@app.get("/")
async def root():
    return {"message": "plumdeck Backend API is running"}

# Include Routers
app.include_router(filesystem.router)
app.include_router(genres.router)
app.include_router(ingest.router)
app.include_router(setlists.router)
app.include_router(settings_router.router)
app.include_router(system.router)
app.include_router(tracks.router)
app.include_router(lyrics.router)
app.include_router(metadata.router)
app.include_router(mcp_info.router)
app.include_router(wordplay.router)
app.include_router(performance_metadata.router)
app.include_router(play.router)
app.include_router(waveform_detail.router)
app.include_router(assist.router)
app.include_router(workflows.router)

# MCP サーバーを /mcp にマウント (外部の MCP クライアントが Streamable HTTP で接続する)
app.mount("/", mcp_app_holder)
