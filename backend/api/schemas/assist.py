from typing import List, Literal, Optional

from pydantic import BaseModel, ConfigDict, Field


class DeckObservationIn(BaseModel):
    """One deck exactly as the desktop client read it from rekordbox's UI.

    These are observations, not assertions: every field may be missing, and the
    server never treats them as an identity on their own.
    """

    model_config = ConfigDict(extra="forbid")

    slot: int = Field(ge=1, le=4)
    loaded: bool = False
    title: Optional[str] = Field(default=None, max_length=512)
    artist: Optional[str] = Field(default=None, max_length=512)
    track_bpm: Optional[float] = None
    tempo_bpm: Optional[float] = None
    display_key: Optional[str] = Field(default=None, max_length=32)


class DeckResolveRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    decks: List[DeckObservationIn] = Field(default_factory=list, max_length=4)
    # Audio files the rekordbox process has open. Includes sampler and metronome
    # files, so this is a candidate set the server narrows down, not deck state.
    open_audio_paths: List[str] = Field(default_factory=list, max_length=256)


Intent = Literal[
    "keep", "hype", "dance", "calm", "emotional", "bright", "throwback", "wordplay",
    "groove",  # legacy alias of "keep"
]


class AssistFiltersIn(BaseModel):
    """Compound conditions: AND across fields, OR inside a list."""

    model_config = ConfigDict(extra="forbid")

    bpm_min: Optional[float] = Field(default=None, gt=0, le=400)
    bpm_max: Optional[float] = Field(default=None, gt=0, le=400)
    genres: List[str] = Field(default_factory=list, max_length=32)
    subgenres: List[str] = Field(default_factory=list, max_length=32)
    artists: List[str] = Field(default_factory=list, max_length=32)
    keys: List[str] = Field(default_factory=list, max_length=24)
    year_min: Optional[int] = Field(default=None, ge=1900, le=2100)
    year_max: Optional[int] = Field(default=None, ge=1900, le=2100)
    query: str = Field(default="", max_length=200)


class AssistRecommendRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    source_track_id: int
    intent: Intent = "keep"
    # Look for half/double-time partners and titled tempo-changing edits.
    transition: bool = False
    limit: int = Field(default=12, ge=1, le=50)
    exclude_track_ids: List[int] = Field(default_factory=list, max_length=10000)
    genre_scope: Literal["any", "same_genre", "same_subgenre"] = "any"
    genres: Optional[List[str]] = Field(default=None, max_length=32)
    filters: AssistFiltersIn = Field(default_factory=AssistFiltersIn)


class AssistSearchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    filters: AssistFiltersIn = Field(default_factory=AssistFiltersIn)
    limit: int = Field(default=12, ge=1, le=50)
    exclude_track_ids: List[int] = Field(default_factory=list, max_length=10000)


class AssistWindowStateIn(BaseModel):
    """What the assist window is showing, reported so an MCP agent can read it."""

    model_config = ConfigDict(extra="forbid")

    deck_slot: Optional[int] = Field(default=None, ge=1, le=4)
    source_track_id: Optional[int] = None
    intent: Intent = "keep"
    transition: bool = False
    genre_scope: Literal["any", "same_genre", "same_subgenre"] = "any"
    limit: int = Field(default=12, ge=1, le=50)
    filters: AssistFiltersIn = Field(default_factory=AssistFiltersIn)
    exclude_track_ids: List[int] = Field(default_factory=list, max_length=10000)
    shown_track_ids: List[int] = Field(default_factory=list, max_length=100)


__all__ = [
    "AssistFiltersIn",
    "AssistRecommendRequest",
    "AssistSearchRequest",
    "AssistWindowStateIn",
    "DeckObservationIn",
    "DeckResolveRequest",
]
