import asyncio
from fastapi import APIRouter, HTTPException, WebSocket, WebSocketDisconnect
from fastapi.encoders import jsonable_encoder
from starlette.concurrency import run_in_threadpool
from api.schemas.common import IngestRequest
from app.services.ingestion_app_service import ingestion_app_service as ingestion_manager

router = APIRouter()


def import_queue_snapshot():
    # A fresh, short transaction sees committed progress without holding a
    # connection throughout audio inference or the lifetime of the socket.
    from sqlmodel import Session
    import infra.database.connection as db
    from app.services.play_import_service import PlayImportService
    with Session(db.engine) as session:
        service = PlayImportService(session)
        active = service.list(active=True)
        active_ids = {row["id"] for row in active}
        recent = [row for row in service.list() if row["id"] not in active_ids][:20]
        return jsonable_encoder({"type": "import_queue", "batches": active + recent})


@router.websocket("/ws/play-imports")
async def websocket_play_imports(websocket: WebSocket):
    await websocket.accept()
    try:
        while True:
            # Send an initial snapshot and periodic snapshots, including while
            # one long-running file has no new progress. Reconnect is lossless.
            await websocket.send_json(await run_in_threadpool(import_queue_snapshot))
            await asyncio.sleep(2)
    except (WebSocketDisconnect, OSError, RuntimeError):
        pass

@router.post("/api/ingest")
async def ingest_files(req: IngestRequest):
    """
    Start ingestion background task.
    Returns immediately. Client should monitor progress via WebSocket.
    """
    from app.services.analysis_job_service import analysis_job_service
    if analysis_job_service.is_running and not analysis_job_service.is_background_running:
        return {"status": "error", "message": "Audio analysis is running; pause it before importing"}
    if ingestion_manager.is_running:
        return {"status": "error", "message": "Ingestion already running"}
    
    # バックグラウンドタスク開始
    success = await ingestion_manager.start_ingestion(
        req.targets, req.force_update, req.analysis_profile
    )
    
    if success:
        return {
            "status": "success",
            "message": "Ingestion started",
            "state": jsonable_encoder(ingestion_manager.state),
        }
    else:
        # Busy analysis slots now queue; only maintenance (backup/restore) refuses a start.
        raise HTTPException(
            status_code=409,
            detail="バックアップ・復元などの処理中のため解析を開始できません。完了後に再試行してください",
        )


@router.get("/api/ingest/status")
async def ingestion_status():
    return jsonable_encoder({
        **ingestion_manager.state,
        "is_running": ingestion_manager.is_running,
    })

@router.post("/api/ingest/cancel")
async def cancel_ingest():
    """
    Cancel running ingestion task.
    """
    await ingestion_manager.cancel_ingestion()
    return {
        "status": "success",
        "message": "Ingestion cancelled",
        "state": jsonable_encoder(ingestion_manager.state),
    }

@router.websocket("/ws/ingest")
async def websocket_ingest(websocket: WebSocket):
    """
    WebSocket Endpoint for monitoring ingestion progress.
    Just connects to the manager and listens for broadcasts.
    """
    try:
        await ingestion_manager.connect(websocket)
        while True:
            # クライアントからのメッセージは現状不要だが、切断検知のために待機
            # 必要であれば "cancel" などのコマンドを受け付けることも可能
            await websocket.receive_text()
    except WebSocketDisconnect:
        ingestion_manager.disconnect(websocket)
    except Exception as e:
        # print(f"WS Error: {e}")
        ingestion_manager.disconnect(websocket)
