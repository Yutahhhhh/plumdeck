"""Assist-mode endpoints.

Every route here is read-only with respect to both libraries: nothing is written
to the plumdeck database and the rekordbox collection is only ever opened
read-only. The window/agent routes only touch the in-memory agent bridge.
"""
from fastapi import APIRouter, Depends, HTTPException
from sqlmodel import Session

from api.schemas.assist import (
    AssistRecommendRequest,
    AssistRouteRequest,
    AssistSearchRequest,
    AssistWindowStateIn,
    DeckResolveRequest,
)
from app.services.assist_app_service import AssistAppService, AssistInputError
from app.services.assist_bridge import assist_bridge
from infra.database.connection import get_session

router = APIRouter(prefix="/api/assist", tags=["assist"])


@router.get("/library-status")
def library_status(session: Session = Depends(get_session)):
    """Whether the local rekordbox collection can be read right now."""
    return AssistAppService(session).library_status()


@router.post("/decks/resolve")
def resolve_decks(payload: DeckResolveRequest, session: Session = Depends(get_session)):
    """Identify which library track each observed deck is actually holding."""
    return AssistAppService(session).resolve_decks(
        [deck.model_dump() for deck in payload.decks],
        payload.open_audio_paths,
    )


@router.post("/recommendations")
def recommendations(payload: AssistRecommendRequest, session: Session = Depends(get_session)):
    try:
        return AssistAppService(session).recommend(
            source_track_id=payload.source_track_id,
            intent=payload.intent,
            transition=payload.transition,
            genre_scope=payload.genre_scope,
            limit=payload.limit,
            exclude_track_ids=payload.exclude_track_ids,
            genres=payload.genres,
            filters=payload.filters.model_dump(),
        )
    except AssistInputError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
    except ValueError as error:
        raise HTTPException(status_code=404, detail=str(error)) from error


@router.post("/routes")
def routes(payload: AssistRouteRequest, session: Session = Depends(get_session)):
    """Find short, playable routes to an exact destination track."""
    try:
        return AssistAppService(session).route(
            source_track_id=payload.source_track_id,
            target_track_id=payload.target_track_id,
            intent=payload.intent,
            transition=payload.transition,
            max_intermediate=payload.max_intermediate,
            limit=payload.limit,
            exclude_track_ids=payload.exclude_track_ids,
            genre_scope=payload.genre_scope,
            filters=payload.filters.model_dump(),
        )
    except AssistInputError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
    except ValueError as error:
        raise HTTPException(status_code=404, detail=str(error)) from error


@router.post("/search")
def search(payload: AssistSearchRequest, session: Session = Depends(get_session)):
    """Compound search without a source deck, limited to draggable originals."""
    try:
        return AssistAppService(session).search(
            payload.filters.model_dump(), payload.limit, payload.exclude_track_ids,
        )
    except ValueError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error


@router.put("/window")
def report_window(payload: AssistWindowStateIn):
    """The assist window's current view, readable by MCP agents."""
    assist_bridge.report_window(payload.model_dump())
    return {"ok": True}


@router.get("/agent")
def agent_updates():
    """Settings and a track list an MCP agent has asked the window to show."""
    return assist_bridge.pending()


@router.delete("/agent/list")
def dismiss_agent_list():
    assist_bridge.dismiss_list()
    return {"dismissed": True}
