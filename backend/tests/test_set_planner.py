"""Phase-based whole-set planning from rekordbox playlists and stated tastes."""
import json

import pytest
from sqlmodel import Session

from app.services.set_planner_service import SetPlannerService
from domain.models.wordplay import WordplayPair
from domain.services.phased_setlist_builder import Phase, PhasedSetlistBuilder
from infra import rekordbox_playlists
from infra.rekordbox_playlists import PlaylistEntry, PlaylistNode
from models import Setlist, Track, TrackEmbedding


def _track(session, name, artist="A", bpm=100.0, key="8A", energy=0.5, genre="Hip-Hop", embed=None):
    track = Track(filepath=f"/music/{name}.mp3", title=name, artist=artist, album="", genre=genre,
                  bpm=bpm, key=key, energy=energy, duration=180)
    session.add(track)
    session.commit()
    session.refresh(track)
    if embed is not None:
        session.add(TrackEmbedding(track_id=track.id, embedding_json=json.dumps(embed)))
        session.commit()
    return track


def _node(tid, bpm=100.0, energy=0.5, key="8A", artist=None, title=None):
    return {"id": tid, "vector": None, "embedding_model": None, "sources": [],
            "track": Track(id=tid, filepath=f"/{tid}.mp3", title=title or f"T{tid}",
                           artist=artist or f"Artist{tid}", genre="G", bpm=bpm, key=key,
                           energy=energy, duration=180)}


def _tree(session, playlists: dict[str, list[Track]]):
    """A fake rekordbox tree: {'現場物/イベ2/hiphop': [tracks], ...}."""
    roots: list[PlaylistNode] = []
    entries: dict[str, list[PlaylistEntry]] = {}
    nodes: dict[str, PlaylistNode] = {}
    for path, tracks in playlists.items():
        parts = path.split("/")
        parent = None
        for depth, name in enumerate(parts):
            current = "/".join(parts[: depth + 1])
            node = nodes.get(current)
            if node is None:
                is_leaf = depth == len(parts) - 1
                node = PlaylistNode(id=f"id-{current}", name=name, parent_id=parent.id if parent else "root",
                                    kind="playlist" if is_leaf else "folder", seq=len(nodes), path=current)
                nodes[current] = node
                (parent.children if parent else roots).append(node)
            parent = node
        entries[parent.id] = [
            PlaylistEntry(parent.id, path, number, str(track.id), track.filepath, track.title, track.artist)
            for number, track in enumerate(tracks, start=1)
        ]
    return roots, entries


@pytest.fixture
def fake_rekordbox(mocker):
    def install(roots, entries):
        mocker.patch.object(rekordbox_playlists, "load_tree", lambda: roots)
        def fake_entries(node, recursive=True):
            found = []

            def walk(current):
                if current.kind == "playlist":
                    found.extend(entries.get(current.id, []))
                for child in current.children:
                    walk(child)

            walk(node)
            return found, []

        mocker.patch.object(rekordbox_playlists, "playlist_entries", fake_entries)
    return install


# ------------------------------------------------------------------ builder

def test_phases_stay_in_order_and_do_not_reuse_tracks():
    first = [_node(1), _node(2), _node(3)]
    second = [_node(3), _node(4), _node(5)]
    placements, notes = PhasedSetlistBuilder().build([
        Phase(label="warm", count=2, pool=first),
        Phase(label="peak", count=2, pool=second),
    ])
    assert [p["phase"] for p in placements] == ["warm", "warm", "peak", "peak"]
    ids = [p["node"]["id"] for p in placements]
    assert len(ids) == len(set(ids))
    assert placements[2]["phase_switch"] is True
    assert notes == []


def test_energy_ramp_orders_a_phase_from_low_to_high():
    pool = [_node(1, energy=0.9), _node(2, energy=0.2), _node(3, energy=0.55)]
    placements, _ = PhasedSetlistBuilder().build([
        Phase(label="build", count=3, pool=pool, energy=(0.2, 0.9)),
    ])
    assert [p["node"]["id"] for p in placements] == [2, 3, 1]


def test_anchor_tracks_are_always_placed_even_when_they_mix_badly():
    pool = [_node(1, bpm=100), _node(2, bpm=100), _node(3, bpm=100), _node(9, bpm=150, key="2B")]
    placements, notes = PhasedSetlistBuilder().build([
        Phase(label="must", count=2, pool=pool, anchor_ids=[9]),
    ])
    assert 9 in [p["node"]["id"] for p in placements]
    assert notes == []


def test_phase_switch_prefers_the_smoothest_entry_into_the_next_phase():
    warm = [_node(1, bpm=100, key="8A")]
    latin = [_node(2, bpm=128, key="3B"), _node(3, bpm=101, key="8A")]
    placements, _ = PhasedSetlistBuilder().build([
        Phase(label="warm", count=1, pool=warm),
        Phase(label="latin", count=2, pool=latin),
    ])
    assert [p["node"]["id"] for p in placements] == [1, 3, 2]


def test_wordplay_only_target_waits_for_its_source_and_duplicate_recordings_are_skipped():
    source = _node(1, bpm=100)
    target = {**_node(2, bpm=100), "wordplay_only_from_ids": {1}}
    other = _node(3, bpm=100)
    same_recording = _node(4, bpm=100, title="T3", artist="Artist3")
    placements, _ = PhasedSetlistBuilder().build(
        [Phase(label="p", count=4, pool=[other, same_recording, source, target])],
        approved_wordplay_edges={(1, 2)},
    )
    ids = [p["node"]["id"] for p in placements]
    assert not ({3, 4} <= set(ids))
    assert ids.index(2) == ids.index(1) + 1
    assert placements[ids.index(2)]["transition"]["approved_wordplay"] is True


def test_edits_of_one_song_are_not_both_placed():
    clean = _node(1, title="No Guidance (feat. Drake)", artist="Chris Brown, Drake")
    dirty = _node(2, title="No Guidance (Dirty)", artist="Chris Brown ft. Drake")
    remix = _node(3, title="No Guidance - DJ X Remix", artist="Chris Brown")
    other = _node(4, title="Other Song", artist="Someone")
    mashup = _node(5, title="Other Song vs Popular - Edit", artist="Someone vs The Weeknd")
    pool = [clean, dirty, remix, other, mashup]
    placements, _ = PhasedSetlistBuilder().build([Phase(label="p", count=5, pool=pool)])
    ids = [p["node"]["id"] for p in placements]
    assert len(ids) == 2 and 4 in ids


def test_featured_artist_counts_as_the_same_artist_back_to_back():
    first = _node(1, artist="Drake")
    featured = _node(2, artist="Chris Brown ft. Drake")
    different = _node(3, artist="Usher")
    placements, _ = PhasedSetlistBuilder().build([Phase(label="p", count=2, pool=[first, featured, different])])
    assert [p["node"]["id"] for p in placements] == [1, 3]


def test_short_pool_is_reported():
    placements, notes = PhasedSetlistBuilder().build([Phase(label="x", count=3, pool=[_node(1)])])
    assert len(placements) == 1
    assert "1/3" in notes[0]


# ----------------------------------------------------------- playlist tree

def test_playlist_path_lookup_folds_case_and_rejects_ambiguity():
    root = PlaylistNode("f", "現場物", "root", "folder", 0, path="現場物")
    child = PlaylistNode("p1", "HIPHOP&Latin", "f", "playlist", 0, path="現場物/HIPHOP&Latin")
    root.children.append(child)
    assert rekordbox_playlists.find_node([root], "現場物/hiphop&latin").id == "p1"
    assert rekordbox_playlists.find_node([root], "p1").id == "p1"
    twin = PlaylistNode("p2", "HIPHOP&Latin", "f", "playlist", 1, path="現場物/HIPHOP&Latin")
    root.children.append(twin)
    with pytest.raises(ValueError, match="複数"):
        rekordbox_playlists.find_node([root], "現場物/HIPHOP&Latin")
    assert rekordbox_playlists.clean_name("R\x10AP ") == "RAP"


# ----------------------------------------------------------------- service

def test_plan_uses_rekordbox_folders_as_phases(session: Session, fake_rekordbox):
    hiphop = [_track(session, f"hip{i}", artist=f"H{i}", bpm=95 + i, energy=0.4 + i * 0.05) for i in range(4)]
    latin = [_track(session, f"lat{i}", artist=f"L{i}", bpm=100 + i, energy=0.7) for i in range(3)]
    roots, entries = _tree(session, {"現場物/イベ2/hiphop": hiphop, "現場物/HIPHOP&Latin": latin})
    fake_rekordbox(roots, entries)

    result = SetPlannerService(session).plan([
        {"label": "イベ2", "count": 3, "rekordbox_playlists": ["現場物/イベ2"]},
        {"label": "HIPHOP&Latin", "count": 2, "rekordbox_playlists": ["現場物/HIPHOP&Latin"]},
    ])
    assert [t["phase"] for t in result["tracks"]] == ["イベ2"] * 3 + ["HIPHOP&Latin"] * 2
    assert {t["id"] for t in result["tracks"][:3]} <= {t.id for t in hiphop}
    assert {t["id"] for t in result["tracks"][3:]} <= {t.id for t in latin}
    assert result["tracks"][0]["sources"][0] == {"type": "playlist", "detail": "現場物/イベ2/hiphop"}
    assert result["phases"][0]["playlists"] == [{"path": "現場物/イベ2", "matched": 4, "unmatched": 0}]
    assert result["track_data"][0] == {"id": result["tracks"][0]["id"], "wordplay_json": None}


def test_unregistered_playlist_tracks_are_reported(session: Session, fake_rekordbox):
    known = _track(session, "known")
    ghost = Track(id=999, filepath="/elsewhere/ghost.mp3", title="Ghost", artist="G", genre="", duration=1)
    roots, entries = _tree(session, {"現場物/X": [known, ghost]})
    fake_rekordbox(roots, entries)

    service = SetPlannerService(session)
    listed = service.playlist_tracks("現場物/X")
    assert [t["id"] for t in listed["tracks"]] == [known.id]
    assert listed["unmatched_count"] == 1
    planned = service.plan([{"label": "x", "count": 2, "rekordbox_playlists": ["現場物/X"]}])
    assert planned["count"] == 1
    assert any("未登録" in note for note in planned["notes"])


def test_preferences_expand_with_artists_similar_tracks_and_wordplay(session: Session):
    vector = [1.0] + [0.0] * 199
    fav = _track(session, "fav", artist="Drake", embed=vector)
    fav_feat = _track(session, "fav2", artist="Future & Drake")
    alike = _track(session, "alike", artist="Other", embed=[0.99, 0.1] + [0.0] * 198)
    mine = _track(session, "mine", artist="Yutahhh", embed=[0.98, 0.1] + [0.0] * 198)
    wordplay_target = _track(session, "wp", artist="WP")
    session.add(WordplayPair(from_track_id=fav.id, to_track_id=wordplay_target.id, keyword="k",
                             normalized_keyword="k", source_phrase="k", target_phrase="k",
                             status="approved"))
    session.commit()

    result = SetPlannerService(session).plan(
        [{"label": "fav", "count": 5, "artists": ["drake"], "similar_to_track_ids": [fav.id]}],
        exclude_artists=["yutahhh"], wordplay_bonus=0.7,
    )
    by_id = {t["id"]: t for t in result["tracks"]}
    assert {fav.id, fav_feat.id, alike.id, wordplay_target.id} <= set(by_id)
    assert mine.id not in by_id
    assert {"type": "similar", "detail": fav.id} in by_id[alike.id]["sources"]
    ids = [t["id"] for t in result["tracks"]]
    assert ids.index(wordplay_target.id) == ids.index(fav.id) + 1
    assert json.loads(by_id[wordplay_target.id]["wordplay_json"])["keyword"] == "k"


def test_explicit_tracks_override_exclusions_and_plan_can_be_saved(session: Session):
    mine = _track(session, "mine", artist="Yutahhh")
    other = _track(session, "other", artist="Other")
    result = SetPlannerService(session).plan(
        [{"label": "p", "count": 2, "anchor_track_ids": [mine.id], "track_ids": [other.id]}],
        exclude_artists=["Yutahhh"], save_as_setlist_name="テストセット",
    )
    assert {t["id"] for t in result["tracks"]} == {mine.id, other.id}
    saved = session.get(Setlist, result["saved_setlist"]["id"])
    assert saved.name == "テストセット"


def test_phase_without_sources_falls_back_to_library_filters(session: Session):
    _track(session, "house", genre="House", bpm=124, energy=0.8)
    _track(session, "hip", genre="Hip-Hop", bpm=95, energy=0.5)
    result = SetPlannerService(session).plan([{"label": "house", "count": 2, "genres": ["House"]}])
    assert [t["genre"] for t in result["tracks"]] == ["House"]
    assert result["tracks"][0]["sources"] == [{"type": "filter"}]


def test_invalid_phase_count_is_rejected(session: Session):
    with pytest.raises(ValueError, match="count"):
        SetPlannerService(session).plan([{"label": "p", "count": 0}])
