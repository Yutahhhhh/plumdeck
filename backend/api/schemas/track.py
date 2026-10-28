from typing import Literal, Optional
from datetime import datetime
from sqlmodel import SQLModel

class TrackRead(SQLModel):
    id: int
    filepath: str
    title: str
    artist: str
    album: Optional[str] = ""
    genre: str
    subgenre: str = ""
    year: Optional[int] = None
    duration: float
    bpm: Optional[float] = None
    is_analyzed: Optional[bool] = None
    analysis_level: Optional[Literal["light", "full"]] = None
    
    key: str
    scale: str
    energy: float
    danceability: float
    loudness: float
    brightness: float
    noisiness: float
    contrast: float
    
    loudness_range: float
    spectral_flux: float
    spectral_rolloff: float
    
    lyrics: Optional[str] = None
    has_lyrics: bool = False
    is_genre_verified: bool = False
    
    created_at: datetime
