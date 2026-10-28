"""Write normalize_key vectors for plumdeck-key.

    python tools/parity/export_key_vectors.py [--out contracts/key]

Each line of normalize_key.jsonl is {"input": str, "camelot": str | null}:
backend/utils/audio_math.normalize_key on a fixed list of spellings (every
note, accidental and scale word in several cases and spacings, Camelot and
Open Key codes, Unicode accidentals and whitespace, the IGNORECASE dotless
i) plus seeded random strings. plumdeck-key's Python-compatible parser must
agree on every line. The run is deterministic.
"""
from __future__ import annotations

import argparse
import itertools
import json
import os
import random
import sys
import tempfile
from pathlib import Path

import parity_env

SEED = 20261029


def inputs(rng: random.Random) -> list[str]:
    notes = list("ABCDEFGabcdefg") + ["H", "h", "X"]
    accidentals = ["", "#", "b", "B", "♯", "♭", "##", "x"]
    scales = ["", "m", "M", "min", "Min", "MIN", "minor", "Minor", "MINOR", "maj", "Maj", "major", "Major",
              "mınor", "MİNOR", "moll", "dur", "-", "mi", "minors", "ma"]
    spaces = ["", " ", "  ", "\t", "　", "\x1c"]
    out = []
    for note, accidental, scale in itertools.product(notes, accidentals, scales):
        for space in spaces[:2] if scale else [""]:
            out.append(f"{note}{accidental}{space}{scale}")
    for number in range(0, 14):
        for letter in ["A", "B", "a", "b", "d", "m", "D", "M", "C"]:
            out += [f"{number}{letter}", f" {number}{letter} ", f"{number} {letter}", f"0{number}{letter}"]
    out += ["", " ", "　", "None", "C major ", " c minor", "Db Minor", "C#  minor", "C# ", "♯C",
            "Am7", "A minor\n", "8A\n", "C　major", "C\x1cminor", "ｃ", "Ｃ minor", "Ｃ", "１A", "8Ａ"]
    alphabet = "ABCDEFGabcdefgm#♯♭ 0123456789ijnoMr"
    for _ in range(4000):
        out.append("".join(rng.choice(alphabet) for _ in range(rng.randint(1, 8))))
    seen, unique = set(), []
    for text in out:
        if text not in seen:
            seen.add(text)
            unique.append(text)
    return unique


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=parity_env.CONTRACTS_DIR / "key")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    sandbox = Path(tempfile.mkdtemp(prefix="plumdeck-key-vectors-"))
    parity_env.sandbox_env(sandbox)
    parity_env.require_sandbox_env()
    parity_env.use_backend()
    os.chdir(sandbox)
    from utils.audio_math import normalize_key

    rows = [{"input": text, "camelot": normalize_key(text)} for text in inputs(random.Random(SEED))]
    path = args.out / "normalize_key.jsonl"
    path.write_text("".join(json.dumps(row, ensure_ascii=True, separators=(",", ":")) + "\n" for row in rows),
                    encoding="ascii", newline="\n")
    accepted = sum(1 for row in rows if row["camelot"])
    print(f"normalize_key: {len(rows)} inputs, {accepted} accepted (python {sys.version.split()[0]})")


if __name__ == "__main__":
    main()
