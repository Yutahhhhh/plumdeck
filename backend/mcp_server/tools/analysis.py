"""Audio analysis is a local durable job, independent of the MCP connection."""
from typing import List, Optional
from mcp_server.instance import mcp
from mcp.server.mcpserver.exceptions import ToolError
from app.services.analysis_job_service import analysis_job_service


def _call(method, *args):
    try:
        return method(*args)
    except ValueError as exc:
        raise ToolError(str(exc)) from exc


@mcp.tool()
def plan_track_analysis(track_ids: Optional[List[int]] = None, genres: Optional[List[str]] = None,
                        features: Optional[List[str]] = None, only_outdated: bool = True,
                        limit: Optional[int] = None) -> dict:
    """解析対象と件数を確認する（楽曲は変更しない）。features: embedding（音響類似度、既定）, rhythm（BPM/ビート）, key, timbre（Energy等）, waveform。
    track_ids未指定ならライブラリ全体。genresで絞り込み可能。only_outdatedで現行解析済みを除外。
    既存セットリストの曲を優先する。まず少数曲をlimitで処理し、速度を確認できる。
    """
    return _call(analysis_job_service.plan, track_ids, genres, features, only_outdated, limit)


@mcp.tool()
def start_track_analysis(track_ids: Optional[List[int]] = None, genres: Optional[List[str]] = None,
                         features: Optional[List[str]] = None, only_outdated: bool = True,
                         limit: Optional[int] = None, workers: int = 2) -> dict:
    """選択した音響項目だけをDBへ再解析する永続ジョブを開始し、すぐにjob idを返す。
    features: embedding（既定）, rhythm, key, timbre, waveform。メタデータ・ジャンル・歌詞・音源ファイルは変更しない。
    BPMは 手動（CSV）> rekordbox > 解析結果 の順で保存する（キーも、設定 rekordbox_key_source がオンなら同じ）。
    rhythmだけのジョブはグリッドの更新で、rekordboxのグリッドがある曲を除外し、既存のBPMは変えない。
    only_outdated=Trueで処理済みをスキップ。workersは1〜4（既定2）。MCP切断後もアプリ内で継続する。
    アプリ終了後はresume_track_analysisで再開する。進捗はget_track_analysis_statusで確認。
    """
    return _call(analysis_job_service.start, track_ids, genres, features, only_outdated, limit, workers)


@mcp.tool()
def get_track_analysis_status(job_id: Optional[str] = None) -> dict:
    """解析ジョブの進捗、残り時間、処理中の曲、失敗理由を返す。job_id省略時は最新ジョブ。"""
    return _call(analysis_job_service.status, job_id)


@mcp.tool()
def pause_track_analysis(job_id: str) -> dict:
    """処理中の曲を保存してから解析を一時停止する。結果と未処理キューは保持し、後で再開可能。"""
    return _call(analysis_job_service.pause, job_id)


@mcp.tool()
def resume_track_analysis(job_id: str, workers: Optional[int] = None, retry_failed: bool = False) -> dict:
    """解析ジョブの未処理分を再開する。workersで負荷を変更可能。retry_failed=Trueなら失敗曲も再試行する。"""
    return _call(analysis_job_service.resume, job_id, workers, retry_failed)
