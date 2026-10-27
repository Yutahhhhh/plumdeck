"""The DJ's own HISTORY as evidence: counted per recording, never from the live set."""
from datetime import datetime, timedelta

from domain.services import play_history
from domain.services.play_history import PlayHistory

NOW = datetime(2026, 10, 24, 20, 0)


def session(days_ago, *tracks, pad=True):
    tracks = list(tracks)
    if pad:
        tracks += [None] * max(0, play_history.MIN_SESSION_TRACKS - len(tracks))
    return NOW - timedelta(days=days_ago), tracks


def test_counts_what_followed_and_what_was_nearby():
    history = PlayHistory([session(30, "a", "b", "x", "y", "c"), session(10, "a", "b")], NOW)
    b = history.evidence("a", "b")
    assert (b.followed, b.nearby, b.plays) == (2, 2, 2)
    assert round(b.days_since) == 10
    c = history.evidence("a", "c")
    # Four positions apart is outside the window; only the plays count.
    assert (c.followed, c.nearby, c.plays) == (0, 0, 1)
    assert history.evidence("a", "never") is None
    assert history.related("a") == ["b", "x", "y"]


def test_only_evidence_about_the_source_lifts_a_pick():
    history = PlayHistory([session(30, "a", "b", "x", "c"), session(2, "d"), session(2, "d")], NOW)
    # Mixed straight out of the source beats merely being a few tracks away.
    assert history.evidence("a", "b").affinity > history.evidence("a", "c").affinity > 0
    # Being in the DJ's current rotation says nothing about mixing out of "a".
    rotation = history.evidence("a", "d")
    assert rotation.plays == 2 and rotation.affinity == 0


def test_previews_and_the_set_being_played_now_are_ignored():
    live = (NOW - timedelta(hours=2), ["a", "b"] + [None] * 10)
    preview = session(30, "a", "c", pad=False)
    history = PlayHistory([live, preview], NOW)
    assert history.evidence("a", "b") is None
    assert history.evidence("a", "c") is None


def test_unknown_files_keep_positions_without_linking_tracks():
    history = PlayHistory([session(5, "a", None, "b")], NOW)
    b = history.evidence("a", "b")
    assert b.followed == 0 and b.nearby == 1


def test_the_reason_names_the_evidence():
    history = PlayHistory([session(3, "a", "b")], NOW)
    text = play_history.describe(history.evidence("a", "b"))
    assert "直後に 1 回" in text and "3日前" in text
    assert play_history.headline(history.evidence("a", "b")) == "実績：直後に1回"
