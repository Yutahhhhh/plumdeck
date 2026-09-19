"""Assist mode: deck identity resolution and intent-driven suggestions.

The failure that matters here is a confident wrong answer — recommending
transitions out of a track that is not actually playing, or offering a candidate
the DJ cannot drag onto a deck. These tests pin the cases where the correct
behaviour is to refuse rather than to guess.
"""
import pytest

from app.services.assist_app_service import (
    STATUS_EMPTY,
    STATUS_NOT_IN_LIBRARY,
    STATUS_RESOLVED,
    STATUS_UNRESOLVED,
    AssistAppService,
)
from domain.models.track import Track, TrackEmbedding
from domain.models.wordplay import WordplayPair
from domain.services import assist_recommendation as scoring
from infra import rekordbox_library
from infra.rekordbox_library import RekordboxEntry, RekordboxLibraryUnavailable


CLICK = "/Applications/rekordbox 7/rekordbox.app/Contents/Resources/binary/Click Sound 01.wav"
SAMPLER = "/Users/dj/Music/PioneerDJ/Sampler/OSC_SAMPLER/PRESET ONESHOT/HORN.wav"


def make_track(session, path, **overrides):
    track = Track(
        filepath=path,
        title=overrides.pop("title", "Bad Girl"),
        artist=overrides.pop("artist", "Usher"),
        genre=overrides.pop("genre", "R&B"),
        bpm=overrides.pop("bpm", 88.0),
        key=overrides.pop("key", "10A"),
        duration=overrides.pop("duration", 261.0),
        energy=overrides.pop("energy", 0.5),
        danceability=overrides.pop("danceability", 0.5),
        noisiness=overrides.pop("noisiness", 0.5),
        year=overrides.pop("year", 2010),
        **overrides,
    )
    session.add(track)
    session.commit()
    session.refresh(track)
    return track


def entry(path, title, artist, content_id="1", bpm=None):
    return RekordboxEntry(
        content_id=content_id, filepath=path, title=title, artist=artist, bpm=bpm
    )


def patch_collection(mocker, entries, registered=None):
    mocker.patch.object(
        rekordbox_library,
        "lookup_by_paths",
        lambda paths: {rekordbox_library.normalize_path(e.filepath): e for e in entries},
    )
    mocker.patch.object(
        rekordbox_library,
        "lookup_by_identities",
        lambda identities: {
            rekordbox_library.normalize_path(e.filepath): e
            for e in entries
            if any(
                rekordbox_library.normalize_text(e.title)
                == rekordbox_library.normalize_text(title)
                and rekordbox_library.normalize_text(e.artist)
                == rekordbox_library.normalize_text(artist)
                for title, artist in identities
            )
        },
    )
    if registered is not None:
        mocker.patch.object(
            rekordbox_library,
            "registered_paths",
            lambda: frozenset(rekordbox_library.normalize_path(p) for p in registered),
        )


def observation(slot, title, artist, loaded=True, **extra):
    return {
        "slot": slot,
        "loaded": loaded,
        "title": title,
        "artist": artist,
        "track_bpm": extra.get("track_bpm"),
        "tempo_bpm": extra.get("tempo_bpm"),
        "display_key": extra.get("display_key"),
    }


# --------------------------------------------------------------- resolution


def test_the_sampler_and_click_files_never_become_deck_tracks(session, mocker, tmp_path):
    audio = tmp_path / "Bad Girl.mp3"
    audio.write_bytes(b"")
    track = make_track(session, str(audio))
    patch_collection(
        mocker,
        [
            entry(CLICK, "Click Sound 01", "", content_id="10"),
            entry(SAMPLER, "HORN", "", content_id="11"),
            entry(str(audio), "Bad Girl", "Usher", content_id="94136785"),
        ],
    )

    result = AssistAppService(session).resolve_decks(
        [observation(1, "Bad Girl", "Usher")], [CLICK, SAMPLER, str(audio)]
    )
    deck = result["decks"][0]
    assert deck["status"] == STATUS_RESOLVED
    assert deck["track"]["id"] == track.id
    assert deck["rekordbox_id"] == "94136785"
    assert deck["match_confidence"] == "title_and_artist"


def test_two_open_copies_of_the_same_track_choose_one_deterministically(session, mocker, tmp_path):
    first = tmp_path / "a" / "Bad Girl.mp3"
    second = tmp_path / "b" / "Bad Girl.mp3"
    for path in (first, second):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"")
        make_track(session, str(path))
    patch_collection(
        mocker,
        [
            entry(str(first), "Bad Girl", "Usher", content_id="1"),
            entry(str(second), "Bad Girl", "Usher", content_id="2"),
        ],
    )

    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Bad Girl", "Usher")], [str(first), str(second)]
    )["decks"][0]
    assert deck["status"] == STATUS_RESOLVED
    assert deck["track"] is not None
    assert deck["rekordbox_id"] == "1"


def test_exact_collection_identity_falls_back_when_open_paths_miss_track(
    session, mocker, tmp_path
):
    audio = tmp_path / "Bad Girl.mp3"
    audio.write_bytes(b"")
    track = make_track(session, str(audio))
    collection_entry = entry(str(audio), "Bad Girl", "Usher", content_id="9")
    patch_collection(mocker, [collection_entry])
    mocker.patch.object(rekordbox_library, "lookup_by_paths", return_value={})

    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Bad Girl", "Usher")], [SAMPLER]
    )["decks"][0]

    assert deck["status"] == STATUS_RESOLVED
    assert deck["track"]["id"] == track.id
    assert deck["rekordbox_id"] == "9"


def test_contradictory_artist_does_not_resolve_by_title(session, mocker, tmp_path):
    audio = tmp_path / "Think About You.mp3"
    audio.write_bytes(b"")
    make_track(session, str(audio), title="Think About You", artist="Kygo")
    # rekordbox shows the full credit list; the collection row has only one name.
    patch_collection(mocker, [entry(str(audio), "Think About You", "Kygo", content_id="7")])

    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Think About You", "Kygo, Valerie Broussard")], [str(audio)]
    )["decks"][0]
    assert deck["status"] == STATUS_UNRESOLVED
    assert deck["track"] is None


def test_a_deck_whose_file_is_not_open_stays_unresolved(session, mocker):
    patch_collection(mocker, [entry(SAMPLER, "HORN", "")])
    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Bad Girl", "Usher")], [SAMPLER]
    )["decks"][0]
    assert deck["status"] == STATUS_UNRESOLVED
    assert deck["track"] is None


def test_an_empty_deck_is_not_treated_as_a_failure(session, mocker):
    patch_collection(mocker, [])
    deck = AssistAppService(session).resolve_decks(
        [observation(2, None, None, loaded=False)], []
    )["decks"][0]
    assert deck["status"] == STATUS_EMPTY


def test_a_track_rekordbox_knows_but_plumdeck_does_not_is_flagged(session, mocker, tmp_path):
    audio = tmp_path / "Unknown.mp3"
    audio.write_bytes(b"")
    patch_collection(mocker, [entry(str(audio), "Unknown", "Nobody", content_id="55")])
    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Unknown", "Nobody")], [str(audio)]
    )["decks"][0]
    assert deck["status"] == STATUS_NOT_IN_LIBRARY
    assert deck["rekordbox_id"] == "55"
    assert deck["track"] is None


def test_an_unreadable_rekordbox_library_degrades_without_guessing(session, mocker):
    def unavailable(_paths):
        raise RekordboxLibraryUnavailable("master.db を読み取れません")

    mocker.patch.object(rekordbox_library, "lookup_by_paths", unavailable)
    result = AssistAppService(session).resolve_decks(
        [observation(1, "Bad Girl", "Usher")], ["/tmp/x.mp3"]
    )
    assert result["library_error"] == "master.db を読み取れません"
    assert result["decks"][0]["status"] == STATUS_UNRESOLVED


# ------------------------------------------------------------ recommendation


@pytest.fixture
def library(session, tmp_path, mocker):
    """A small library whose files all exist and are all known to rekordbox."""
    tracks = {}
    specs = {
        "source": dict(title="Bad Girl", artist="Usher", bpm=88.0, key="10A", energy=0.50,
                       danceability=0.55, noisiness=0.40, genre="R&B"),
        "near": dict(title="Close Groove", artist="A", bpm=89.0, key="10A", energy=0.52,
                     danceability=0.57, noisiness=0.42, genre="R&B"),
        "far_tempo": dict(title="Way Faster", artist="B", bpm=112.0, key="10A", energy=0.55,
                          danceability=0.58, noisiness=0.44, genre="R&B"),
        "contrast": dict(title="Different Room", artist="C", bpm=90.0, key="11A", energy=0.85,
                         danceability=0.90, noisiness=0.80, genre="Techno"),
    }
    for name, spec in specs.items():
        path = tmp_path / f"{name}.mp3"
        path.write_bytes(b"")
        tracks[name] = make_track(session, str(path), **spec)
    mocker.patch.object(
        rekordbox_library,
        "registered_paths",
        lambda: frozenset(
            rekordbox_library.normalize_path(track.filepath) for track in tracks.values()
        ),
    )
    return tracks


def test_groove_refuses_candidates_that_leave_the_pocket(session, library):
    result = AssistAppService(session).recommend(library["source"].id, "groove")
    ids = [candidate["id"] for candidate in result["candidates"]]
    assert library["near"].id in ids
    # 112 vs 88 BPM is a 27% jump; it is a valid mix, but it is not "keep the groove".
    assert library["far_tempo"].id not in ids


def test_presets_move_the_floor_in_their_own_direction(session, library):
    service = AssistAppService(session)
    loud = library["contrast"].id  # +0.35 energy, +2% tempo, key +1
    hype = [c["id"] for c in service.recommend(library["source"].id, "hype")["candidates"]]
    calm = [c["id"] for c in service.recommend(library["source"].id, "calm")["candidates"]]
    assert hype[0] == loud
    # +2.3% is outside calm's tempo window (it may slow down, barely speed up).
    assert library["near"].id in calm and loud not in calm


def test_legacy_groove_is_keep(session, library):
    result = AssistAppService(session).recommend(library["source"].id, "groove")
    assert result["intent"] == "keep"


def test_a_candidate_explains_the_preset_with_measured_moves(session, library):
    result = AssistAppService(session).recommend(library["source"].id, "hype")
    top = result["candidates"][0]
    assert top["summary"].startswith("盛り上げる向き")
    assert "エネルギー +0.35" in top["summary"]
    assert "9A→" not in top["summary"] and "10A→11A（+1）" in top["summary"]


def test_other_presets_offer_their_own_best_pick(session, library):
    result = AssistAppService(session).recommend(library["source"].id, "keep", limit=1)
    main = {c["id"] for c in result["candidates"]}
    labels = {item["label"] for item in result["alternatives"]}
    assert "盛り上げるなら" in labels
    assert all(item["track"]["id"] not in main for item in result["alternatives"])
    assert "keep" not in {item["intent"] for item in result["alternatives"]}


def test_candidates_outside_the_rekordbox_collection_are_dropped(session, library, mocker):
    mocker.patch.object(
        rekordbox_library,
        "registered_paths",
        lambda: frozenset({rekordbox_library.normalize_path(library["source"].filepath)}),
    )
    result = AssistAppService(session).recommend(library["source"].id, "keep")
    assert result["candidates"] == []
    assert result["unavailable_originals"] >= 1
    assert any("rekordbox 未登録" in note for note in result["notes"])


def test_a_missing_file_is_not_offered_even_when_rekordbox_knows_it(session, library):
    import os

    os.remove(library["near"].filepath)
    result = AssistAppService(session).recommend(library["source"].id, "keep")
    assert library["near"].id not in [candidate["id"] for candidate in result["candidates"]]


def test_an_unreadable_collection_stops_recommendations(session, library, mocker):
    def unavailable():
        raise RekordboxLibraryUnavailable("master.db を読み取れません")

    mocker.patch.object(rekordbox_library, "registered_paths", unavailable)
    result = AssistAppService(session).recommend(library["source"].id, "keep")
    assert result["candidates"] == []
    assert any("master.db" in note for note in result["notes"])


def test_wordplay_without_an_approved_pair_returns_nothing_rather_than_a_substitute(
    session, library
):
    result = AssistAppService(session).recommend(library["source"].id, "wordplay")
    assert result["candidates"] == []
    assert result["notes"]


def test_wordplay_uses_only_approved_directed_pairs_and_keeps_the_caveat(session, library):
    pair = WordplayPair(
        from_track_id=library["source"].id,
        to_track_id=library["contrast"].id,
        keyword="girl",
        normalized_keyword="girl",
        source_phrase="bad girl",
        target_phrase="girl like you",
        status="pending",
    )
    session.add(pair)
    session.commit()
    service = AssistAppService(session)
    assert service.recommend(library["source"].id, "wordplay")["candidates"] == []

    pair.status = "approved"
    session.add(pair)
    session.commit()
    result = service.recommend(library["source"].id, "wordplay")
    assert [candidate["id"] for candidate in result["candidates"]] == [library["contrast"].id]
    reasons = result["candidates"][0]["reasons"]
    wordplay_reason = next(reason for reason in reasons if reason["kind"] == "wordplay")
    assert "未検証" in wordplay_reason["text"]

    # The reverse direction is a different transition and must not be implied.
    assert service.recommend(library["contrast"].id, "wordplay")["candidates"] == []


def test_the_track_on_the_other_deck_can_be_excluded(session, library):
    result = AssistAppService(session).recommend(
        library["source"].id, "keep", exclude_track_ids=[library["contrast"].id]
    )
    assert library["contrast"].id not in [c["id"] for c in result["candidates"]]


def test_a_track_without_analysis_is_still_ranked_and_says_what_is_missing(session, library):
    session.add(
        TrackEmbedding(
            track_id=library["source"].id,
            model_name="discogs-effnet",
            embedding_json="[0.1, 0.2, 0.3]",
        )
    )
    session.commit()
    result = AssistAppService(session).recommend(library["source"].id, "keep")
    candidate = result["candidates"][0]
    similarity = next(r for r in candidate["reasons"] if r["kind"] == "similarity")
    assert "解析" in similarity["text"] or "音色比較" in similarity["text"]
    assert candidate["score"] > 0


def test_an_unknown_intent_is_rejected(session, library):
    with pytest.raises(ValueError):
        AssistAppService(session).recommend(library["source"].id, "vibes")


# ------------------------------------------------------------------ scoring


def test_missing_key_information_is_dropped_rather_than_scored_as_neutral():
    known = scoring.evaluate(
        {"bpm": 120, "key": "8A", "energy": 0.5},
        {"bpm": 120, "key": "9A", "energy": 0.5},
        "groove",
    )
    unknown = scoring.evaluate(
        {"bpm": 120, "key": "8A", "energy": 0.5},
        {"bpm": 120, "key": "", "energy": 0.5},
        "groove",
    )
    assert "key" in known.components
    assert "key" not in unknown.components
    # Dropping the component renormalizes; it must not silently become a penalty
    # nor a free pass.
    assert unknown.score == pytest.approx(1.0)
    assert any(reason["kind"] == "key" and reason["tone"] == "caution"
               for reason in [r.to_dict() for r in unknown.reasons])


def test_feature_direction_follows_the_preset():
    quiet = {"energy": 0.4}
    loud = {"energy": 0.7}
    assert scoring.feature_score(quiet, loud, "energy", "up") > 0.5
    assert scoring.feature_score(quiet, loud, "energy", "down") < 0.5
    assert scoring.feature_score(quiet, quiet, "energy", "hold") == pytest.approx(1.0)
    assert scoring.feature_score({}, loud, "energy", "up") is None


def test_half_time_transitions_are_recognised_as_compatible():
    result = scoring.evaluate(
        {"bpm": 174, "key": "8A", "energy": 0.5},
        {"bpm": 87, "key": "8A", "energy": 0.5},
        "groove",
    )
    tempo = next(r for r in result.reasons if r.kind == "tempo")
    assert "2倍換算" in tempo.text
    assert result.components["bpm"] == pytest.approx(scoring.HALF_DOUBLE_TEMPO_FACTOR)
    assert scoring.tempo_score(174, 174) > result.components["bpm"]


def test_cross_genre_continuity_is_soft_but_visible():
    source = {"bpm": 120, "key": "8A", "energy": 0.5, "genre": "House", "subgenre": "Deep House"}
    same = {"bpm": 121, "key": "8A", "energy": 0.5, "genre": "House", "subgenre": "Deep House"}
    cross = {"bpm": 121, "key": "8A", "energy": 0.5, "genre": "Latin", "subgenre": "House"}
    same_result = scoring.evaluate(source, same, "keep")
    cross_result = scoring.evaluate(source, cross, "keep")
    assert cross_result.components["continuity"] == 0.0
    assert cross_result.score < same_result.score
    continuity = next(reason for reason in cross_result.reasons if reason.kind == "continuity")
    assert continuity.tone == "caution"


def test_similarity_is_never_described_as_a_groove_match():
    result = scoring.evaluate(
        {"bpm": 120, "key": "8A", "energy": 0.5},
        {"bpm": 121, "key": "8A", "energy": 0.5},
        "groove",
        vector_similarity=0.93,
    )
    similarity = next(r for r in result.reasons if r.kind == "similarity")
    assert similarity.text == "音色の傾向が近い"
    assert similarity.tone == "neutral"


def test_ranking_is_deterministic_for_equal_scores():
    left = ({"id": 9}, scoring.Scored(score=0.5))
    right = ({"id": 2}, scoring.Scored(score=0.5))
    assert [item[0]["id"] for item in scoring.rank([left, right], 2)] == [2, 9]
    assert [item[0]["id"] for item in scoring.rank([right, left], 2)] == [2, 9]


@pytest.mark.parametrize("artist", ["", None])
def test_missing_observed_artist_does_not_match_named_artist(session, mocker, artist):
    patch_collection(mocker, [entry(SAMPLER, "Song", "Someone")])
    deck = AssistAppService(session).resolve_decks(
        [observation(1, "Song", artist)], [SAMPLER]
    )["decks"][0]
    assert deck["status"] == STATUS_UNRESOLVED


def test_unicode_path_spellings_preserve_original_identity(session, mocker, tmp_path):
    import unicodedata
    import sqlite3

    original = str(tmp_path / "café.mp3")
    decomposed = unicodedata.normalize("NFD", original)
    # Model a normalization-insensitive macOS volume, regardless of test host.
    mocker.patch.object(rekordbox_library.os.path, "samefile", return_value=True)
    connection = sqlite3.connect(":memory:")
    connection.executescript("CREATE TABLE djmdArtist (ID TEXT, Name TEXT); "
                             "CREATE TABLE djmdContent (FolderPath TEXT, ID TEXT, Title TEXT, "
                             "ArtistID TEXT, BPM INTEGER, rb_local_deleted INTEGER);")
    connection.execute("INSERT INTO djmdArtist VALUES ('a', 'Usher')")
    connection.execute("INSERT INTO djmdContent VALUES (?, '1', 'Bad Girl', 'a', 8800, 0)",
                       (decomposed,))
    mocker.patch.object(rekordbox_library, "_database", return_value=tmp_path / "master.db")
    mocker.patch.object(rekordbox_library, "_connect", return_value=connection)
    entries = rekordbox_library.lookup_by_paths([original])
    assert list(entries) == [decomposed]
    track = make_track(session, original)
    assert AssistAppService(session)._tracks_by_filepath([decomposed])[decomposed].id == track.id
    assert rekordbox_library.is_registered_path(original, frozenset({decomposed}))
    mocker.patch.object(rekordbox_library.os.path, "samefile", return_value=False)
    assert not AssistAppService(session)._tracks_by_filepath([decomposed])
    assert not rekordbox_library.is_registered_path(original, frozenset({decomposed}))
    assert not rekordbox_library.same_path(original, original.upper())


def test_collection_cache_reloads_for_wal_changes_and_database_identity(mocker, tmp_path):
    first = tmp_path / "master.db"
    second = tmp_path / "other.db"
    first.write_bytes(b"db")
    second.write_bytes(b"db")
    db = mocker.patch.object(rekordbox_library, "_database", return_value=first)
    cache = rekordbox_library._CollectionPathCache()
    loader = mocker.patch.object(cache, "_load", side_effect=[frozenset({"a"}),
                                frozenset({"a", "b"}), frozenset({"c"})])
    assert cache.get() == frozenset({"a"})
    assert cache.get() == frozenset({"a"})
    (tmp_path / "master.db-wal").write_bytes(b"new rows")
    assert cache.get() == frozenset({"a", "b"})
    db.return_value = second
    assert cache.get() == frozenset({"c"})
    assert loader.call_count == 3


def test_tested_wordplay_edge_preferred_over_unverified(session, library):
    for verification in ("unverified", "tested"):
        session.add(WordplayPair(from_track_id=library["source"].id,
                    to_track_id=library["near"].id, keyword=verification,
                    normalized_keyword=verification, source_phrase="girl", target_phrase="girl",
                    status="approved",
                    verification_status=verification))
    session.commit()
    pair = AssistAppService(session)._approved_pairs(library["source"].id)[library["near"].id]
    assert pair["verification_status"] == "tested"
    assert scoring.wordplay_score(pair) == 1.0


@pytest.mark.parametrize("source,candidate,expected", [
    (88, 180, "BPM 88 → 90（+2.3%、原曲180 BPMの1/2換算）"),
    (174, 87, "BPM 174 → 174（+0.0%、原曲87 BPMの2倍換算）"),
    (100, 102, "BPM 100 → 102（+2.0%）"),
])
def test_tempo_reason_uses_effective_matched_tempo(source, candidate, expected):
    result = scoring.evaluate({"bpm": source}, {"bpm": candidate}, "groove")
    assert next(reason.text for reason in result.reasons if reason.kind == "tempo") == expected


@pytest.mark.parametrize("energy,expected", [(0.51, "エネルギーを維持"),
                         (0.8, "エネルギーを上げる"), (0.2, "エネルギーを下げる")])
def test_energy_reason_describes_actual_direction_without_raw_values(energy, expected):
    result = scoring.evaluate({"energy": 0.5}, {"energy": energy}, "keep")
    assert next(reason.text for reason in result.reasons if reason.kind == "energy") == expected
    assert "energy" in result.components


@pytest.mark.parametrize("scope,field", [("same_genre", "genre"), ("same_subgenre", "subgenre")])
def test_genre_scope_applies_normalized_exact_match_before_pool_cap(session, library, mocker, scope, field):
    setattr(library["source"], field, " House ")
    setattr(library["near"], field, "ＨＯＵＳＥ")
    setattr(library["contrast"], field, "House Techno")
    for track in library.values():
        session.add(track)
    session.commit()
    mocker.patch("app.services.assist_app_service.MAX_CANDIDATE_POOL", 1)
    result = AssistAppService(session).recommend(library["source"].id, "keep", genre_scope=scope)
    assert [candidate["id"] for candidate in result["candidates"]] == [library["near"].id]


@pytest.mark.parametrize("scope,field", [("same_genre", "genre"), ("same_subgenre", "subgenre")])
def test_missing_source_scope_metadata_never_relaxes_filter(session, library, scope, field):
    setattr(library["source"], field, " ")
    session.add(library["source"])
    session.commit()
    result = AssistAppService(session).recommend(library["source"].id, "keep", genre_scope=scope)
    assert result["candidates"] == []
    assert "未登録" in result["notes"][0]


def test_wordplay_obeys_genre_scope(session, library):
    session.add(WordplayPair(from_track_id=library["source"].id,
                to_track_id=library["contrast"].id, keyword="girl", normalized_keyword="girl",
                source_phrase="girl", target_phrase="girl", status="approved"))
    session.commit()
    result = AssistAppService(session).recommend(library["source"].id, "wordplay", genre_scope="same_genre")
    assert result["candidates"] == []


def test_recording_history_excludes_duplicates_but_preserves_remixes(session, library, tmp_path):
    for name, title, artist in [("copy", " CLOSE GROOVE ", "Ａ"),
                                ("source_copy", "bad girl", "USHER"),
                                ("remix", "Close Groove (Club Remix)", "A")]:
        path = tmp_path / f"{name}.mp3"
        path.write_bytes(b"")
        library[name] = make_track(session, str(path), title=title, artist=artist)
    service = AssistAppService(session)
    initial = service.recommend(library["source"].id, "keep")
    ids = [candidate["id"] for candidate in initial["candidates"]]
    assert len(set(ids) & {library["near"].id, library["copy"].id}) == 1
    assert library["source_copy"].id not in ids
    result = service.recommend(library["source"].id, "keep", exclude_track_ids=[library["near"].id])
    ids = [candidate["id"] for candidate in result["candidates"]]
    assert not set(ids) & {library["near"].id, library["copy"].id, library["source_copy"].id}
    assert library["remix"].id in ids


def test_assist_history_request_accepts_long_session():
    from api.schemas.assist import AssistRecommendRequest
    request = AssistRecommendRequest(source_track_id=1, exclude_track_ids=list(range(10000)),
                                     genre_scope="same_subgenre")
    assert len(request.exclude_track_ids) == 10000
