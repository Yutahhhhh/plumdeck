"""Assist presets, transition mode, compound filters, search and the MCP agent bridge.

The same rule as the rest of assist applies: when a condition cannot be
checked, the track is left out or marked unknown — never assumed to fit.
"""
import pytest

from app.services.assist_app_service import AssistAppService, AssistInputError
from app.services.assist_bridge import AssistBridge
from domain.models.track import Track
from domain.services import assist_recommendation as scoring
from domain.services.assist_filters import AssistFilters
from infra import rekordbox_library


def add(session, tmp_path, name, **spec):
    path = tmp_path / f"{name}.mp3"
    path.write_bytes(b"")
    defaults = dict(title=name, artist=f"{name} Artist", genre="House", bpm=124.0, key="8A",
                    duration=200.0, energy=0.6, danceability=0.5, noisiness=0.4,
                    brightness=0.2, year=2020)
    track = Track(filepath=str(path), **{**defaults, **spec})
    session.add(track)
    session.commit()
    session.refresh(track)
    return track


@pytest.fixture
def crate(session, tmp_path, mocker):
    tracks = {
        "source": add(session, tmp_path, "Source", bpm=100.0, key="8A", year=2020),
        "steady": add(session, tmp_path, "Steady", bpm=101.0, key="8A", year=2019),
        "old": add(session, tmp_path, "Old", bpm=99.0, key="9A", year=2008),
        "major": add(session, tmp_path, "Major", bpm=100.0, key="8B", brightness=0.3),
        "edit": add(session, tmp_path, "Tall Boys 100-128 Transition (Dirty)", bpm=128.0, key="3A"),
        "wrong_edit": add(session, tmp_path, "Other 90-128 Transition", bpm=128.0),
        "double": add(session, tmp_path, "Double", bpm=200.0, key="8A"),
        "fast": add(session, tmp_path, "Fast", bpm=128.0, key="8A", genre="Techno", artist="DJ Fast"),
        "unknown_bpm": add(session, tmp_path, "Unknown", bpm=None, genre="Techno"),
    }
    mocker.patch.object(
        rekordbox_library, "registered_paths",
        lambda: frozenset(rekordbox_library.normalize_path(t.filepath) for t in tracks.values()),
    )
    return tracks


def ids(result):
    return [candidate["id"] for candidate in result["candidates"]]


# ----------------------------------------------------------------- key moves

@pytest.mark.parametrize("source,candidate,move", [
    ("8A", "8A", "same"), ("8A", "9A", "+1"), ("8A", "7A", "-1"), ("8A", "3A", "+7"),
    ("12A", "1A", "+1"), ("8A", "8B", "to_major"), ("8B", "8A", "to_minor"),
    ("8A", "9B", "diagonal"), ("8A", "11A", "other"), ("A minor", "C major", "to_major"),
])
def test_camelot_moves_are_named(source, candidate, move):
    assert scoring.key_move(source, candidate) == move


def test_mood_presets_pull_the_key_toward_their_mode():
    assert scoring.key_score("8B", "8A", "emotional") == 1.0
    assert scoring.key_score("8A", "8B", "emotional") < 0.5
    assert scoring.key_score("8A", "8B", "bright") == 1.0
    assert scoring.key_score("8A", "9A", "hype") > scoring.key_score("8A", "7A", "hype")
    assert scoring.key_score("8A", "7A", "calm") > scoring.key_score("8A", "9A", "calm")
    assert scoring.key_score("8A", "", "hype") is None


# ------------------------------------------------------------------ presets

def test_throwback_only_offers_records_at_least_five_years_older(session, crate):
    result = AssistAppService(session).recommend(crate["source"].id, "throwback")
    assert ids(result) == [crate["old"].id]
    assert "2008年（12年前）" in result["candidates"][0]["summary"]


def test_throwback_without_a_source_year_says_so(session, crate):
    crate["source"].year = None
    session.add(crate["source"])
    session.commit()
    result = AssistAppService(session).recommend(crate["source"].id, "throwback")
    assert result["candidates"] == [] and "リリース年" in result["notes"][0]


def test_bright_prefers_the_move_to_major(session, crate):
    result = AssistAppService(session).recommend(crate["source"].id, "bright")
    assert ids(result)[0] == crate["major"].id


def test_normal_presets_never_offer_a_track_with_unknown_tempo(session, crate):
    for preset in scoring.MOOD_PRESETS:
        assert crate["unknown_bpm"].id not in ids(AssistAppService(session).recommend(crate["source"].id, preset))


# --------------------------------------------------------------- transition

@pytest.mark.parametrize("title,expected", [
    ("PGD - Tall Boys 100-128 Transition (Dirty)", (100.0, 128.0, True)),
    ("breathin - Tall Boys 124-100 Transition (Dirty)", (124.0, 100.0, True)),
    ("Lose It All 135 - 150", (135.0, 150.0, False)),
    ("Toss A Coin To Your Witcher 110 -150", (110.0, 150.0, False)),
    ("Circo Loco - MarkCutz Wordplay Transition (Dirty)", None),
    ("Greatest Hits 1999-2004", None),
    ("Track 100-101", None),
])
def test_transition_titles_are_parsed_conservatively(title, expected):
    assert scoring.parse_transition(title) == expected


def test_transition_mode_offers_matching_edits_and_double_time(session, crate):
    result = AssistAppService(session).recommend(crate["source"].id, "keep", transition=True)
    found = ids(result)
    assert crate["edit"].id in found
    assert crate["double"].id in found
    # An edit that opens at 90 does not start from a 100 BPM deck.
    assert crate["wrong_edit"].id not in found
    # Ordinary nearby tempos are not what transition mode is for.
    assert crate["steady"].id not in found
    assert result["caveats"]
    edit = next(c for c in result["candidates"] if c["id"] == crate["edit"].id)
    assert any("入り 100 → 出口 128" in r["text"] for r in edit["reasons"])


def test_a_titled_edit_in_normal_mode_warns_its_analysed_tempo_is_approximate(session, crate):
    crate["edit"].bpm = 101.0
    session.add(crate["edit"])
    session.commit()
    result = AssistAppService(session).recommend(crate["source"].id, "keep")
    edit = next(c for c in result["candidates"] if c["id"] == crate["edit"].id)
    assert any(r["kind"] == "tempo_change" and r["tone"] == "caution" for r in edit["reasons"])


def test_a_transition_edit_on_deck_is_mixed_out_of_at_its_closing_tempo(session, crate):
    # The edit ends at 128, whatever its analysed BPM says.
    crate["edit"].bpm = 100.0
    session.add(crate["edit"])
    session.commit()
    result = AssistAppService(session).recommend(crate["edit"].id, "keep")
    assert crate["fast"].id in ids(result)
    assert crate["steady"].id not in ids(result)
    assert "出口の 128 BPM" in result["notes"][0]


# ------------------------------------------------------------------ filters

def test_filters_combine_and_across_fields_and_or_within_one():
    conditions = AssistFilters.from_dict({"genres": ["house", "Disco"], "bpm_min": 120, "bpm_max": 126})
    assert conditions.matches({"genre": "House", "bpm": 124})
    assert conditions.matches({"genre": "ＤＩＳＣＯ", "bpm": 120})
    assert not conditions.matches({"genre": "House", "bpm": 128})
    assert not conditions.matches({"genre": "Techno", "bpm": 124})
    # Unknown values never pass a constrained field.
    assert not conditions.matches({"genre": "House", "bpm": None})


def test_invalid_key_filter_is_an_input_error(session, crate):
    with pytest.raises(AssistInputError):
        AssistAppService(session).recommend(crate["source"].id, "keep", filters={"keys": ["H minor"]})


def test_filters_narrow_recommendations(session, crate):
    result = AssistAppService(session).recommend(
        crate["source"].id, "keep", filters={"keys": ["9A"]}
    )
    assert ids(result) == [crate["old"].id]
    assert result["filters"] == ["キー 9A"]


def test_search_without_a_deck_requires_a_condition_and_orders_by_tempo(session, crate):
    service = AssistAppService(session)
    assert service.search({})["candidates"] == []
    result = service.search({"bpm_min": 99, "bpm_max": 101}, limit=10)
    bpms = [c["bpm"] for c in result["candidates"]]
    assert bpms == sorted(bpms) and set(bpms) <= {99.0, 100.0, 101.0}
    artist = service.search({"artists": ["dj fast"]})
    assert ids(artist) == [crate["fast"].id]


def test_search_drops_originals_rekordbox_does_not_know(session, crate, mocker):
    mocker.patch.object(rekordbox_library, "registered_paths", lambda: frozenset())
    result = AssistAppService(session).search({"genres": ["Techno"]})
    assert result["candidates"] == [] and result["unavailable_originals"] >= 1


# -------------------------------------------------------------- agent bridge

def test_describe_tracks_rejects_what_the_dj_cannot_load(session, crate, mocker):
    registered = frozenset(rekordbox_library.normalize_path(crate["old"].filepath) for _ in [0])
    mocker.patch.object(rekordbox_library, "registered_paths", lambda: registered)
    checked = AssistAppService(session).describe_tracks(
        [crate["old"].id, crate["steady"].id, 99999, crate["source"].id],
        source_track_id=crate["source"].id, intent="keep",
    )
    assert [t["id"] for t in checked["tracks"]] == [crate["old"].id]
    assert {r["track_id"] for r in checked["rejected"]} == {crate["steady"].id, 99999, crate["source"].id}
    assert checked["tracks"][0]["reasons"]


def test_bridge_reports_the_window_and_carries_agent_updates():
    now = [1000.0]
    bridge = AssistBridge(clock=lambda: now[0])
    assert bridge.window()["open"] is False
    bridge.report_window({"source_track_id": 3, "intent": "keep"})
    assert bridge.window()["open"] is True
    now[0] += 60
    assert bridge.window()["open"] is False  # a stale report means the window closed

    settings = bridge.apply_settings({"intent": "hype", "limit": None}, "Codex")
    assert settings["intent"] == "hype" and "limit" not in settings
    listed = bridge.show_list("  ", [{"id": 5}], [], note="", agent_name="Codex")
    assert listed["title"] == "エージェントの提案" and listed["note"] is None
    pending = bridge.pending()
    assert pending["settings"]["revision"] < pending["list"]["revision"] == pending["revision"]
    bridge.dismiss_list()
    assert bridge.pending()["list"] is None and bridge.pending()["revision"] > listed["revision"]


# ---------------------------------------------------------------------- API

def test_recommend_and_search_routes(client, crate):
    response = client.post("/api/assist/recommendations", json={
        "source_track_id": crate["source"].id, "intent": "hype", "limit": 5,
        "filters": {"bpm_min": 95, "bpm_max": 110},
    })
    assert response.status_code == 200, response.text
    body = response.json()
    assert body["intent"] == "hype" and body["intent_label"] == "盛り上げる"
    assert len(body["candidates"]) <= 5
    assert client.post("/api/assist/recommendations", json={
        "source_track_id": crate["source"].id, "filters": {"keys": ["H minor"]},
    }).status_code == 422
    search = client.post("/api/assist/search", json={"filters": {"genres": ["Techno"]}})
    assert search.status_code == 200 and search.json()["candidates"]


def test_window_and_agent_routes(client, mocker):
    bridge = AssistBridge()
    mocker.patch("api.routers.assist.assist_bridge", bridge)
    assert client.put("/api/assist/window", json={"source_track_id": 1, "intent": "dance"}).status_code == 200
    assert bridge.window()["state"]["intent"] == "dance"
    assert client.put("/api/assist/window", json={"intent": "shift"}).status_code == 422
    bridge.show_list("t", [{"id": 1}], [])
    assert client.get("/api/assist/agent").json()["list"]["title"] == "t"
    assert client.delete("/api/assist/agent/list").status_code == 200
    assert client.get("/api/assist/agent").json()["list"] is None


def test_mcp_tools_read_and_drive_the_window(session, crate, mocker):
    from mcp_server.tools import assist as tools

    bridge = AssistBridge()
    mocker.patch.object(tools, "assist_bridge", bridge)
    bridge.report_window({"source_track_id": crate["source"].id, "intent": "keep",
                          "exclude_track_ids": [crate["steady"].id], "filters": {}})
    state = tools.assist_get_state()
    assert state["window_open"] and state["source_track"]["title"] == "Source"
    assert state["settings"]["excluded_track_count"] == 1

    ranked = tools.assist_recommend(intent="hype")
    assert ranked["mode"] == "recommend" and ranked["intent"] == "hype"
    assert crate["steady"].id not in [c["id"] for c in ranked["candidates"]]

    applied = tools.assist_apply_settings(intent="groove", filters={"keys": ["A minor"]}, agent_name="Codex")
    assert applied["applied"]["intent"] == "keep"
    assert applied["applied"]["filters"]["keys"] == ["8A"]
    with pytest.raises(ValueError):
        tools.assist_apply_settings(filters={"keys": ["H minor"]})

    shown = tools.assist_show_tracks([
        {"track_id": crate["old"].id, "reason": "キー +1 で上げられる"},
        {"track_id": 424242, "reason": "存在しない"},
    ], title="盛り上げる候補", agent_name="Codex")
    assert shown["shown_track_ids"] == [crate["old"].id]
    assert shown["rejected"][0]["track_id"] == 424242
    listed = bridge.pending()["list"]
    assert listed["title"] == "盛り上げる候補"
    assert listed["tracks"][0]["agent_reason"] == "キー +1 で上げられる"
    tools.assist_clear_tracks()
    assert bridge.pending()["list"] is None
