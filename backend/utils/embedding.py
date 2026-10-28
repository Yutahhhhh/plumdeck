"""Keep embeddings from different preprocessing pipelines out of the same space."""
import numpy as np

LEGACY_MODELS = ("musicnn", "msd-musicnn-1", "")


def embedding_space(model):
    return "musicnn" if model is None or model in LEGACY_MODELS else model


def cosine_similarity(left, right, left_model=None, right_model=None):
    """Return None when there is no comparable evidence, not a dissimilarity."""
    if embedding_space(left_model) != embedding_space(right_model):
        return None
    if left is None or right is None:
        return None
    try:
        left, right = np.asarray(left, dtype=float), np.asarray(right, dtype=float)
    except (TypeError, ValueError):
        return None
    if left.ndim != 1 or left.shape != right.shape or not left.size:
        return None
    if not np.isfinite(left).all() or not np.isfinite(right).all():
        return None
    with np.errstate(over="ignore", invalid="ignore"):
        denominator = np.linalg.norm(left) * np.linalg.norm(right)
        if not np.isfinite(denominator) or denominator <= 0:
            return None
        return float(np.clip(np.dot(left, right) / denominator, -1, 1))


def neutral_similarity(similarities):
    """Median of the measured similarities in one candidate set, or None.

    A candidate whose embedding is still pending takes this value: it ranks
    like a typical candidate. Leaving the component out and renormalizing
    instead lifts pending tracks above every measured one (most measured
    similarities are below the tempo score), and scoring 0 buries them.
    """
    values = [value for value in similarities if value is not None and np.isfinite(value)]
    return float(np.median(values)) if values else None
