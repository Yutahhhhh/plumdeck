from pydantic import BaseModel
from typing import List, Optional, Dict, Any

class CsvImportRow(BaseModel):
    """CSVの1行に対応するモデル"""
    filepath: str
    title: str
    artist: str
    album: Optional[str] = ""
    genre: Optional[str] = ""
    subgenre: Optional[str] = ""
    year: Optional[int] = None
    bpm: Optional[float] = 0.0
    key: Optional[str] = ""
    energy: Optional[float] = 0.0
    danceability: Optional[float] = 0.0
    brightness: Optional[float] = 0.0
    loudness: Optional[float] = 0.0
    noisiness: Optional[float] = 0.0
    contrast: Optional[float] = 0.0
    duration: Optional[float] = 0.0
    
    # Extended Features
    loudness_range: Optional[float] = 0.0
    spectral_flux: Optional[float] = 0.0
    spectral_rolloff: Optional[float] = 0.0
    bpm_confidence: Optional[float] = 0.0
    key_strength: Optional[float] = 0.0
    bpm_raw: Optional[float] = 0.0
    
    # Large Data (JSON Lists)
    beat_positions: Optional[List[float]] = []
    waveform_peaks: Optional[List[float]] = []

class ImportAnalysisResult(BaseModel):
    """
    CSVインポート時の解析結果（ドライラン結果）。
    ユーザーに確認を求めるために使用する。
    """
    total_rows: int
    new_tracks: List[CsvImportRow]             # 新規追加候補
    duplicates: List[CsvImportRow]             # 完全一致（パスもメタデータも同じ）-> 基本スキップ
    path_updates: List[Dict[str, Any]]         # パス変更候補 { "old_path":..., "new_path":..., "track":... }
    
class ImportExecuteRequest(BaseModel):
    """
    ユーザー確認後の実行リクエスト。
    duplicatesは通常含めない（スキップ扱い）。
    """
    new_tracks: List[CsvImportRow]
    path_updates: List[Dict[str, Any]]

class MetadataImportRow(BaseModel):
    """メタデータ更新用CSVの行モデル"""
    filepath: str
    title: Optional[str] = None
    artist: Optional[str] = None
    album: Optional[str] = None
    genre: Optional[str] = None
    subgenre: Optional[str] = None
    year: Optional[int] = None
    # Recorded as manual values: rekordbox and re-analysis keep them.
    bpm: Optional[float] = None
    key: Optional[str] = None
    is_genre_verified: Optional[bool] = None

class MetadataImportAnalysisResult(BaseModel):
    """メタデータインポート解析結果"""
    total_rows: int
    updates: List[Dict[str, Any]]  # { "current": Track, "new": MetadataImportRow }
    not_found: List[MetadataImportRow]

class MetadataImportExecuteRequest(BaseModel):
    """メタデータインポート実行リクエスト"""
    updates: List[Dict[str, Any]] # { "filepath": str, "data": MetadataImportRow }
