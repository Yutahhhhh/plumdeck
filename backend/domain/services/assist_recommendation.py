"""Scoring for assist-mode "what should I play next" suggestions.

The DJ picks which way to move the floor — a *preset* — and every preset reads
the same measurements (tempo, Camelot key movement, analysed features, release
year) in its own direction rather than reordering one fixed score:

* ``keep``      — stay in the current pocket.
* ``hype``      — raise the heat: energy up, while preserving a playable
  stylistic bridge.
* ``dance``     — more groove: danceability up, while preserving a playable
  stylistic bridge.
* ``calm``      — cool down: energy and noise down, key -1 / to minor.
* ``emotional`` — deeper and more wistful: minor keys, darker tone, energy held.
* ``bright``    — open and euphoric: major keys, brighter tone, energy held.
* ``throwback`` — reach back in time: at least five years older, still mixable.
* ``wordplay``  — only tracks with a human-approved wordplay edge out of the
                  source, ranked by how playable that transition is.

Independently of the preset, *transition* mode swaps the tempo rule: instead of
staying near the current BPM it looks for half/double-time partners and for
tempo-changing edits whose title declares the change ("100-128 Transition").

A component whose inputs are missing is dropped and the remaining weights are
renormalized. Substituting a neutral value would quietly turn "we do not know"
into "this is fine", which is exactly the failure a DJ cannot afford mid-set.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import math
import re
from typing import Any, Iterable, Optional

from utils.audio_math import bpm_distance, normalize_key


@dataclass(frozen=True)
class Preset:
    label: str
    detail: str
    # Hard tempo window on the matched (half/double-aware) tempo, in percent.
    tempo_window: tuple[float, float]
    # Where inside that window the tempo is best, in percent.
    tempo_center: float
    # feature -> "up" | "down" | "hold" | "not_down" | "not_up"
    directions: dict[str, str]
    weights: dict[str, float]


PRESETS: dict[str, Preset] = {
    "keep": Preset(
        "キープ", "今の流れを保つ", (-6.0, 6.0), 0.0,
        {"energy": "hold", "danceability": "hold", "noisiness": "hold"},
        {"bpm": 0.25, "key": 0.17, "vector": 0.16, "continuity": 0.22,
         "energy": 0.08, "danceability": 0.06, "noisiness": 0.06},
    ),
    "hype": Preset(
        "盛り上げる", "熱量を上げる", (-2.0, 8.0), 3.0,
        {"energy": "up", "noisiness": "not_down"},
        {"bpm": 0.20, "key": 0.19, "vector": 0.06, "continuity": 0.04,
         "energy": 0.37, "noisiness": 0.14},
    ),
    "dance": Preset(
        "踊らせる", "体が動くグルーヴへ", (-6.0, 6.0), 0.0,
        {"danceability": "up", "energy": "not_down"},
        {"bpm": 0.20, "key": 0.12, "vector": 0.10, "continuity": 0.08,
         "danceability": 0.36, "energy": 0.14},
    ),
    "calm": Preset(
        "落ち着かせる", "クールダウン", (-8.0, 2.0), -3.0,
        {"energy": "down", "noisiness": "down"},
        {"bpm": 0.18, "key": 0.17, "vector": 0.07, "continuity": 0.08,
         "energy": 0.34, "noisiness": 0.16},
    ),
    "emotional": Preset(
        "エモく", "切なく・深く", (-6.0, 6.0), 0.0,
        {"brightness": "down", "energy": "hold"},
        {"bpm": 0.18, "key": 0.23, "vector": 0.08, "continuity": 0.08,
         "brightness": 0.23, "energy": 0.20},
    ),
    "bright": Preset(
        "明るく", "開放感・多幸感へ", (-6.0, 6.0), 0.0,
        {"brightness": "up", "energy": "hold"},
        {"bpm": 0.18, "key": 0.23, "vector": 0.08, "continuity": 0.08,
         "brightness": 0.23, "energy": 0.20},
    ),
    "throwback": Preset(
        "時代を戻す", "懐かしい曲で沸かせる", (-6.0, 6.0), 0.0,
        {"energy": "hold", "danceability": "hold"},
        {"bpm": 0.20, "key": 0.15, "vector": 0.08, "continuity": 0.08,
         "era": 0.30, "energy": 0.10, "danceability": 0.09},
    ),
    "wordplay": Preset(
        "ワードプレイ", "言葉でつなぐ", (-100.0, 100.0), 0.0,
        {"energy": "hold"},
        {"wordplay": 0.40, "bpm": 0.24, "key": 0.18, "vector": 0.08, "energy": 0.10},
    ),
}
INTENTS = tuple(PRESETS)
MOOD_PRESETS = tuple(name for name in PRESETS if name != "wordplay")
# Older API values keep working.
LEGACY_INTENTS = {"groove": "keep"}

# How far each feature has to move to count as a full step in its direction.
# Scaled to the library's spread: danceability and brightness vary far less
# than energy, so a 0.15 step would never be reached on them.
FEATURE_STEPS: dict[str, float] = {
    "energy": 0.15, "danceability": 0.06, "brightness": 0.06, "noisiness": 0.10,
}
FEATURE_LABELS: dict[str, str] = {
    "energy": "エネルギー", "danceability": "ダンサビリティ",
    "brightness": "明るさ", "noisiness": "ノイズ感",
}
COMPONENT_LABELS: dict[str, str] = {
    "bpm": "テンポ", "key": "キー", "vector": "音色", "era": "年代",
    "continuity": "系統の連続性", "wordplay": "ワードプレイ", **FEATURE_LABELS,
}

THROWBACK_MIN_YEARS = 5
TEMPO_SIGMA_PERCENT = 3.0
# A half/double-time reading can be musically useful, but it is not as close as
# a direct BPM match. Keep it available outside transition mode while preventing
# it from tying with an ordinary near-tempo candidate.
HALF_DOUBLE_TEMPO_FACTOR = 0.72
# Transition mode: how closely a half/double partner or an edit's opening tempo
# must match the deck.
TRANSITION_TOLERANCE = math.log2(1.04)

TRANSITION_CAVEATS = [
    "トランジション曲はタイトルの「100-128」などの表記から入りと出口の BPM を読み取っています。"
    "表記の誤りや、表記のない曲は判定できません",
    "テンポが変わる曲の解析 BPM・キー・エネルギーは曲の一部分の値で、入りや出口と一致しない場合があります。"
    "rekordbox のビートグリッドも途中でずれることがあるため、試聴して確認してください",
    "倍テン・ハーフテンは BPM の比率だけで判定しています。実際に半分・倍のノリに聞こえるかは試聴で確認してください",
]

# An aspect counts as a strength or a weakness only past these values.
STRENGTH_THRESHOLD = 0.7
WEAKNESS_THRESHOLD = 0.4

# "Like X" (used by the chat agent's tools): what makes two tracks alike,
# independent of whether they mix.
LIKENESS_WEIGHTS: dict[str, float] = {"timbre": 0.50, "feel": 0.25, "genre": 0.15, "era": 0.10}
LIKENESS_LABELS: dict[str, str] = {"timbre": "音色", "feel": "ノリ", "genre": "ジャンル", "era": "年代"}
REFERENCE_BLEND = 0.55
LIKE_THRESHOLD = 0.7
ERA_SPAN_YEARS = 10.0
FEEL_FEATURES = ("energy", "danceability", "noisiness")


def resolve_intent(intent: str) -> str:
    intent = LEGACY_INTENTS.get(intent, intent)
    if intent not in PRESETS:
        raise ValueError(f"unknown intent: {intent}")
    return intent


@dataclass
class Reason:
    kind: str
    tone: str  # "good" | "neutral" | "caution"
    text: str

    def to_dict(self) -> dict[str, str]:
        return {"kind": self.kind, "tone": self.tone, "text": self.text}


@dataclass
class Scored:
    score: float
    components: dict[str, float] = field(default_factory=dict)
    reasons: list[Reason] = field(default_factory=list)
    summary: str = ""
    strengths: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class TransitionFit:
    kind: str  # "edit" | "half" | "double"
    score: float
    start_bpm: Optional[float]
    end_bpm: Optional[float]
    explicit: bool  # the title literally says "Transition"


def _number(value: Any) -> Optional[float]:
    if isinstance(value, bool) or value is None:
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def _positive(value: Any) -> Optional[float]:
    number = _number(value)
    return number if number is not None and number > 0 else None


# ------------------------------------------------------------------ tempo

def matched_tempo(source_bpm: Any, candidate_bpm: Any) -> Optional[tuple[float, float, int]]:
    """(effective BPM, signed percent, octave shift) under the nearest
    half/double-time reading, or None when either tempo is unknown."""
    source, candidate = _positive(source_bpm), _positive(candidate_bpm)
    if source is None or candidate is None:
        return None
    ratio = math.log2(candidate) - math.log2(source)
    shift = min((0, -1, 1), key=lambda value: abs(ratio + value))
    effective = candidate * (2 ** shift)
    return effective, (effective / source - 1) * 100, shift


def tempo_score(source_bpm: Any, candidate_bpm: Any, center: float = 0.0) -> Optional[float]:
    """Gaussian around the preset's preferred tempo change."""
    matched = matched_tempo(source_bpm, candidate_bpm)
    if matched is None:
        return None
    score = math.exp(-0.5 * ((matched[1] - center) / TEMPO_SIGMA_PERCENT) ** 2)
    return score * HALF_DOUBLE_TEMPO_FACTOR if matched[2] else score


_TRANSITION_PATTERN = re.compile(
    r"(?<![\d.])(\d{2,3}(?:\.\d+)?)\s*(?:-|–|—|~|〜|→|->|>|to)\s*(\d{2,3}(?:\.\d+)?)(?![\d.])",
    re.IGNORECASE,
)


def parse_transition(title: Any) -> Optional[tuple[float, float, bool]]:
    """Opening and closing BPM declared in a title such as
    "Tall Boys 100-128 Transition (Dirty)" or "Lose It All 135 - 150".

    Both numbers must look like tempos and differ, so "Vol. 1-2" or a year span
    is never read as a tempo change.
    """
    if not isinstance(title, str):
        return None
    explicit = "transition" in title.casefold()
    for match in _TRANSITION_PATTERN.finditer(title):
        start, end = float(match.group(1)), float(match.group(2))
        if 55 <= start <= 200 and 55 <= end <= 200 and abs(start - end) >= 5:
            return start, end, explicit
    return None


def transition_fit(source_bpm: Any, candidate: dict) -> Optional[TransitionFit]:
    """How well a candidate takes the floor to a new tempo from the deck.

    A declared tempo-changing edit fits when its opening tempo matches the deck;
    a regular track fits when it sits at half or double the deck's tempo.
    """
    source = _positive(source_bpm)
    if source is None:
        return None
    declared = parse_transition(candidate.get("title"))
    if declared is not None:
        start, end, explicit = declared
        distance = bpm_distance(source, start)
        if distance <= TRANSITION_TOLERANCE:
            score = math.exp(-0.5 * (distance / math.log2(1.02)) ** 2)
            return TransitionFit("edit", score, start, end, explicit)
        return None
    candidate_bpm = _positive(candidate.get("bpm"))
    if candidate_bpm is None:
        return None
    ratio = math.log2(candidate_bpm) - math.log2(source)
    for octave, kind in ((1, "double"), (-1, "half")):
        distance = abs(ratio - octave)
        if distance <= TRANSITION_TOLERANCE:
            score = math.exp(-0.5 * (distance / math.log2(1.02)) ** 2)
            return TransitionFit(kind, score, source, candidate_bpm, False)
    return None


# -------------------------------------------------------------------- key

def _camelot(key: Any) -> Optional[tuple[int, str]]:
    camelot = normalize_key(key if isinstance(key, str) else None)
    if not camelot:
        return None
    return int(camelot[:-1]), camelot[-1]


def key_move(source_key: Any, candidate_key: Any) -> Optional[str]:
    """Named Camelot movement: same, +1, -1, +2, +7 (semitone up), -7,
    to_major, to_minor, diagonal or other."""
    source, candidate = _camelot(source_key), _camelot(candidate_key)
    if source is None or candidate is None:
        return None
    step = (candidate[0] - source[0]) % 12
    step = step - 12 if step > 6 else step
    if source[1] == candidate[1]:
        return {0: "same", 1: "+1", -1: "-1", 2: "+2", -5: "+7", 5: "-7"}.get(step, "other")
    if step == 0:
        return "to_major" if candidate[1] == "B" else "to_minor"
    return "diagonal" if abs(step) == 1 else "other"


_COMPATIBLE = {"same": 1.0, "+1": 0.9, "-1": 0.9, "to_major": 0.85, "to_minor": 0.85,
               "diagonal": 0.55, "+7": 0.5, "-7": 0.4, "+2": 0.4, "other": 0.15}
_KEY_TABLES: dict[str, dict[str, float]] = {
    "keep": _COMPATIBLE,
    "dance": _COMPATIBLE,
    "throwback": _COMPATIBLE,
    "wordplay": _COMPATIBLE,
    "hype": {"+1": 1.0, "+7": 0.95, "same": 0.8, "+2": 0.75, "to_major": 0.7, "-1": 0.55,
             "to_minor": 0.55, "diagonal": 0.5, "-7": 0.2, "other": 0.15},
    "calm": {"-1": 1.0, "to_minor": 0.9, "same": 0.8, "-7": 0.6, "+1": 0.55, "to_major": 0.5,
             "diagonal": 0.45, "+7": 0.2, "+2": 0.2, "other": 0.15},
}
# Which Camelot letter each mood preset is steering toward.
_MODE_TARGET = {"emotional": ("A", "to_minor"), "bright": ("B", "to_major")}


def key_score(source_key: Any, candidate_key: Any, preset: str = "keep") -> Optional[float]:
    """Camelot movement scored in the preset's direction. Unknown is unknown."""
    move = key_move(source_key, candidate_key)
    if move is None:
        return None
    if preset in _MODE_TARGET:
        letter, arrival = _MODE_TARGET[preset]
        candidate = _camelot(candidate_key)
        if move == arrival:
            return 1.0
        compatible = _COMPATIBLE[move] >= 0.85
        if candidate and candidate[1] == letter:
            return 0.85 if compatible else 0.55 if move == "diagonal" else 0.15
        return 0.45 if compatible else 0.15
    return _KEY_TABLES[preset][move]


_KEY_MOVE_TEXT = {
    "same": "同じキー", "+1": "+1", "-1": "-1", "+2": "+2（全音上げ）", "+7": "+7（半音上げ）",
    "-7": "-7（半音下げ）", "to_major": "長調へ", "to_minor": "短調へ", "diagonal": "斜め移動",
    "other": "離れたキー",
}


# --------------------------------------------------------------- features

def direction_score(delta: float, direction: str, step: float) -> float:
    """How well a feature change follows the wanted direction.

    Going the right way scores up to a full step, then eases off slowly: a jump
    far beyond one step is still the right direction but no longer a smooth mix.
    """
    if direction == "hold":
        return max(0.0, 1.0 - min(1.0, abs(delta) / (step * 2)))
    signed = delta if direction in ("up", "not_down") else -delta
    if direction in ("not_down", "not_up"):
        if signed >= 0:
            return 1.0 if signed <= step * 2 else max(0.5, 1.0 - (signed - step * 2) / (step * 4))
        return max(0.0, 1.0 + signed / step)
    if signed <= step:
        return min(1.0, max(0.0, 0.5 + signed / (step * 2)))
    return max(0.6, 1.0 - (signed - step) / (step * 4))


def feature_score(source: dict, candidate: dict, feature: str, direction: str) -> Optional[float]:
    left, right = _number(source.get(feature)), _number(candidate.get(feature))
    if left is None or right is None:
        return None
    return direction_score(right - left, direction, FEATURE_STEPS[feature])


def era_score(source: dict, candidate: dict) -> Optional[float]:
    left, right = _positive(source.get("year")), _positive(candidate.get("year"))
    if left is None or right is None:
        return None
    older = left - right
    if older < THROWBACK_MIN_YEARS:
        return 0.0
    return min(1.0, 0.6 + 0.4 * (older - THROWBACK_MIN_YEARS) / 5)


def wordplay_score(pair: Optional[dict]) -> Optional[float]:
    """Approved wordplay edges only; a tested one outranks an untested one."""
    if not pair:
        return None
    return 1.0 if pair.get("verification_status") == "tested" else 0.8


def _feature_deltas(source: dict, candidate: dict) -> dict[str, float]:
    deltas = {}
    for feature in FEEL_FEATURES:
        left, right = _number(source.get(feature)), _number(candidate.get(feature))
        if left is not None and right is not None:
            deltas[feature] = right - left
    return deltas


def feature_closeness(source: dict, candidate: dict) -> Optional[float]:
    deltas = _feature_deltas(source, candidate)
    if not deltas:
        return None
    mean_delta = sum(abs(value) for value in deltas.values()) / len(deltas)
    return max(0.0, 1.0 - min(1.0, mean_delta / 0.35))


def genre_continuity_score(source: dict, candidate: dict) -> Optional[float]:
    """How safely the candidate preserves the source's broad musical lane.

    This is deliberately a soft score. A DJ may want to cross genres, but a
    cross-genre pick should not look as if it were a normal continuity match.
    Missing labels remain unknown rather than being treated as compatible.
    """
    return _genre_likeness(source, candidate)


# ---------------------------------------------------------------- filters

def passes_intent_filter(intent: str, source: dict, candidate: dict) -> bool:
    """Hard constraints that ranking must not be able to talk its way past.

    Transition mode applies its own tempo rule instead (see transition_fit).
    """
    intent = resolve_intent(intent)
    if intent == "wordplay":
        return True
    matched = matched_tempo(source.get("bpm"), candidate.get("bpm"))
    if matched is None:
        # Without a tempo on both sides there is no evidence the mix works.
        return False
    low, high = PRESETS[intent].tempo_window
    if not low <= matched[1] <= high:
        return False
    if intent == "throwback":
        score = era_score(source, candidate)
        return score is not None and score > 0
    return True


def passes_transition_filter(intent: str, source: dict, candidate: dict) -> bool:
    """Transition mode keeps the preset's non-tempo rules (throwback's age)."""
    intent = resolve_intent(intent)
    if intent == "throwback":
        score = era_score(source, candidate)
        return score is not None and score > 0
    return True


# ---------------------------------------------------------------- scoring

def evaluate(
    source: dict,
    candidate: dict,
    intent: str,
    vector_similarity: Optional[float] = None,
    pair: Optional[dict] = None,
    has_analysis: bool = True,
    transition: Optional[TransitionFit] = None,
) -> Scored:
    """Scores one candidate for a preset and explains the result."""
    intent = resolve_intent(intent)
    preset = PRESETS[intent]
    values: dict[str, Optional[float]] = {
        "bpm": transition.score if transition else
        tempo_score(source.get("bpm"), candidate.get("bpm"), preset.tempo_center),
        "key": key_score(source.get("key"), candidate.get("key"), intent),
        "vector": _number(vector_similarity),
        "continuity": genre_continuity_score(source, candidate),
        "era": era_score(source, candidate),
        "wordplay": wordplay_score(pair),
    }
    for feature, direction in preset.directions.items():
        values[feature] = feature_score(source, candidate, feature, direction)

    components = {
        name: max(0.0, min(1.0, value))
        for name, value in values.items()
        if name in preset.weights and value is not None
    }
    available = sum(preset.weights[name] for name in components)
    score = (
        sum(preset.weights[name] * value for name, value in components.items()) / available
        if available > 0
        else 0.0
    )
    reasons = build_reasons(source, candidate, intent, values, pair, has_analysis, transition)
    strengths, summary = summarize(intent, source, candidate, components, transition)
    return Scored(score=score, components=components, reasons=reasons,
                  summary=summary, strengths=strengths)


def summarize(
    intent: str,
    source: dict,
    candidate: dict,
    components: dict[str, float],
    transition: Optional[TransitionFit] = None,
) -> tuple[list[str], str]:
    """How well the candidate does what the preset asked, in one line.

    Names the measured moves ("エネルギー +0.12・キー 8A→9A（+1）・BPM +2.4%")
    so the DJ can check the claim, then what is weak and what is unknown.
    Strengths are ordered by what they actually contributed (weight x value).
    """
    preset = PRESETS[intent]
    weights = preset.weights
    contribution = sorted(components, key=lambda name: (-weights[name] * components[name], name))
    strengths = [name for name in contribution if components[name] >= STRENGTH_THRESHOLD]
    weaknesses = [name for name in contribution if components[name] < WEAKNESS_THRESHOLD]
    unknown = [name for name in weights if name not in components and name != "wordplay"]

    facts = []
    for feature in preset.directions:
        left, right = _number(source.get(feature)), _number(candidate.get(feature))
        if left is not None and right is not None and feature in weights:
            facts.append(f"{FEATURE_LABELS[feature]} {right - left:+.2f}")
    if intent == "throwback":
        left, right = _positive(source.get("year")), _positive(candidate.get("year"))
        if left is not None and right is not None:
            facts.append(f"{int(right)}年（{int(left - right)}年前）")
    move = key_move(source.get("key"), candidate.get("key"))
    if move is not None:
        facts.append(
            f"キー {normalize_key(source.get('key'))}→{normalize_key(candidate.get('key'))}"
            f"（{_KEY_MOVE_TEXT[move]}）"
        )
    if transition is not None:
        facts.append(_transition_text(transition))
    else:
        matched = matched_tempo(source.get("bpm"), candidate.get("bpm"))
        if matched is not None:
            tempo_fact = f"BPM {matched[1]:+.1f}%"
            if matched[2]:
                tempo_fact += "（倍テン／ハーフテン扱い）"
            facts.append(tempo_fact)

    total = sum(weights[name] * value for name, value in components.items())
    available = sum(weights[name] for name in components)
    fit = total / available if available else 0.0
    head = f"{preset.label}向き" if fit >= 0.65 else f"{preset.label}の効果は控えめ"
    parts = [f"{head}：{'・'.join(facts)}" if facts else head]
    label = lambda names: "・".join(COMPONENT_LABELS[name] for name in names)
    if weaknesses:
        parts.append(f"{label(weaknesses)}は弱め")
    if unknown:
        parts.append(f"{label(unknown)}は未判定")
    return strengths, "／".join(parts)


def _transition_text(fit: TransitionFit) -> str:
    if fit.kind == "edit":
        return f"トランジション {fit.start_bpm:g}→{fit.end_bpm:g} BPM"
    return f"{'倍テン' if fit.kind == 'double' else 'ハーフテン'} {fit.start_bpm:g}→{fit.end_bpm:g} BPM"


def describe_weights(intent: str) -> str:
    """What the preset's score is made of, e.g. エネルギー 36%・テンポ 22%…"""
    weights = PRESETS[resolve_intent(intent)].weights
    ordered = sorted(weights.items(), key=lambda item: -item[1])
    return "・".join(f"{COMPONENT_LABELS[name]} {round(weight * 100)}%" for name, weight in ordered)


# --------------------------------------------------------------- likeness

@dataclass
class Likeness:
    score: float
    aspects: list[str]  # likeness keys that are close, strongest first


def _genre_likeness(anchor: dict, candidate: dict) -> Optional[float]:
    left = (anchor.get("genre") or "").strip().casefold()
    right = (candidate.get("genre") or "").strip().casefold()
    if not left or not right:
        return None
    if left != right:
        return 0.0
    left_sub = (anchor.get("subgenre") or "").strip().casefold()
    right_sub = (candidate.get("subgenre") or "").strip().casefold()
    return 1.0 if left_sub and left_sub == right_sub else 0.8


def _era_likeness(anchor: dict, candidate: dict) -> Optional[float]:
    left, right = _positive(anchor.get("year")), _positive(candidate.get("year"))
    if left is None or right is None:
        return None
    return max(0.0, 1.0 - abs(left - right) / ERA_SPAN_YEARS)


def likeness(anchor: dict, candidate: dict, vector_similarity: Optional[float]) -> Optional[Likeness]:
    """How much `candidate` is "like" `anchor`, regardless of whether they mix.

    Genre and era alone never make two tracks alike — plenty of same-genre,
    same-year records sound nothing alike — so without timbre or feel there is
    no likeness to report.
    """
    values = {
        "timbre": _number(vector_similarity),
        "feel": feature_closeness(anchor, candidate),
        "genre": _genre_likeness(anchor, candidate),
        "era": _era_likeness(anchor, candidate),
    }
    if values["timbre"] is None and values["feel"] is None:
        return None
    measured = {name: max(0.0, min(1.0, value)) for name, value in values.items() if value is not None}
    total = sum(LIKENESS_WEIGHTS[name] for name in measured)
    score = sum(LIKENESS_WEIGHTS[name] * value for name, value in measured.items()) / total
    aspects = sorted(
        (name for name, value in measured.items() if value >= STRENGTH_THRESHOLD),
        key=lambda name: -LIKENESS_WEIGHTS[name] * measured[name],
    )
    return Likeness(score=score, aspects=aspects)


def blend(mix_score: float, like: Optional[Likeness]) -> float:
    """Order for "like X": likeness first, but a track must still mix.

    A candidate whose likeness cannot be measured keeps only its mix share, so
    it sinks below measured look-alikes instead of being assumed to match.
    """
    like_score = like.score if like is not None else 0.0
    return REFERENCE_BLEND * like_score + (1.0 - REFERENCE_BLEND) * mix_score


def describe_like(label: str, like: Likeness) -> str:
    aspects = "・".join(LIKENESS_LABELS[name] for name in like.aspects[:3]) or "雰囲気"
    return f"{label}のような{aspects}"


# ---------------------------------------------------------------- reasons

def build_reasons(
    source: dict,
    candidate: dict,
    intent: str,
    values: dict[str, Optional[float]],
    pair: Optional[dict],
    has_analysis: bool,
    transition: Optional[TransitionFit] = None,
) -> list[Reason]:
    reasons: list[Reason] = []

    # An approved edge is worth surfacing under any preset, not just wordplay.
    if pair:
        reasons.append(_wordplay_reason(pair))

    if transition is not None:
        reasons.append(_transition_reason(source, transition))
    else:
        reasons.append(_tempo_reason(source, candidate, intent))
        declared = parse_transition(candidate.get("title"))
        if declared is not None:
            reasons.append(Reason(
                "tempo_change", "caution",
                f"タイトル上テンポが変わる曲（{declared[0]:g}→{declared[1]:g}）：解析 BPM は参考値です",
            ))
    reasons.append(_key_reason(source, candidate, values.get("key"), intent))
    reasons.append(_continuity_reason(source, candidate, values.get("continuity")))
    for feature, direction in PRESETS[intent].directions.items():
        reasons.append(_feature_reason(source, candidate, feature, direction))
    if intent == "throwback":
        reasons.append(_era_reason(source, candidate))

    similarity = values.get("vector")
    if similarity is not None:
        reasons.append(Reason(
            "similarity", "neutral",
            "音色の傾向が近い" if similarity >= 0.7 else "音色の傾向に違いがある",
        ))
    elif has_analysis:
        reasons.append(Reason("similarity", "caution", "音色を比較できません"))
    else:
        reasons.append(Reason("similarity", "caution", "音響解析が未実行です"))
    return reasons


def _tempo_reason(source: dict, candidate: dict, intent: str) -> Reason:
    matched = matched_tempo(source.get("bpm"), candidate.get("bpm"))
    if matched is None:
        return Reason("tempo", "caution", "BPM が未取得のためテンポの相性は判定できません")
    effective, percent, shift = matched
    source_bpm, candidate_bpm = float(source["bpm"]), float(candidate["bpm"])
    conversion = "1/2" if shift == -1 else "2倍"
    note = f"、原曲{candidate_bpm:g} BPMの{conversion}換算" if shift else ""
    center = PRESETS[intent].tempo_center
    off = abs(percent - center)
    tone = "good" if off <= 3 else "neutral" if off <= 6 else "caution"
    return Reason("tempo", tone, f"BPM {source_bpm:g} → {effective:g}（{percent:+.1f}%{note}）")


def _transition_reason(source: dict, fit: TransitionFit) -> Reason:
    if fit.kind == "edit":
        label = "トランジション曲" if fit.explicit else "タイトルからテンポ変化曲と判断"
        return Reason(
            "transition", "good" if fit.explicit else "neutral",
            f"{label}：入り {fit.start_bpm:g} → 出口 {fit.end_bpm:g} BPM"
            f"（基準 {float(source['bpm']):g} BPM から入れる）",
        )
    kind = "倍テン" if fit.kind == "double" else "ハーフテン"
    return Reason("transition", "neutral", f"{kind}：{fit.start_bpm:g} → {fit.end_bpm:g} BPM")


def _key_reason(source: dict, candidate: dict, score: Optional[float], intent: str) -> Reason:
    left = normalize_key(source.get("key") if isinstance(source.get("key"), str) else None)
    right = normalize_key(candidate.get("key") if isinstance(candidate.get("key"), str) else None)
    if score is None:
        missing = "元曲" if not left else "候補"
        return Reason("key", "caution", f"{missing}のキー情報がないため判定できません")
    move = key_move(left, right)
    tone = "good" if score >= 0.85 else "neutral" if score >= 0.5 else "caution"
    return Reason("key", tone, f"キー {left} → {right}：{_KEY_MOVE_TEXT[move]}")


def _continuity_reason(source: dict, candidate: dict, score: Optional[float]) -> Reason:
    if score is None:
        return Reason("continuity", "caution", "ジャンル情報がないため系統の連続性は判定できません")
    if score >= 0.95:
        return Reason("continuity", "good", "同じサブジャンルで流れを保ちやすい")
    if score >= 0.75:
        return Reason("continuity", "neutral", "同じジャンルだがサブジャンルは異なります")
    return Reason("continuity", "caution", "ジャンルが変わるため流れの連続性は弱めです")


def _feature_reason(source: dict, candidate: dict, feature: str, direction: str) -> Reason:
    label = FEATURE_LABELS[feature]
    left, right = _number(source.get(feature)), _number(candidate.get(feature))
    if left is None or right is None:
        return Reason(feature, "caution", f"{label}は未解析です")
    delta = right - left
    score = direction_score(delta, direction, FEATURE_STEPS[feature])
    tone = "good" if score >= 0.65 else "neutral" if score >= 0.4 else "caution"
    movement = (
        "を維持" if abs(delta) < FEATURE_STEPS[feature] / 3
        else "を上げる" if delta > 0 else "を下げる"
    )
    return Reason(feature, tone, f"{label}{movement}")


def _era_reason(source: dict, candidate: dict) -> Reason:
    left, right = _positive(source.get("year")), _positive(candidate.get("year"))
    if left is None or right is None:
        return Reason("era", "caution", "リリース年が未登録のため判定できません")
    return Reason("era", "good", f"{int(right)}年リリース（基準の{int(left - right)}年前）")


def _wordplay_reason(pair: dict) -> Reason:
    keyword = pair.get("keyword") or ""
    source_phrase = (pair.get("source_phrase") or "").strip()
    target_phrase = (pair.get("target_phrase") or "").strip()
    tested = pair.get("verification_status") == "tested"
    caveat = (
        "音出し検証済み"
        if tested
        else "音出しは未検証です（承認は音を確認したという意味ではありません）"
    )
    return Reason(
        "wordplay",
        "good" if tested else "neutral",
        f"承認済みワードプレイ「{keyword}」：「{source_phrase}」→「{target_phrase}」／{caveat}",
    )


def rank(scored: Iterable[tuple[Any, Scored]], limit: int) -> list[tuple[Any, Scored]]:
    """Deterministic ordering: score first, then track id, never insertion order."""
    ordered = sorted(scored, key=lambda item: (-item[1].score, _sort_id(item[0])))
    return ordered[: max(0, limit)]


def _sort_id(track: Any) -> int:
    identifier = track.get("id") if isinstance(track, dict) else getattr(track, "id", None)
    return int(identifier) if isinstance(identifier, int) else 0
