"""Assist for MCP agents: read and drive plumdeck's assist window.

A CLI or agent app (Claude Code, Codex…) reads what the DJ is looking at with
assist_get_state, ranks with the same engine the window uses (assist_recommend /
assist_search), and updates the window either by changing its settings
(assist_apply_settings — plumdeck re-ranks) or by putting its own picks on
screen (assist_show_tracks). plumdeck does not reason about requests; the agent
calling these tools does.
"""
from typing import Any, Dict, List, Optional

from mcp_server.instance import mcp, db_session, serialize
from app.services.assist_app_service import AssistAppService
from app.services.assist_bridge import assist_bridge
from domain.services import assist_recommendation as scoring
from domain.services.assist_filters import AssistFilters

_BRIEF_FIELDS = (
    "id", "title", "artist", "bpm", "key", "genre", "subgenre", "year",
    "energy", "danceability", "brightness", "score", "summary",
)
MAX_SHOWN_TRACKS = 50
_GENRE_SCOPES = ("any", "same_genre", "same_subgenre")


def _brief(candidate: Dict[str, Any]) -> Dict[str, Any]:
    brief = {name: candidate.get(name) for name in _BRIEF_FIELDS if name in candidate}
    brief["reasons"] = [reason.get("text") for reason in candidate.get("reasons") or []]
    if candidate.get("like"):
        brief["like"] = candidate["like"].get("text")
    if candidate.get("wordplay"):
        brief["wordplay_keyword"] = candidate["wordplay"].get("keyword")
    return brief


def _window_state() -> Dict[str, Any]:
    return assist_bridge.window().get("state") or {}


def _reference(like_track_id, like_artist) -> Optional[Dict[str, Any]]:
    if like_track_id is not None:
        return {"kind": "track", "track_id": int(like_track_id)}
    if like_artist:
        return {"kind": "artist", "artist": like_artist}
    return None


def _track_summary(track_id: Optional[int]) -> Optional[Dict[str, Any]]:
    if not track_id:
        return None
    with db_session() as session:
        track = AssistAppService(session).tracks.get_by_id(track_id)
        if track is None:
            return None
        payload = serialize(track)
    return {name: payload.get(name) for name in (
        "id", "title", "artist", "bpm", "key", "genre", "subgenre", "year",
        "energy", "danceability", "brightness",
    )}


@mcp.tool()
def assist_get_state() -> Dict[str, Any]:
    """What plumdeck's assist window is showing right now.

    Returns whether the window is open, the source deck track (the track the DJ
    is mixing out of), the selected preset (intent), transition mode, genre
    scope, compound filters, how many loaded tracks are excluded, the track ids
    currently listed, and any list you already put on screen. Call this first:
    assist_recommend / assist_search default to these settings.
    """
    window = assist_bridge.window()
    state = dict(window.get("state") or {})
    exclusions = state.pop("exclude_track_ids", []) or []
    return {
        "window_open": window["open"],
        "reported_seconds_ago": window["reported_seconds_ago"],
        "source_track": _track_summary(state.get("source_track_id")),
        "settings": {**state, "excluded_track_count": len(exclusions)},
        "presets": {name: f"{preset.label}（{preset.detail}）" for name, preset in scoring.PRESETS.items()},
        "agent_list": window.get("agent_list"),
    }


@mcp.tool()
def assist_recommend(
    source_track_id: Optional[int] = None,
    intent: Optional[str] = None,
    transition: Optional[bool] = None,
    genre_scope: Optional[str] = None,
    limit: Optional[int] = None,
    filters: Optional[Dict[str, Any]] = None,
    like_track_id: Optional[int] = None,
    like_artist: Optional[str] = None,
) -> Dict[str, Any]:
    """Rank next-track candidates with the assist window's engine. Read-only.

    Every omitted argument comes from the window's current state (source deck,
    loaded-track exclusions, preset, filters…). intent (which way to move the
    floor): keep | hype | dance | calm | emotional | bright | throwback |
    wordplay. transition=true looks for half/double-time partners and titled
    tempo-changing edits ("100-128 Transition"); read the returned `caveats`.
    genre_scope: any | same_genre | same_subgenre. filters: {bpm_min, bpm_max,
    genres[], subgenres[], artists[], keys[] (Camelot or "A minor"), year_min,
    year_max, query} — AND across fields, OR within a list.
    like_track_id / like_artist: rough numeric "sounds like X" evidence blended
    into the order. It measures sound, not culture or context: use it as
    material for your own judgement. Only tracks rekordbox knows and that exist
    on disk are returned. Without a source track this becomes assist_search.
    Nothing changes on screen: use assist_show_tracks or assist_apply_settings.
    """
    state = _window_state()
    source = source_track_id if source_track_id is not None else state.get("source_track_id")
    chosen_filters = filters if filters is not None else state.get("filters") or {}
    chosen_limit = max(1, min(int(limit or state.get("limit") or 12), 50))
    excluded = list(state.get("exclude_track_ids") or [])
    reference = _reference(like_track_id, like_artist)
    with db_session() as session:
        service = AssistAppService(session)
        if not source:
            result = service.search(chosen_filters, chosen_limit, excluded, reference=reference)
            result["candidates"] = [_brief(c) for c in result["candidates"]]
            result["mode"] = "search"
            return result
        result = service.recommend(
            source_track_id=int(source),
            intent=intent or state.get("intent") or "keep",
            transition=transition if transition is not None else bool(state.get("transition")),
            limit=chosen_limit,
            exclude_track_ids=excluded,
            genre_scope=genre_scope or state.get("genre_scope") or "any",
            filters=chosen_filters,
            reference=reference,
        )
    result["candidates"] = [_brief(c) for c in result["candidates"]]
    result["alternatives"] = [
        {"intent": item["intent"], "label": item["label"], "track": _brief(item["track"])}
        for item in result["alternatives"]
    ]
    result["mode"] = "recommend"
    return result


@mcp.tool()
def assist_search(
    filters: Optional[Dict[str, Any]] = None,
    limit: Optional[int] = None,
    like_track_id: Optional[int] = None,
    like_artist: Optional[str] = None,
) -> Dict[str, Any]:
    """Search without a source deck: compound filters (same shape as
    assist_recommend) and/or "sounds like X" (like_track_id / like_artist).

    Only tracks rekordbox knows and that exist on disk are returned, excluding
    the tracks the DJ has already loaded. Read-only.
    """
    state = _window_state()
    chosen_filters = filters if filters is not None else state.get("filters") or {}
    chosen_limit = max(1, min(int(limit or state.get("limit") or 12), 50))
    with db_session() as session:
        result = AssistAppService(session).search(
            chosen_filters, chosen_limit, list(state.get("exclude_track_ids") or []),
            reference=_reference(like_track_id, like_artist),
        )
    result["candidates"] = [_brief(c) for c in result["candidates"]]
    return result


@mcp.tool()
def assist_apply_settings(
    intent: Optional[str] = None,
    transition: Optional[bool] = None,
    genre_scope: Optional[str] = None,
    limit: Optional[int] = None,
    filters: Optional[Dict[str, Any]] = None,
    agent_name: Optional[str] = None,
) -> Dict[str, Any]:
    """Change the assist window's settings; plumdeck then re-ranks its own list.

    Omitted arguments are left as they are. `filters` replaces the window's
    conditions entirely (pass {} to clear them). Use this when the DJ's request
    maps onto a preset and conditions ("もっと盛り上げたい、2000年代で" →
    intent="hype", filters={"year_min": 2000, "year_max": 2009}). Pass
    `agent_name` (e.g. "Claude Code", "Codex") so the DJ sees who changed it.
    """
    if intent is not None:
        intent = scoring.resolve_intent(intent)
    if genre_scope is not None and genre_scope not in _GENRE_SCOPES:
        raise ValueError(f"genre_scope must be one of {', '.join(_GENRE_SCOPES)}")
    settings: Dict[str, Any] = {
        "intent": intent,
        "transition": transition,
        "genre_scope": genre_scope,
        "limit": max(1, min(int(limit), 50)) if limit is not None else None,
    }
    if filters is not None:
        # Validate now, so the window never receives conditions it cannot apply.
        settings["filters"] = AssistFilters.from_dict(filters).to_dict()
    applied = assist_bridge.apply_settings(settings, agent_name)
    window = assist_bridge.window()
    return {
        "applied": applied,
        "window_open": window["open"],
        "hint": None if window["open"] else "The assist window is not open; it will apply these when opened.",
    }


@mcp.tool()
def assist_show_tracks(
    tracks: List[Dict[str, Any]],
    title: Optional[str] = None,
    note: Optional[str] = None,
    agent_name: Optional[str] = None,
) -> Dict[str, Any]:
    """Put your own picks on the assist window, ready to drag onto a deck.

    `tracks`: up to 50, in order, each {"track_id": int, "reason": str} — a
    short Japanese reason the DJ reads next to the track. `title`: a short
    heading ("90年代R&Bで落ち着かせる"). `note`: optional one or two lines.
    Each pick is re-checked: tracks rekordbox does not know, missing files or
    the deck's own track are dropped and returned under `rejected`. Kept picks
    also show plumdeck's own tempo/key/energy reasons against the source deck,
    measured with the window's current preset. Replaces any list you showed
    before; assist_clear_tracks removes it.
    """
    state = _window_state()
    picks = []
    for item in (tracks or [])[:MAX_SHOWN_TRACKS]:
        try:
            picks.append((int(item["track_id"]), str(item.get("reason") or "").strip()[:400]))
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError("each track must be {\"track_id\": int, \"reason\": str}") from error
    reasons = dict(picks)
    with db_session() as session:
        checked = AssistAppService(session).describe_tracks(
            [track_id for track_id, _ in picks],
            source_track_id=state.get("source_track_id"),
            intent=state.get("intent") or "keep",
        )
    shown = [{**track, "agent_reason": reasons.get(track["id"]) or None} for track in checked["tracks"]]
    listed = assist_bridge.show_list(title or "", shown, checked["rejected"], note, agent_name)
    return {
        "shown_track_ids": [track["id"] for track in shown],
        "rejected": checked["rejected"],
        "window_open": assist_bridge.window()["open"],
        "revision": listed["revision"],
    }


@mcp.tool()
def assist_clear_tracks() -> Dict[str, Any]:
    """Remove the list you put on the assist window with assist_show_tracks."""
    assist_bridge.clear_list()
    return {"cleared": True}
