"""MCP サーバー専用の自動テスト。

外部 MCP クライアント (Claude Desktop / Claude Code) が Streamable HTTP で
plumdeck の MCP サーバーに接続したときの挙動を、実際の MCP クライアント
(mcp.client.session.ClientSession + streamable_http_client) を使って検証する。

- `client` fixture (TestClient) は使わず、MCP 専用の async fixture `mcp_session` を使う。
- テストはすべて @pytest.mark.asyncio を付ける。
- ツール結果は result.content[0].text を JSON パースして dict として検証する。
"""

import asyncio
import json
import threading
import time

import httpx2
import pytest
from mcp.client.session import ClientSession
from mcp.client.streamable_http import streamable_http_client
from sqlmodel import Session

from models import Track, Setlist, SetlistTrack, Lyrics, TrackEmbedding


class _MCPClientProxy:
    """バックグラウンドのイベントループ上で動作する ClientSession へのプロキシ。

    pytest-asyncio は async fixture のセットアップとティアダウンを別タスクで実行するため、
    anyio のタスクグループ (cancel scope) がタスクをまたいで exit され
    "Attempted to exit cancel scope in a different task" エラーになる。
    これを避けるため、MCP セッション全体を単一のバックグラウンドイベントループ
    (単一タスク) で完結させ、テストからは run_coroutine_threadsafe で呼び出す。
    """

    def __init__(self, client_session: ClientSession, loop: asyncio.AbstractEventLoop):
        self._session = client_session
        self._loop = loop

    def _submit(self, coro):
        future = asyncio.run_coroutine_threadsafe(coro, self._loop)
        return asyncio.wrap_future(future)

    async def initialize(self):
        return await self._submit(self._session.initialize())

    async def list_tools(self, **kwargs):
        return await self._submit(self._session.list_tools(**kwargs))

    async def call_tool(self, name, arguments=None, **kwargs):
        return await self._submit(self._session.call_tool(name, arguments, **kwargs))


@pytest.fixture
def mcp_session(session: Session):
    """MCP サーバーへの接続を確立し、初期化済みの ClientSession へのプロキシを提供する。

    - `main` は fixture 内で import する (conftest のモック適用後に import されるようにする)。
    - httpx2.ASGITransport は lifespan を自動実行しないため、`lifespan(app)` を手動で実行する。
    - `session` fixture (同期) はそのまま依存できる。
    - MCP セッション全体を単一のバックグラウンドイベントループで完結させる
      (anyio のタスクグループがタスクをまたぐのを防ぐため)。
    """
    from main import app, lifespan
    from mcp.server.transport_security import TransportSecuritySettings
    from mcp_server.server import mcp_app_holder

    # テスト環境の Host ヘッダー (testserver) を許可するため DNS リバインディング保護を緩和する
    mcp_app_holder.transport_security = TransportSecuritySettings(
        enable_dns_rebinding_protection=False
    )

    loop = asyncio.new_event_loop()
    thread = threading.Thread(target=loop.run_forever, daemon=True)
    thread.start()

    holder: dict = {}
    stop_event = asyncio.Event()

    async def _run():
        async with lifespan(app):
            transport = httpx2.ASGITransport(app=app)
            async with httpx2.AsyncClient(
                transport=transport, base_url="http://testserver"
            ) as http_client:
                async with streamable_http_client(
                    "http://testserver/mcp", http_client=http_client
                ) as (read, write):
                    async with ClientSession(read, write) as client_session:
                        await client_session.initialize()
                        holder["session"] = client_session
                        await stop_event.wait()

    task = asyncio.run_coroutine_threadsafe(_run(), loop)

    # セッションが確立されるまで待機
    deadline = time.time() + 30
    while "session" not in holder:
        if task.done():
            task.result()  # セットアップ失敗なら例外を送出
        if time.time() > deadline:
            raise TimeoutError("MCP session setup timed out")
        time.sleep(0.01)

    proxy = _MCPClientProxy(holder["session"], loop)

    yield proxy

    # ティアダウン: 同じバックグラウンドループ (単一タスク) 内でセッションを閉じる
    async def _stop():
        stop_event.set()

    asyncio.run_coroutine_threadsafe(_stop(), loop).result(timeout=10)
    try:
        task.result(timeout=10)
    except Exception:
        pass
    loop.call_soon_threadsafe(loop.stop)
    thread.join(timeout=10)


def _add_track(
    session: Session,
    filepath: str,
    title: str,
    artist: str = "Artist",
    album: str = "Album",
    genre: str = "House",
    subgenre: str = "",
    bpm: float = 120,
    duration: float = 100,
    year: int | None = None,
    energy: float = 0.5,
) -> Track:
    """テスト用の Track を DB に追加して返す。"""
    track = Track(
        filepath=filepath,
        title=title,
        artist=artist,
        album=album,
        genre=genre,
        subgenre=subgenre,
        bpm=bpm,
        duration=duration,
        year=year,
        energy=energy,
    )
    session.add(track)
    session.commit()
    session.refresh(track)
    return track


def _result_dict(result) -> dict:
    """CallToolResult の TextContent を JSON パースして dict として返す。"""
    assert result.is_error is False, f"tool call unexpectedly failed: {result.content}"
    text = result.content[0].text
    return json.loads(text)


@pytest.mark.asyncio
@pytest.mark.parametrize("tool_name", ["generate_auto_setlist", "recommend_next_track"])
async def test_setlist_tools_apply_year_constraints(mcp_session, session, tool_name):
    source = _add_track(session, "/year/source.mp3", "Source", year=2020)
    expected = _add_track(session, "/year/1995.mp3", "1995", year=1995)
    _add_track(session, "/year/2005.mp3", "2005", year=2005)
    _add_track(session, "/year/unknown.mp3", "Unknown", year=None)
    args = {"year_min": 1990, "year_max": 1999, "target_noisiness": 0.1}
    if tool_name == "recommend_next_track":
        args["track_id"] = source.id
    else:
        args["length"] = 3
    data = _result_dict(await mcp_session.call_tool(tool_name, args))
    assert [track["id"] for track in data["tracks"]] == [expected.id]


@pytest.mark.asyncio
async def test_selective_analysis_job_over_mcp(mcp_session, session, tmp_path, mocker):
    from app.services.analysis_job_service import AnalysisJobService
    from domain.services.analysis.constants import EMBEDDING_MODEL, EMBEDDING_PIPELINE_VERSION
    track = _add_track(session, "/analysis/song.mp3", "Keep title", genre="House")
    def analyze(path, features):
        assert path == track.filepath and features == ["embedding"]
        return {"embedding": [0.1] * 200, "embedding_model": EMBEDDING_MODEL,
                "features_extra": {"embedding_pipeline_version": EMBEDDING_PIPELINE_VERSION}}, 0.01
    service = AnalysisJobService(session.bind, str(tmp_path / "jobs.sqlite3"), analyze)
    mocker.patch("mcp_server.tools.analysis.analysis_job_service", service)
    plan = _result_dict(await mcp_session.call_tool("plan_track_analysis", {"track_ids": [track.id]}))
    assert plan["selected_tracks"] == 1
    started = _result_dict(await mcp_session.call_tool("start_track_analysis", {"track_ids": [track.id], "workers": 1}))
    await asyncio.to_thread(service._thread.join, 5)
    assert not service.is_running
    status = _result_dict(await mcp_session.call_tool("get_track_analysis_status", {"job_id": started["id"]}))
    assert status["status"] == "completed"
    assert status["counts"]["completed"] == 1
    session.rollback()
    session.expire_all()
    assert session.get(Track, track.id).title == "Keep title"
    invalid = await mcp_session.call_tool("start_track_analysis", {"features": ["lyrics"]})
    assert invalid.is_error and "features" in invalid.content[0].text


# ---------------------------------------------------------------------------
# プロトコル疎通
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_initialize_returns_server_info(mcp_session):
    """initialize の結果にサーバー情報 (name=plumdeck) とプロトコルバージョン、tools 能力が含まれることを検証する。"""
    result = await mcp_session.initialize()
    assert result.server_info.name == "plumdeck"
    assert result.protocol_version
    assert result.capabilities.tools is not None


@pytest.mark.asyncio
async def test_list_tools_returns_all_tools(mcp_session):
    """list_tools で全ツールが返り、MCP-client reasoning toolsが含まれることを検証する。"""
    result = await mcp_session.list_tools()
    names = [t.name for t in result.tools]
    assert len(names) == 85
    assert {"list_wordplay_pairs", "propose_wordplay_pairs", "approve_wordplay_pair", "reject_wordplay_pair"}.issubset(names)
    assert {"register_track_lyrics", "register_track_lyrics_batch"}.issubset(names)
    assert {"plan_track_analysis", "start_track_analysis", "get_track_analysis_status",
            "pause_track_analysis", "resume_track_analysis"}.issubset(names)
    assert {
        "junction_get_state", "junction_prepare_engine", "junction_inspect_exchange",
        "junction_get_exchange_text",
        "junction_list_audio_devices",
        "junction_get_network", "junction_test_network", "junction_configure_network",
        "junction_clear_network", "junction_create_session", "junction_join_session",
        "junction_create_invite", "junction_import_exchange",
        "junction_approve_participant", "junction_reject_participant",
        "junction_cancel_invite", "junction_retry_participant", "junction_update_profile",
        "junction_reorder_roster", "junction_start_session", "junction_request_handoff",
        "junction_cancel_handoff", "junction_accept_handoff", "junction_resume_recovery",
        "junction_leave_session", "junction_end_session", "junction_configure_program",
        "junction_start_program_recording", "junction_stop_program_recording",
        "junction_load_private_preview", "junction_play_private_preview",
        "junction_pause_private_preview", "junction_seek_private_preview",
        "junction_set_private_preview_gain", "junction_unload_private_preview",
        "junction_get_private_preview_state", "junction_set_microphone_enabled",
        "junction_attach_live_monitor", "junction_detach_live_monitor",
    }.issubset(names)
    for expected in [
        "search_tracks",
        "list_setlists",
        "create_setlist",
        "generate_auto_setlist",
        "recommend_next_track_page",
        "export_setlist_m3u8",
        "find_wordplay_links",
        "add_track_to_setlist_with_wordplay",
        "update_setlist_track_wordplay",
        "clear_setlist_track_wordplay",
        "get_genre_analysis_context",
        "apply_genre_analysis",
        "apply_genre_analyses",
        "get_track_lyrics",
        "search_lyrics",
    ]:
        assert expected in names


@pytest.mark.asyncio
async def test_paged_recommendations_mcp_returns_global_order_and_total(mcp_session, session: Session):
    target = _add_track(session, "/page-target.mp3", "Target", bpm=120)
    weaker = _add_track(session, "/page-weaker.mp3", "Weaker", bpm=126)
    strongest = _add_track(session, "/page-strongest.mp3", "Strongest", bpm=120)
    vector = json.dumps([0.1] * 200)
    for track in (target, weaker, strongest):
        session.add(TrackEmbedding(track_id=track.id, embedding_json=vector))
    session.commit()

    result = await mcp_session.call_tool("recommend_next_track_page", {
        "track_id": target.id, "limit": 1, "offset": 0,
    })
    page = _result_dict(result)
    assert page["total"] == 2
    assert page["items"][0]["id"] == strongest.id
    assert page["has_more"] is True


# ---------------------------------------------------------------------------
# 楽曲・検索 (read)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_search_tracks(mcp_session, session: Session):
    """Track を 3 件作成し、search_tracks で q / genres / bpm 検索が機能することを検証する。"""
    _add_track(session, "/t1.mp3", "Sunrise", genre="House", bpm=120)
    _add_track(session, "/t2.mp3", "Midnight", genre="Techno", bpm=130)
    _add_track(session, "/t3.mp3", "Sunset", genre="House", bpm=124)

    # q 検索 (タイトル横断)
    result = await mcp_session.call_tool("search_tracks", {"q": "Sun"})
    data = _result_dict(result)
    assert data["count"] == 2
    titles = {t["title"] for t in data["tracks"]}
    assert titles == {"Sunrise", "Sunset"}

    # genres 検索
    result = await mcp_session.call_tool("search_tracks", {"genres": ["Techno"]})
    data = _result_dict(result)
    assert data["count"] == 1
    assert data["tracks"][0]["title"] == "Midnight"

    # bpm 検索 (bpm_range=0 で完全一致)
    result = await mcp_session.call_tool("search_tracks", {"bpm": 124, "bpm_range": 0})
    data = _result_dict(result)
    assert data["count"] == 1
    assert data["tracks"][0]["title"] == "Sunset"


@pytest.mark.asyncio
async def test_get_track_similar(mcp_session, session: Session):
    """Track 2 件 (embedding 付き) を作成し、get_track_similar がエラーなく返ることを検証する。"""
    t1 = _add_track(session, "/sim1.mp3", "Sim1", genre="House", bpm=120)
    t2 = _add_track(session, "/sim2.mp3", "Sim2", genre="House", bpm=122)
    # ベクトル類似検索には embedding が必要
    vec = [0.1] * 200
    session.add(TrackEmbedding(track_id=t1.id, embedding_json=json.dumps(vec)))
    session.add(TrackEmbedding(track_id=t2.id, embedding_json=json.dumps(vec)))
    session.commit()

    result = await mcp_session.call_tool("get_track_similar", {"track_id": t1.id})
    data = _result_dict(result)
    assert "count" in data
    assert "tracks" in data


@pytest.mark.asyncio
async def test_suggest_track_genre(mcp_session, session: Session):
    """Track 作成後 suggest_track_genre がエラーなく返ることを検証する。"""
    t1 = _add_track(session, "/sug1.mp3", "Sug1", genre="Unknown", bpm=120)
    result = await mcp_session.call_tool("suggest_track_genre", {"track_id": t1.id})
    data = _result_dict(result)
    # embedding が無い場合は suggested_genre=None になるが、エラーにはならない
    assert "suggested_genre" in data


# ---------------------------------------------------------------------------
# 楽曲・更新 (write)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_update_track_genre(mcp_session, session: Session):
    """Track 作成 → update_track_genre で genre が更新されることを検証する。"""
    t1 = _add_track(session, "/ug1.mp3", "Ug1", genre="House")
    result = await mcp_session.call_tool(
        "update_track_genre", {"track_id": t1.id, "genre": "Techno"}
    )
    data = _result_dict(result)
    assert data["genre"] == "Techno"


@pytest.mark.asyncio
async def test_update_track_info(mcp_session, session: Session):
    """Track 作成 → update_track_info で title / year が反映されることを検証する。"""
    t1 = _add_track(session, "/ui1.mp3", "Old Title", year=2000)
    result = await mcp_session.call_tool(
        "update_track_info", {"track_id": t1.id, "title": "New Title", "year": 2020}
    )
    data = _result_dict(result)
    assert data["title"] == "New Title"
    assert data["year"] == 2020


# ---------------------------------------------------------------------------
# セットリスト
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_list_setlists_empty(mcp_session):
    """初期状態で list_setlists が空リストを返すことを検証する。"""
    result = await mcp_session.call_tool("list_setlists", {})
    data = _result_dict(result)
    assert data["setlists"] == []


@pytest.mark.asyncio
async def test_create_setlist_and_add_tracks(mcp_session, session: Session):
    """create_setlist → add_track_to_setlist (2 件) → get_setlist_tracks で順序どおり返ることを検証する。"""
    t1 = _add_track(session, "/c1.mp3", "C1")
    t2 = _add_track(session, "/c2.mp3", "C2")

    result = await mcp_session.call_tool("create_setlist", {"name": "My Set"})
    setlist = _result_dict(result)
    setlist_id = setlist["id"]

    await mcp_session.call_tool(
        "add_track_to_setlist", {"setlist_id": setlist_id, "track_id": t1.id}
    )
    await mcp_session.call_tool(
        "add_track_to_setlist", {"setlist_id": setlist_id, "track_id": t2.id}
    )

    result = await mcp_session.call_tool(
        "get_setlist_tracks", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert data["count"] == 2
    assert [t["title"] for t in data["tracks"]] == ["C1", "C2"]


@pytest.mark.asyncio
async def test_set_setlist_tracks_replaces(mcp_session, session: Session):
    """set_setlist_tracks で一括置換できることを検証する。"""
    t1 = _add_track(session, "/r1.mp3", "R1")
    t2 = _add_track(session, "/r2.mp3", "R2")
    t3 = _add_track(session, "/r3.mp3", "R3")

    result = await mcp_session.call_tool("create_setlist", {"name": "Replace Set"})
    setlist_id = _result_dict(result)["id"]

    result = await mcp_session.call_tool(
        "set_setlist_tracks",
        {"setlist_id": setlist_id, "track_ids": [t1.id, t2.id, t3.id]},
    )
    data = _result_dict(result)
    assert data["count"] == 3
    assert [t["title"] for t in data["tracks"]] == ["R1", "R2", "R3"]

    # 置換 (順序変更 + 削除)
    result = await mcp_session.call_tool(
        "set_setlist_tracks", {"setlist_id": setlist_id, "track_ids": [t3.id, t1.id]}
    )
    data = _result_dict(result)
    assert data["count"] == 2
    assert [t["title"] for t in data["tracks"]] == ["R3", "R1"]


@pytest.mark.asyncio
async def test_remove_track_from_setlist(mcp_session, session: Session):
    """追加後に remove_track_from_setlist で削除できることを検証する。"""
    t1 = _add_track(session, "/rm1.mp3", "Rm1")
    t2 = _add_track(session, "/rm2.mp3", "Rm2")

    result = await mcp_session.call_tool("create_setlist", {"name": "Remove Set"})
    setlist_id = _result_dict(result)["id"]

    await mcp_session.call_tool(
        "set_setlist_tracks", {"setlist_id": setlist_id, "track_ids": [t1.id, t2.id]}
    )
    result = await mcp_session.call_tool(
        "remove_track_from_setlist", {"setlist_id": setlist_id, "track_id": t1.id}
    )
    data = _result_dict(result)
    assert data["count"] == 1
    assert data["tracks"][0]["title"] == "Rm2"


@pytest.mark.asyncio
async def test_rename_setlist(mcp_session, session: Session):
    """rename_setlist で名前が変更されることを検証する。"""
    result = await mcp_session.call_tool("create_setlist", {"name": "Old Name"})
    setlist_id = _result_dict(result)["id"]

    result = await mcp_session.call_tool(
        "rename_setlist", {"setlist_id": setlist_id, "name": "New Name"}
    )
    data = _result_dict(result)
    assert data["name"] == "New Name"


@pytest.mark.asyncio
async def test_delete_setlist(mcp_session, session: Session):
    """delete_setlist で削除され、list_setlists に反映されることを検証する。"""
    result = await mcp_session.call_tool("create_setlist", {"name": "Del Set"})
    setlist_id = _result_dict(result)["id"]

    result = await mcp_session.call_tool("delete_setlist", {"setlist_id": setlist_id})
    data = _result_dict(result)
    assert data["ok"] is True

    result = await mcp_session.call_tool("list_setlists", {})
    data = _result_dict(result)
    assert data["setlists"] == []


@pytest.mark.asyncio
async def test_export_setlist_m3u8(mcp_session, session: Session):
    """Track (filepath 付き) をセットリストに追加し、export_setlist_m3u8 で #EXTM3U と filepath が含まれることを検証する。"""
    t1 = _add_track(session, "/music/song.mp3", "Song", artist="Art")
    result = await mcp_session.call_tool("create_setlist", {"name": "Export Set"})
    setlist_id = _result_dict(result)["id"]
    await mcp_session.call_tool(
        "add_track_to_setlist", {"setlist_id": setlist_id, "track_id": t1.id}
    )

    result = await mcp_session.call_tool(
        "export_setlist_m3u8", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert "#EXTM3U" in data["m3u8"]
    assert "/music/song.mp3" in data["m3u8"]


@pytest.mark.asyncio
async def test_validate_setlist_export(mcp_session, session: Session):
    """validate_setlist_export がエラーなく返ることを検証する。"""
    t1 = _add_track(session, "/music/val.mp3", "Val", artist="Art")
    result = await mcp_session.call_tool("create_setlist", {"name": "Val Set"})
    setlist_id = _result_dict(result)["id"]
    await mcp_session.call_tool(
        "add_track_to_setlist", {"setlist_id": setlist_id, "track_id": t1.id}
    )

    result = await mcp_session.call_tool(
        "validate_setlist_export", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert "total" in data
    assert "missing" in data


# ---------------------------------------------------------------------------
# レコメンド・自動生成 (vibe)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_generate_auto_setlist_with_structured_targets(mcp_session, session: Session):
    """MCP client-selected targets generate a set without another LLM call."""
    _add_track(session, "/auto1.mp3", "Auto1", genre="House", bpm=120, energy=0.6)
    _add_track(session, "/auto2.mp3", "Auto2", genre="House", bpm=122, energy=0.7)
    _add_track(session, "/auto3.mp3", "Auto3", genre="House", bpm=124, energy=0.8)
    result = await mcp_session.call_tool(
        "generate_auto_setlist", {"target_bpm": 122, "target_energy": 0.7, "length": 3}
    )
    data = _result_dict(result)
    assert data["count"] == 3


@pytest.mark.asyncio
async def test_generate_auto_setlist_default_length(mcp_session, session: Session):
    """length 指定なしで generate_auto_setlist を呼んでもエラーにならないことを検証する。"""
    _add_track(session, "/autod1.mp3", "AutoD1", genre="House", bpm=120)
    result = await mcp_session.call_tool("generate_auto_setlist", {})
    data = _result_dict(result)
    # デフォルト曲数 (10) と候補プール (1 件) の小さい方になるが、エラーにはならない
    assert 1 <= data["count"] <= 10


@pytest.mark.asyncio
async def test_recommend_next_track_with_structured_target(mcp_session, session: Session):
    t1 = _add_track(session, "/rec1.mp3", "Rec1", genre="House", bpm=120)
    _add_track(session, "/rec2.mp3", "Rec2", genre="House", bpm=122)
    result = await mcp_session.call_tool(
        "recommend_next_track", {"track_id": t1.id, "target_energy": 0.9}
    )
    data = _result_dict(result)
    assert "count" in data
    assert "tracks" in data


# ---------------------------------------------------------------------------
# ワードプレイ
# ---------------------------------------------------------------------------


def _add_lyrics(session: Session, track_id: int, content: str):
    session.add(Lyrics(track_id=track_id, content=content))
    session.commit()


@pytest.mark.asyncio
async def test_find_wordplay_links(mcp_session, session: Session):
    """共通キーワード 'midnight' を含む歌詞 2 件 → find_wordplay_links が links を返すことを検証する。"""
    t1 = _add_track(session, "/wp1.mp3", "Wp1")
    t2 = _add_track(session, "/wp2.mp3", "Wp2")
    _add_lyrics(session, t1.id, "Walking through the midnight city")
    _add_lyrics(session, t2.id, "Meet me at the midnight train")
    result = await mcp_session.call_tool(
        "find_wordplay_links", {"track_id": t1.id, "keywords": ["midnight"]}
    )
    data = _result_dict(result)
    assert data["track_id"] == t1.id
    assert "midnight" in data["keywords"]
    assert len(data["links"]) >= 1
    link = data["links"][0]
    assert link["keyword"] == "midnight"
    assert link["track"]["id"] == t2.id


@pytest.mark.asyncio
async def test_add_track_to_setlist_with_wordplay(mcp_session, session: Session):
    """add_track_to_setlist_with_wordplay で wordplay_json 付きで追加されることを検証する。"""
    t1 = _add_track(session, "/wpadd1.mp3", "WpAdd1")
    t2 = _add_track(session, "/wpadd2.mp3", "WpAdd2")

    result = await mcp_session.call_tool("create_setlist", {"name": "Wordplay Set"})
    setlist_id = _result_dict(result)["id"]

    result = await mcp_session.call_tool(
        "add_track_to_setlist_with_wordplay",
        {"setlist_id": setlist_id, "track_id": t1.id, "keyword": "midnight"},
    )
    data = _result_dict(result)
    assert data["count"] == 1

    result = await mcp_session.call_tool(
        "get_setlist_tracks", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert data["count"] == 1
    assert data["tracks"][0]["title"] == "WpAdd1"
    assert data["tracks"][0]["wordplay_json"]
    assert "midnight" in data["tracks"][0]["wordplay_json"]


@pytest.mark.asyncio
async def test_update_and_clear_setlist_track_wordplay(mcp_session, session: Session):
    """update_setlist_track_wordplay → clear_setlist_track_wordplay の一連の流れを検証する。"""
    t1 = _add_track(session, "/wpup1.mp3", "WpUp1")

    result = await mcp_session.call_tool("create_setlist", {"name": "WpUpdate Set"})
    setlist_id = _result_dict(result)["id"]
    await mcp_session.call_tool(
        "add_track_to_setlist", {"setlist_id": setlist_id, "track_id": t1.id}
    )
    result = await mcp_session.call_tool(
        "get_setlist_tracks", {"setlist_id": setlist_id}
    )
    setlist_track_id = _result_dict(result)["tracks"][0]["setlist_track_id"]

    # 更新
    result = await mcp_session.call_tool(
        "update_setlist_track_wordplay",
        {
            "setlist_track_id": setlist_track_id,
            "wordplay": {
                "keyword": "midnight",
                "source_phrase": "midnight city",
                "target_phrase": "midnight train",
            },
        },
    )
    data = _result_dict(result)
    assert "midnight" in data["wordplay_json"]

    # クリア
    result = await mcp_session.call_tool(
        "clear_setlist_track_wordplay", {"setlist_track_id": setlist_track_id}
    )
    data = _result_dict(result)
    assert data["ok"] is True

    result = await mcp_session.call_tool(
        "get_setlist_tracks", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert data["tracks"][0]["wordplay_json"] is None


# ---------------------------------------------------------------------------
# ジャンル・歌詞
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_list_genres_subgenres(mcp_session, session: Session):
    """Track 作成後 list_genres / list_subgenres に反映されることを検証する。"""
    _add_track(session, "/g1.mp3", "G1", genre="House", subgenre="Deep House")
    _add_track(session, "/g2.mp3", "G2", genre="Techno", subgenre="Minimal")

    result = await mcp_session.call_tool("list_genres", {})
    data = _result_dict(result)
    assert "House" in data["genres"]
    assert "Techno" in data["genres"]

    result = await mcp_session.call_tool("list_subgenres", {})
    data = _result_dict(result)
    assert "Deep House" in data["subgenres"]
    assert "Minimal" in data["subgenres"]


@pytest.mark.asyncio
async def test_get_unknown_genre_tracks(mcp_session, session: Session):
    """未検証 (is_genre_verified=False) の Track が get_unknown_genre_tracks で返ることを検証する。"""
    _add_track(session, "/unk1.mp3", "Unk1", genre="Unknown")
    ver1 = _add_track(session, "/ver1.mp3", "Ver1", genre="House")
    # 検証済みにする (unknown リストから除外される)
    ver1.is_genre_verified = True
    session.add(ver1)
    session.commit()

    result = await mcp_session.call_tool("get_unknown_genre_tracks", {})
    data = _result_dict(result)
    titles = {t["title"] for t in data["tracks"]}
    assert "Unk1" in titles
    assert "Ver1" not in titles


@pytest.mark.asyncio
async def test_genre_context_and_apply_use_client_result(mcp_session, session: Session):
    """The MCP client reads context and submits its own structured classification."""
    t1 = _add_track(session, "/an1.mp3", "An1", genre="Unknown")
    context_result = await mcp_session.call_tool(
        "get_genre_analysis_context", {"track_ids": [t1.id], "mode": "both"}
    )
    context = _result_dict(context_result)
    assert context["tracks"][0]["title"] == "An1"

    result = await mcp_session.call_tool("apply_genre_analysis", {
        "track_id": t1.id,
        "genre": "Techno",
        "subgenre": "Minimal Techno",
        "reason": "client classification",
        "confidence": "High",
    })
    data = _result_dict(result)
    assert data["analysis"]["genre"] == "Techno"


@pytest.mark.asyncio
async def test_get_track_lyrics(mcp_session, session: Session):
    """Lyrics レコード作成 → get_track_lyrics で content を取得できることを検証する。"""
    t1 = _add_track(session, "/ly1.mp3", "Ly1")
    session.add(Lyrics(track_id=t1.id, content="Hello world lyrics"))
    session.commit()

    result = await mcp_session.call_tool("get_track_lyrics", {"track_id": t1.id})
    data = _result_dict(result)
    assert data["content"] == "Hello world lyrics"


@pytest.mark.asyncio
async def test_search_lyrics(mcp_session, session: Session):
    """Lyrics 作成 → search_lyrics(q=...) でヒットすることを検証する。"""
    t1 = _add_track(session, "/ly2.mp3", "Ly2")
    session.add(Lyrics(track_id=t1.id, content="Midnight city lights"))
    session.commit()

    result = await mcp_session.call_tool("search_lyrics", {"q": "midnight"})
    data = _result_dict(result)
    assert len(data["results"]) >= 1
    assert data["results"][0]["track"]["title"] == "Ly2"


# ---------------------------------------------------------------------------
# エラー応答
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_call_unknown_tool_returns_error(mcp_session):
    """未知ツールを呼ぶと is_error=True になることを検証する。"""
    result = await mcp_session.call_tool("nonexistent_tool", {})
    assert result.is_error is True


@pytest.mark.asyncio
async def test_update_track_genre_not_found(mcp_session):
    """存在しない track_id で update_track_genre を呼ぶと is_error=True かつ 'not found' を含むことを検証する。"""
    result = await mcp_session.call_tool(
        "update_track_genre", {"track_id": 99999, "genre": "House"}
    )
    assert result.is_error is True
    text = result.content[0].text
    assert "not found" in text


@pytest.mark.asyncio
async def test_get_track_lyrics_not_found(mcp_session):
    """存在しない track_id で get_track_lyrics を呼ぶと is_error=True になることを検証する。"""
    result = await mcp_session.call_tool("get_track_lyrics", {"track_id": 99999})
    assert result.is_error is True


@pytest.mark.asyncio
async def test_delete_setlist_not_found(mcp_session):
    """存在しない setlist_id で delete_setlist を呼ぶと is_error=True になることを検証する。"""
    result = await mcp_session.call_tool("delete_setlist", {"setlist_id": 99999})
    assert result.is_error is True


@pytest.mark.asyncio
async def test_call_tool_missing_required_argument(mcp_session):
    """必須引数なしで create_setlist を呼ぶと is_error=True (引数バリデーションエラー) になることを検証する。"""
    result = await mcp_session.call_tool("create_setlist", {})
    assert result.is_error is True


# ---------------------------------------------------------------------------
# E2E シナリオ
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_e2e_search_to_setlist_export(mcp_session, session: Session):
    """検索 → セットリスト作成 → 一括置換 → 順序確認 → M3U8 エクスポートの一連の流れを検証する。"""
    t1 = _add_track(session, "/music/e1.mp3", "E1", genre="House", bpm=120)
    t2 = _add_track(session, "/music/e2.mp3", "E2", genre="House", bpm=124)
    t3 = _add_track(session, "/music/e3.mp3", "E3", genre="Techno", bpm=130)

    # 検索で絞り込み (House のみ)
    result = await mcp_session.call_tool("search_tracks", {"genres": ["House"]})
    data = _result_dict(result)
    assert data["count"] == 2
    house_ids = [t["id"] for t in data["tracks"]]
    house_titles = [t["title"] for t in data["tracks"]]

    # セットリスト作成
    result = await mcp_session.call_tool("create_setlist", {"name": "E2E Set"})
    setlist_id = _result_dict(result)["id"]

    # 一括置換 (House 2 曲)
    result = await mcp_session.call_tool(
        "set_setlist_tracks", {"setlist_id": setlist_id, "track_ids": house_ids}
    )
    data = _result_dict(result)
    assert data["count"] == 2

    # 順序確認 (検索結果の並び順どおりに保持される)
    result = await mcp_session.call_tool(
        "get_setlist_tracks", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert [t["title"] for t in data["tracks"]] == house_titles

    # M3U8 エクスポート内容確認
    result = await mcp_session.call_tool(
        "export_setlist_m3u8", {"setlist_id": setlist_id}
    )
    data = _result_dict(result)
    assert "#EXTM3U" in data["m3u8"]
    assert "/music/e1.mp3" in data["m3u8"]
    assert "/music/e2.mp3" in data["m3u8"]
    assert "/music/e3.mp3" not in data["m3u8"]


@pytest.mark.asyncio
async def test_wordplay_review_generate_save_and_reject_over_mcp(mcp_session, session):
    source = _add_track(session, '/mcp-wordplay/source.mp3', 'Source')
    target = _add_track(session, '/mcp-wordplay/target.mp3', 'Target')
    proposal = {
        'from_track_id': source.id, 'to_track_id': target.id,
        'keyword': 'yeah', 'source_phrase': 'yeah', 'target_phrase': 'yeah',
        'source_section_position': 'end', 'target_section_position': 'start',
        'from_timestamp': 60.0, 'to_timestamp': 4.0,
        'target_intro_timestamp': 0.5,
    }
    created = _result_dict(await mcp_session.call_tool('propose_wordplay_pairs', {'pairs': [proposal]}))
    pair = created['items'][0]
    assert pair['status'] == 'pending'
    pending = _result_dict(await mcp_session.call_tool('list_wordplay_pairs', {'status': 'pending'}))
    assert pending['total'] == 1
    _result_dict(await mcp_session.call_tool('approve_wordplay_pair', {'pair_id': pair['id']}))
    generated = _result_dict(await mcp_session.call_tool('generate_auto_setlist', {
        'length': 2, 'seed_track_ids': [source.id],
    }))
    assert json.loads(generated['tracks'][1]['wordplay_json'])['from_track_id'] == source.id
    saved = _result_dict(await mcp_session.call_tool('create_setlist', {'name': 'Wordplay MCP'}))
    result = _result_dict(await mcp_session.call_tool('set_setlist_tracks', {
        'setlist_id': saved['id'], 'track_data': generated['tracks'],
    }))
    assert json.loads(result['tracks'][1]['wordplay_json'])['pair_id'] == pair['id']
    _result_dict(await mcp_session.call_tool('reject_wordplay_pair', {'pair_id': pair['id']}))
    assert _result_dict(await mcp_session.call_tool('list_wordplay_pairs', {}))['total'] == 0
    persisted = _result_dict(await mcp_session.call_tool('get_setlist_tracks', {'setlist_id': saved['id']}))
    assert len(persisted['tracks']) == 2
    assert persisted['tracks'][1]['wordplay_json'] is not None
