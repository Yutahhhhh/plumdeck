from typing import Optional, Dict, Any, List
from datetime import datetime
from sqlmodel import Field, SQLModel
from sqlalchemy import JSON, Column, LargeBinary
from pydantic import ConfigDict
import json

from utils.array_codec import pack_f32, pack_u8_waveform, unpack_f32, unpack_u8_waveform

class Track(SQLModel, table=True):
    __tablename__ = "tracks"
    """
    音楽トラックモデル
    """
    id: Optional[int] = Field(default=None, primary_key=True)
    filepath: str = Field(index=True, unique=True, nullable=False)
    
    # メタデータ
    title: str = Field(index=True)
    artist: str = Field(index=True)
    album: Optional[str] = Field(default="", index=True)
    genre: str
    subgenre: str = Field(default="")
    year: Optional[int] = Field(default=None, index=True)
    
    # 解析データ
    bpm: Optional[float] = None
    key: str = Field(default="")
    scale: str = Field(default="")
    duration: float
    # ``light`` is the bounded analysis profile; ``full`` is detailed analysis.
    # Both can be ready before optional embeddings; NULL denotes legacy full.
    analysis_level: Optional[str] = Field(default=None)
    
    # Basic Audio Features
    energy: float = Field(default=0.0)
    danceability: float = Field(default=0.0)
    loudness: float = Field(default=-60.0)
    brightness: float = Field(default=0.0)
    noisiness: float = Field(default=0.0)
    contrast: float = Field(default=0.0)
    
    # Advanced Features
    loudness_range: float = Field(default=0.0)
    spectral_flux: float = Field(default=0.0)
    spectral_rolloff: float = Field(default=0.0)
    
    is_genre_verified: bool = Field(default=False)
    created_at: datetime = Field(default_factory=datetime.now, index=True)

    # Pydantic V2 形式の Config 設定
    # extra="allow" により、辞書化した後に外部から has_lyrics を注入してもバリデーションエラーになりません
    model_config = ConfigDict(
        arbitrary_types_allowed=True,
        extra="allow" 
    )

class TrackAnalysis(SQLModel, table=True):
    __tablename__ = "track_analyses"
    track_id: int = Field(primary_key=True, foreign_key="tracks.id")
    # v4: 波形/ビートは JSON テキストではなくコンパクトな BLOB で保持する。
    #   - waveform_u8 : 0..1 振幅を 500 点 uint8 にダウンサンプルした生バイト
    #   - beats_f32   : ビート位置(秒) の float32 生バイト
    # 読み書きは waveform_peaks / beat_positions プロパティ (List[float]) 経由で行う。
    beats_f32: Optional[bytes] = Field(default=None, sa_column=Column("beats_f32", LargeBinary))
    waveform_u8: Optional[bytes] = Field(default=None, sa_column=Column("waveform_u8", LargeBinary))
    features_extra_json: str = Field(default="{}")

    @property
    def features_extra(self) -> Dict[str, Any]:
        try:
            return json.loads(self.features_extra_json)
        except:
            return {}

    @property
    def waveform_peaks(self) -> List[float]:
        return unpack_u8_waveform(self.waveform_u8)

    @waveform_peaks.setter
    def waveform_peaks(self, values: Optional[List[float]]) -> None:
        self.waveform_u8 = pack_u8_waveform(values)

    @property
    def beat_positions(self) -> List[float]:
        return unpack_f32(self.beats_f32)

    @beat_positions.setter
    def beat_positions(self, values: Optional[List[float]]) -> None:
        self.beats_f32 = pack_f32(values)

class TrackEmbedding(SQLModel, table=True):
    __tablename__ = "track_embeddings"
    track_id: int = Field(primary_key=True, foreign_key="tracks.id")
    model_name: str = Field(default="musicnn")
    embedding_json: str = Field(default="[]")
    updated_at: datetime = Field(default_factory=datetime.now)
