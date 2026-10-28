from fastapi import APIRouter, Depends, HTTPException
from sqlmodel import Session, select, text
from sqlalchemy import func
import duckdb
from typing import Dict, Any, List
from pydantic import BaseModel
from infra.database.connection import get_session, get_setting_value
from models import Track, Setlist, Lyrics

import subprocess
import platform
import os

router = APIRouter()

class SaveFileRequest(BaseModel):
    path: str
    content: str

class RevealFileRequest(BaseModel):
    path: str

@router.post("/api/system/save-file")
def save_file_to_disk(req: SaveFileRequest):
    """
    指定されたパスにファイル内容を保存する。
    Tauri等のクライアントからローカルファイルシステムへの書き込みを代行するために使用。
    """
    try:
        # セキュリティチェック: 必要であればパスの検証を行う（例: 特定のディレクトリ以下のみ許可など）
        # 今回はローカルツールなので制限なしとするが、本番環境では注意が必要
        with open(req.path, "w", encoding="utf-8") as f:
            f.write(req.content)
        return {"status": "success"}
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to save file: {str(e)}")

@router.post("/api/system/reveal-file")
def reveal_file_in_os(req: RevealFileRequest):
    """
    指定されたパスのファイルをOSのファイルマネージャーで表示する。
    """
    path = req.path
    if not os.path.exists(path):
         # ファイルがない場合は親ディレクトリを開くなどのフォールバックも考えられるが、一旦エラーログのみ
         print(f"File not found for reveal: {path}")
         return {"status": "error", "detail": "File not found"}
    
    system_name = platform.system()
    try:
        if system_name == "Darwin": # macOS
            subprocess.run(["open", "-R", path], check=True)
        elif system_name == "Windows":
            from utils.reveal_file import reveal_windows_file
            reveal_windows_file(path)
        elif system_name == "Linux":
            subprocess.run(["xdg-open", os.path.dirname(path)], check=True)
        return {"status": "success"}
    except Exception as e:
        print(f"Failed to reveal file: {e}")
        return {"status": "error", "detail": str(e)}

@router.get("/api/")
def health_check(session: Session = Depends(get_session)):
    db_version = duckdb.__version__

    return {
        "status": "ok",
        "duckdb_version": db_version,
        "ai_runtime": "mcp-client"
    }

@router.get("/api/dashboard")
def get_dashboard_stats(session: Session = Depends(get_session)):
    """
    ダッシュボード表示用の統計情報を一括取得する。
    DuckDBの集計機能を使用して高速に処理を行う。
    """
    
    # 1. Basic Counts
    from utils.ingestion import completed_analysis_condition
    total_tracks = session.exec(select(func.count()).select_from(Track)).one()
    analyzed_tracks = session.exec(select(func.count()).select_from(Track).where(completed_analysis_condition())).one()
    unanalyzed_tracks = total_tracks - analyzed_tracks
    
    # 2. Genre Distribution (All)
    # 大文字小文字を区別せず集計
    genre_query = text("""
        SELECT 
            genre, 
            COUNT(*) as count 
        FROM tracks 
        WHERE genre IS NOT NULL AND genre != 'Unknown' AND genre != ''
        GROUP BY genre 
        ORDER BY count DESC
    """)
    genre_rows = session.connection().execute(genre_query).fetchall()
    
    genre_distribution = [
        {"name": row[0], "count": row[1]} for row in genre_rows
    ]

    # 3. Unverified Genres Count
    unverified_count = session.exec(
        select(func.count()).select_from(Track).where(Track.is_genre_verified == False)
    ).one()

    # 4. Lyrics Count
    lyrics_count = session.exec(
        select(func.count()).select_from(Lyrics).where(Lyrics.content != None, Lyrics.content != "")
    ).one()

    # 5. Recent Setlists
    recent_setlists = session.exec(
        select(Setlist).order_by(Setlist.updated_at.desc()).limit(5)
    ).all()

    # 6. System Config Check
    root_path = get_setting_value(session, "root_path", "")
    return {
        "total_tracks": total_tracks,
        "analyzed_tracks": analyzed_tracks,
        "unanalyzed_tracks": unanalyzed_tracks,
        "genre_distribution": genre_distribution,
        "unverified_genres_count": unverified_count,
        "lyrics_tracks_count": lyrics_count,
        "recent_setlists": recent_setlists,
        "config": {
            "has_root_path": bool(root_path)
        }
    }
