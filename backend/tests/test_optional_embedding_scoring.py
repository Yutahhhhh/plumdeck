"""Missing embeddings must not count as evidence that two tracks differ."""
import json

import numpy as np
import pytest

from app.services.setlist_app_service import SetlistAppService
from domain.models.track import Track, TrackEmbedding
from domain.services import assist_recommendation
from domain.services.phased_setlist_builder import transition_score
from domain.services.setlist_builder import SetlistBuilder
from utils.audio_math import calculate_mixability_score
from utils.embedding import cosine_similarity


@pytest.mark.parametrize("left,right,left_model,right_model", [
    (None, [1, 0], None, None),
    ([1, 0], None, None, None),
    ([1, 0], [1, 0], "musicnn", "other-pipeline"),
    ([0, 0], [1, 0], None, None),
    ([1, 0], [0, 0], None, None),
    ([1, float("nan")], [1, 0], None, None),
    ([1, float("inf")], [1, 0], None, None),
    ([1, 0], [1], None, None),
    (["invalid"], [1], None, None),
])
def test_unavailable_vectors_are_not_zero_similarity(left, right, left_model, right_model):
    assert cosine_similarity(left, right, left_model, right_model) is None


def test_comparable_vectors_keep_measured_similarity():
    assert cosine_similarity([1, 0], [0, 1]) == 0.0
    assert cosine_similarity([1, 0], [-1, 0]) == -1.0
    assert cosine_similarity([1, 0], [1, 0], "musicnn", "msd-musicnn-1") == 1.0


def test_mixability_ignores_key_and_renormalizes_only_unavailable_vectors():
    missing = calculate_mixability_score(120, "8A", 120, "3B", None)
    measured = calculate_mixability_score(120, "8A", 120, "8A", 1.0)
    orthogonal = calculate_mixability_score(120, "8A", 120, "8A", 0.0)
    assert missing == measured == 1.0
    assert orthogonal == pytest.approx(0.35 / 0.75)
    assert calculate_mixability_score(120, None, 120, None, float("nan")) == 1.0
    # Explicit callers may still request harmonic matching.
    assert calculate_mixability_score(120, "8A", 120, "3B", None,
                                     weights={"bpm": 0.5, "key": 0.5, "vector": 0}) == pytest.approx(0.55)


def _node(track_id, vector=None, model="musicnn", key="8A"):
    return {"id": track_id, "vector": vector, "embedding_model": model,
            "track": Track(id=track_id, filepath=f"/scoring/{track_id}.mp3",
                           title=f"Track {track_id}", artist=f"Artist {track_id}",
                           bpm=120, key=key, energy=0.5, duration=180)}


@pytest.mark.parametrize("candidate", [
    _node(2, None, key="3B"),
    _node(2, [1, 0], "other-pipeline", key="3B"),
    _node(2, [1, 0], key="3B"),
])
def test_chain_and_phased_transitions_share_optional_embedding_semantics(candidate):
    source = _node(1, [1, 0])
    assert SetlistBuilder()._calculate_transition_score(source, candidate) == 1.0
    assert transition_score(source, candidate) == 1.0


def test_bridge_can_prefer_pending_embedding_to_measured_dissimilarity():
    start, end = _node(1, [1, 0]), _node(4, [1, 0])
    pending, dissimilar = _node(2), _node(3, [0, 1])
    path = SetlistBuilder().build_path([dissimilar, pending], start, end, 3)
    assert [track.id for track in path] == [1, 2, 4]


def test_assist_renormalizes_incompatible_embeddings_and_explains_pending():
    source = {"bpm": 120, "key": "8A", "genre": "House", "year": 2020,
              "energy": 0.5, "danceability": 0.5, "brightness": 0.5, "noisiness": 0.5}
    missing = assist_recommendation.evaluate(source, source, "keep", None, has_analysis=False)
    incompatible = assist_recommendation.evaluate(
        source, source, "keep", cosine_similarity([1, 0], [1, 0], "musicnn", "other"))
    assert missing.score == incompatible.score
    assert "vector" not in missing.components
    assert any(reason.text == "音色の比較は計算待ち" for reason in missing.reasons)


@pytest.mark.parametrize("target_has_embedding", [True, False])
def test_python_and_sql_recommendations_do_not_penalize_pending_or_unusable_embeddings(session, target_has_embedding):
    target = _node(100)["track"]
    candidates = [_node(number, key="3B" if number % 2 else "8A")["track"] for number in range(101, 108)]
    session.add_all([target, *candidates])
    session.commit()
    vector = json.dumps([1.0] * 200)
    if target_has_embedding:
        session.add(TrackEmbedding(track_id=target.id, model_name="musicnn", embedding_json=vector))
    for candidate, model, value in zip(candidates, ["musicnn", "other", "musicnn", "musicnn", "musicnn", "musicnn"],
                                        [vector, vector, "bad", json.dumps([0.0] * 200),
                                         json.dumps([None] + [1.0] * 199), json.dumps([float("nan")] + [1.0] * 199)]):
        session.add(TrackEmbedding(track_id=candidate.id, model_name=model, embedding_json=value))
    session.commit()
    service = SetlistAppService(session)
    paged = service.recommend_next_track_page(target.id, limit=20)
    legacy = service.recommend_next_track(target.id, limit=20)
    assert [track["id"] for track in paged["items"]] == [track["id"] for track in legacy]
    assert {item["id"] for item in paged["items"]} == {track.id for track in candidates}
    assert all(np.isfinite(item["recommendation_score"]) for item in paged["items"])
    assert [item["recommendation_score"] for item in paged["items"]] == pytest.approx([1.0] * len(candidates))
