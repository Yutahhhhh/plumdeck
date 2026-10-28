import math
from datetime import datetime
from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, Field, StrictFloat, StrictInt, field_validator, model_validator


FiniteNumber = Annotated[StrictInt | StrictFloat, Field(ge=0)]


class CuePoint(BaseModel):
    model_config = ConfigDict(extra="forbid")

    slot: StrictInt = Field(ge=0, le=15)
    position_ms: FiniteNumber
    label: str = Field(default="", max_length=100)
    color: str | None = Field(default=None, max_length=32)

    @field_validator("position_ms")
    @classmethod
    def finite_position(cls, value: float) -> float:
        if not math.isfinite(value):
            raise ValueError("must be finite")
        return value


class SavedLoop(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: str = Field(min_length=1, max_length=64)
    start_ms: FiniteNumber
    end_ms: FiniteNumber
    label: str = Field(default="", max_length=100)

    @field_validator("id")
    @classmethod
    def nonblank_id(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("must not be blank")
        return value

    @field_validator("start_ms", "end_ms")
    @classmethod
    def finite_position(cls, value: float) -> float:
        if not math.isfinite(value):
            raise ValueError("must be finite")
        return value

    @model_validator(mode="after")
    def ordered(self) -> "SavedLoop":
        if self.end_ms <= self.start_ms:
            raise ValueError("end_ms must be greater than start_ms")
        return self


class BeatGrid(BaseModel):
    model_config = ConfigDict(extra="forbid")

    bpm: StrictInt | StrictFloat = Field(ge=20, le=300)
    first_beat_ms: FiniteNumber
    beats_per_bar: StrictInt = Field(default=4, ge=1, le=16)
    beat_times_ms: list[FiniteNumber] | None = Field(default=None, min_length=2, max_length=100000)
    beat_numbers: list[Annotated[StrictInt, Field(ge=1, le=16)]] | None = None
    source: Literal["rekordbox", "analysis", "manual"] = "manual"
    # Raw rhythm algorithm confidence; algorithms use different scales.
    # Not a calibrated probability. Unavailable for imported/manual grids.
    confidence: StrictInt | StrictFloat | None = Field(
        default=None, ge=0,
        description="Raw rhythm algorithm confidence; not a calibrated probability",
    )

    @model_validator(mode="after")
    def validate_beats(self) -> "BeatGrid":
        if self.confidence is not None and not math.isfinite(self.confidence):
            raise ValueError("confidence must be finite")
        if self.beat_times_ms is not None:
            if any(not math.isfinite(t) for t in self.beat_times_ms):
                raise ValueError("beat times must be finite")
            if any(b <= a for a, b in zip(self.beat_times_ms, self.beat_times_ms[1:])):
                raise ValueError("beat times must be strictly increasing")
            if abs(self.first_beat_ms - self.beat_times_ms[0]) > 0.01:
                raise ValueError("first_beat_ms must match the first beat timestamp")
        if self.beat_numbers is not None:
            if self.beat_times_ms is None or len(self.beat_numbers) != len(self.beat_times_ms):
                raise ValueError("beat_numbers must match beat_times_ms length")
            if any(n > self.beats_per_bar for n in self.beat_numbers):
                raise ValueError("beat numbers must fit beats_per_bar")
        return self

    @field_validator("bpm", "first_beat_ms")
    @classmethod
    def finite_number(cls, value: float) -> float:
        if not math.isfinite(value):
            raise ValueError("must be finite")
        return value


class PerformanceMetadataWrite(BaseModel):
    """Complete replacement payload. Revision 0 creates the initial record."""

    model_config = ConfigDict(extra="forbid")

    revision: StrictInt = Field(ge=0)
    cue_points: list[CuePoint] = Field(default_factory=list, max_length=16)
    loops: list[SavedLoop] = Field(default_factory=list, max_length=32)
    beat_grid: BeatGrid | None = None

    @model_validator(mode="after")
    def unique_keys(self) -> "PerformanceMetadataWrite":
        slots = [cue.slot for cue in self.cue_points]
        if len(slots) != len(set(slots)):
            raise ValueError("cue point slots must be unique")
        loop_ids = [loop.id for loop in self.loops]
        if len(loop_ids) != len(set(loop_ids)):
            raise ValueError("loop ids must be unique")
        return self


class PerformanceMetadataRead(BaseModel):
    track_id: int
    revision: int
    cue_points: list[CuePoint]
    loops: list[SavedLoop]
    beat_grid: BeatGrid | None
    created_at: datetime | None
    updated_at: datetime | None
    grid_warning: str | None = None
    # A missing grid is being analyzed in the background; GET again for it.
    grid_pending: bool = False


class GridAnalysisRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    force: bool = False


class GridBatchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    only_outdated: bool = True
    track_ids: list[StrictInt] | None = Field(default=None, max_length=100000)


class RekordboxCueImportRequest(BaseModel):
    """Expected plumdeck revision for a source-to-owned-metadata import."""

    model_config = ConfigDict(extra="forbid")
    revision: StrictInt = Field(ge=0)


class RekordboxCueBulkImportRequest(BaseModel):
    """Whole-library import request; reserved for future import options."""

    model_config = ConfigDict(extra="forbid")


class RekordboxCueImportError(BaseModel):
    track_id: int
    message: str


class RekordboxCueBulkImportRead(BaseModel):
    imported: int
    skipped: int
    failed: int
    conflicts: int
    errors: list[RekordboxCueImportError]
    errors_truncated: int = 0


class CueSlotsRequest(BaseModel):
    """Track ids to summarise. Capped so a stray caller cannot scan the library."""

    track_ids: list[int] = Field(default_factory=list, max_length=500)
