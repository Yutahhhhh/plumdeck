import json
from typing import Any

from sqlmodel import Session, select

from api.schemas.performance_metadata import (
    PerformanceMetadataWrite,
    RekordboxCueBulkImportRequest,
    RekordboxCueImportRequest,
)
from domain.models.performance_metadata import TrackPerformanceMetadata
from domain.models.track import Track
from infra.database.connection import db_lock
from infra.repositories.performance_metadata_repository import PerformanceMetadataRepository


class PerformanceMetadataNotFoundError(LookupError):
    pass


class PerformanceMetadataConflictError(RuntimeError):
    def __init__(self, current_revision: int):
        super().__init__(f"Performance metadata revision conflict; current revision is {current_revision}")
        self.current_revision = current_revision


class PerformanceMetadataValidationError(ValueError):
    pass


class PerformanceMetadataAppService:
    MAX_BULK_IMPORT_ERRORS = 100

    def __init__(self, session: Session):
        self.session = session
        self.repository = PerformanceMetadataRepository(session)

    def _track(self, track_id: int) -> Track:
        track = self.session.get(Track, track_id)
        if track is None:
            raise PerformanceMetadataNotFoundError(f"Track {track_id} not found")
        return track

    @staticmethod
    def _decode(raw: str | None, fallback: Any) -> Any:
        if not raw:
            return fallback
        try:
            return json.loads(raw)
        except (TypeError, ValueError):
            return fallback

    def _item(self, track_id: int, row: TrackPerformanceMetadata | None) -> dict[str, Any]:
        if row is None:
            return {
                "track_id": track_id,
                "revision": 0,
                "cue_points": [],
                "loops": [],
                "beat_grid": None,
                "created_at": None,
                "updated_at": None,
            }
        return {
            "track_id": row.track_id,
            "revision": row.revision,
            "cue_points": self._decode(row.cue_points_json, []),
            "loops": self._decode(row.loops_json, []),
            "beat_grid": self._decode(row.beat_grid_json, None),
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        }

    @staticmethod
    def _validate_duration(track: Track, request: PerformanceMetadataWrite) -> None:
        duration_ms = float(track.duration or 0) * 1000
        if duration_ms <= 0:
            return
        for cue in request.cue_points:
            if cue.position_ms >= duration_ms:
                raise PerformanceMetadataValidationError(
                    f"cue point slot {cue.slot} must be before the track duration"
                )
        for loop in request.loops:
            if loop.end_ms > duration_ms:
                raise PerformanceMetadataValidationError(
                    f"loop {loop.id} must end within the track duration"
                )
        if request.beat_grid and request.beat_grid.first_beat_ms >= duration_ms:
            raise PerformanceMetadataValidationError(
                "beat_grid.first_beat_ms must be before the track duration"
            )
        if (request.beat_grid and request.beat_grid.beat_times_ms
                and request.beat_grid.beat_times_ms[-1] >= duration_ms):
            raise PerformanceMetadataValidationError(
                "Every beat_grid.beat_times_ms entry must be before the track duration"
            )

    def get(self, track_id: int) -> dict[str, Any]:
        from app.services.grid_candidate_service import GridCandidateService
        track = self._track(track_id)
        item = self._item(track_id, self.repository.get(track_id))
        if item["beat_grid"] is None:
            candidate, warning, pending = GridCandidateService(self.session).automatic(track)
            item["grid_warning"] = warning
            item["grid_pending"] = pending
            if candidate:
                item["beat_grid"] = candidate.model_dump(mode="json")
        return item

    def cue_points_summary(self, track_ids: list[int]) -> dict[int, list[float | None]]:
        """Hot cue positions per track, as an 8 slot array with gaps as null.

        Reads only what is stored: unlike `get`, this never falls back to grid
        analysis, so it stays cheap enough for a track list.
        """
        summary: dict[int, list[float | None]] = {}
        for row in self.repository.get_many(track_ids):
            try:
                cues = json.loads(row.cue_points_json or "[]")
            except json.JSONDecodeError:
                continue
            slots: list[float | None] = [None] * 8
            for cue in cues:
                if not isinstance(cue, dict):
                    continue
                slot, position = cue.get("slot"), cue.get("position_ms")
                if isinstance(slot, int) and 0 <= slot <= 7 and isinstance(position, (int, float)):
                    slots[slot] = float(position)
            if any(position is not None for position in slots):
                summary[row.track_id] = slots
        return summary

    def import_rekordbox_cues(
        self,
        track_id: int,
        request: RekordboxCueImportRequest,
    ) -> dict[str, Any]:
        """Replace only plumdeck's cues from rekordbox, preserving loops and grid."""
        from infra.rekordbox_cues import read_hot_cues

        track = self._track(track_id)
        cues = read_hot_cues(track.filepath)
        # Read the fields that must be preserved and perform the compare/write
        # under one lock. `replace` re-enters this RLock and commits the update.
        with db_lock:
            # The service may have loaded metadata earlier in this session;
            # expire it so the revision comparison observes the committed row.
            self.session.expire_all()
            current = self._item(track_id, self.repository.get(track_id))
            replacement = PerformanceMetadataWrite(
                revision=request.revision,
                cue_points=cues,
                loops=current["loops"],
                beat_grid=current["beat_grid"],
            )
            return self.replace(track_id, replacement)

    def import_all_rekordbox_cues(
        self,
        _request: RekordboxCueBulkImportRequest,
    ) -> dict[str, Any]:
        """Synchronise hot cues for every exact-path rekordbox match."""
        from infra.rekordbox_cues import read_hot_cues_bulk

        # Capture plain snapshots before opening rekordbox. These revisions are
        # the optimistic concurrency boundary for every later per-track write.
        with db_lock:
            self.session.expire_all()
            tracks = list(self.session.exec(select(Track.id, Track.filepath)).all())
            revisions = dict(self.session.exec(select(
                TrackPerformanceMetadata.track_id,
                TrackPerformanceMetadata.revision,
            )).all())
            # End the read transaction before rekordbox I/O so later writes
            # see revisions committed while the source snapshot was read.
            self.session.rollback()

        source = read_hot_cues_bulk([filepath for _, filepath in tracks])
        imported = skipped = failed = conflicts = 0
        all_errors: list[dict[str, Any]] = []

        # This session starts after the source read, then each successful save
        # commits. Reusing it avoids opening one connection per matched track.
        with Session(self.session.get_bind()) as write_session:
            write_service = PerformanceMetadataAppService(write_session)
            for track_id, filepath in tracks:
                source_error = source.errors_by_path.get(filepath)
                if source_error is not None:
                    failed += 1
                    all_errors.append({"track_id": track_id, "message": source_error})
                    continue
                if filepath not in source.cues_by_path:
                    skipped += 1
                    continue

                try:
                    with db_lock:
                        # `save()` refreshes after commit, opening a new DuckDB
                        # read transaction that must end before the next row.
                        write_session.rollback()
                        write_session.expire_all()
                        row = write_service.repository.get(track_id)
                        current_revision = row.revision if row else 0
                        expected_revision = revisions.get(track_id, 0)
                        if current_revision != expected_revision:
                            raise PerformanceMetadataConflictError(current_revision)
                        current = write_service._item(track_id, row)
                        replacement = PerformanceMetadataWrite(
                            revision=expected_revision,
                            cue_points=source.cues_by_path[filepath],
                            loops=current["loops"],
                            beat_grid=current["beat_grid"],
                        )
                        write_service.replace(track_id, replacement)
                    imported += 1
                except PerformanceMetadataConflictError as exc:
                    write_session.rollback()
                    conflicts += 1
                    all_errors.append({"track_id": track_id, "message": str(exc)})
                except Exception as exc:
                    write_session.rollback()
                    failed += 1
                    all_errors.append({"track_id": track_id, "message": str(exc)})

        errors = all_errors[:self.MAX_BULK_IMPORT_ERRORS]
        return {
            "imported": imported,
            "skipped": skipped,
            "failed": failed,
            "conflicts": conflicts,
            "errors": errors,
            "errors_truncated": len(all_errors) - len(errors),
        }

    def replace(self, track_id: int, request: PerformanceMetadataWrite) -> dict[str, Any]:
        # DuckDB does not provide row-level locking. Serialize the compare/write
        # pair (including the first read in this session) so revision checks
        # remain atomic across request sessions.
        with db_lock:
            track = self._track(track_id)
            self._validate_duration(track, request)
            row = self.repository.get(track_id)
            current_revision = row.revision if row else 0
            if request.revision != current_revision:
                raise PerformanceMetadataConflictError(current_revision)
            if row is None:
                row = TrackPerformanceMetadata(track_id=track_id)
            row.cue_points_json = json.dumps(
                [item.model_dump(mode="json") for item in request.cue_points],
                separators=(",", ":"),
            )
            row.loops_json = json.dumps(
                [item.model_dump(mode="json") for item in request.loops],
                separators=(",", ":"),
            )
            row.beat_grid_json = (
                json.dumps(request.beat_grid.model_dump(mode="json"), separators=(",", ":"))
                if request.beat_grid
                else None
            )
            row.revision = current_revision + 1
            self.repository.save(row)
            return self._item(track_id, row)
