from contextlib import contextmanager
from typing import Any, Dict, Iterator, List

from sqlmodel import Session
from mcp.server.mcpserver import MCPServer

from infra.database import connection

mcp = MCPServer(
    name="plumdeck",
    title="plumdeck Music Library",
    instructions=(
        "plumdeck is a local-first DJ music library. Use these tools to search tracks, "
        "manage setlists, classify genres, and inspect lyrics/wordplay. You are the "
        "reasoning model: plumdeck never calls a separate LLM. Translate natural-language "
        "vibes into search/setlist tool parameters yourself. For genre work, call "
        "get_genre_analysis_context, classify the returned tracks, then call "
        "apply_genre_analysis or apply_genre_analyses. For wordplay, inspect lyrics, "
        "choose phrases yourself, then call find_wordplay_links. Check list_wordplay_pairs "
        "before proposing; persist candidates with propose_wordplay_pairs as pending. "
        "For the performer's cue-drumming style, model the isolated source vocal with "
        "source_cue_mode='cue_drumming_intro' and start/end cue timestamps; start the "
        "target underneath at target_intro_timestamp, then land its word or section at "
        "target_landing_timestamp (legacy to_timestamp is the same landing cue). "
        "Only propose a short, recognizable vocal hit that remains intelligible when "
        "tapped repeatedly; reject generic sentence-tail matches such as I know or yeah. "
        "Because the target intro overlaps the source cue, require the same normalized "
        "genre and prioritize the same subgenre, drum feel, era, and intro texture. "
        "Reject phrase matches based only on genre when analyzed audio features clash. "
        "The target may land at its opening or at a later labelled section. "
        "Only approve_wordplay_pair when the user explicitly approves that pair; "
        "approval does not mean its audio was tested. reject_wordplay_pair permanently "
        "deletes only the registry item. Approved directed pairs inform setlist generation "
        "while genre/year and other candidate filters remain in force. Prefer search_tracks "
        "before assuming a track exists; returned track ids are required by mutation tools. "
        "For audio reanalysis use plan_track_analysis then start_track_analysis. "
        "Default to embedding-only and only_outdated to preserve tags and avoid unnecessary work. "
        "Monitor, pause and resume durable jobs with the track analysis tools."
        " Junction tools control only the running local plumdeck desktop app through its "
        "authenticated loopback bridge. On a fresh app launch call junction_prepare_engine "
        "before other Junction tools. Inspect exchange text before importing it, check "
        "junction_get_state after mutations, and do not approve, reject, end, leave, start "
        "recording, or begin a handoff without the user's explicit intent. Junction Live "
        "deck attachment is display-only; Program remains the actual venue output."
    ),
)


@contextmanager
def db_session() -> Iterator[Session]:
    with Session(connection.engine) as session:
        yield session


def serialize(obj: Any) -> Any:
    """SQLModel/pydantic インスタンスを JSON-safe な dict に変換する（datetime は ISO 文字列化）。"""
    if isinstance(obj, list):
        return [serialize(o) for o in obj]
    if isinstance(obj, dict):
        return {k: serialize(v) for k, v in obj.items()}
    if hasattr(obj, "model_dump"):
        return serialize(obj.model_dump(mode="json"))
    return obj


def track_list_payload(tracks: List[Any]) -> Dict[str, Any]:
    items = serialize(tracks)
    return {"count": len(items), "tracks": items}
