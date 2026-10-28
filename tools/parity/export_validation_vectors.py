"""Write pydantic's lax-mode scalar coercions for plumdeck-backend's validators.

    python tools/parity/export_validation_vectors.py [--out contracts/compat-vectors]

FastAPI validates query parameters (strings) and JSON bodies (json.loads
values) with pydantic in lax Python mode. Each line of
pydantic_lax.jsonl is one input for one target type:
{"type": "int" | "float" | "bool" | "str", "input": <typed>, "ok": bool,
"value": <typed> | null}, with values encoded as in export_vectors.py
(ints as decimal strings, floats as IEEE-754 bits). The Rust unit tests in
plumdeck-backend replay every case. The inputs are a fixed list plus seeded
random spellings, so reruns produce the same file.
"""
from __future__ import annotations

import argparse
import itertools
import json
import math
import random
from pathlib import Path

import pydantic
import pydantic_core
from pydantic import TypeAdapter

import parity_env
from export_vectors import typed

SEED = 20261028

STRINGS = [
    "", " ", "0", "1", "-1", "+1", "-0", "00012", " 1 ", "\t1\n", "　1　", "\x0b1", "\x1c1",
    "\xa01", " 1", "\u00851", "1_000", "1__0", "_1", "1_", "0_0", "1_0.0", "1.0", "1.00",
    "1.0_0", "1.5", "-1.0", "-0.0", "0.", ".5", "5.", "1.", "1e3", "1.0e0", "1e", "1E-2", "+.5e-2",
    "0x10", "0b1", "0o7", "--1", "+-1", "-", "+", "1 .0", "1.0 ", "1.0000000001", "1e400", "-1e400",
    "9223372036854775807", "9223372036854775808", "-9223372036854775808", "-9223372036854775809",
    "999999999999999999999999999999", "inf", "-inf", "+inf", "Inf", "iNf", "infinity", "-Infinity",
    "infinit", "nan", "NaN", "-nan", "+NAN", "nan(1)", "１２", "١", "٣", "²", "true", "false", "True",
    "FALSE", "yes", "YES", "no", "on", "On", "off", "OFF", "t", "T", "f", "F", "y", "n", "Y", "N",
    " true ", "true ", "tru", "2", "1.0", "0.0", "None", "null",
]

JSON_VALUES = [
    True, False, None, 0, 1, 2, -1, 2 ** 63 - 1, 2 ** 63, 2 ** 70, -(2 ** 63), -(2 ** 63) - 1,
    0.0, -0.0, 1.0, 1.5, -1.0, 2.0, 0.5, 1e20, -1e20, 9.2e18, 9.3e18, -9.2e18, math.nan, math.inf,
    -math.inf, 1e-300, [1], [], {"a": 1}, {}, *STRINGS,
]


def outcome(adapter: TypeAdapter, value):
    try:
        return True, adapter.validate_python(value)
    except pydantic.ValidationError:
        return False, None


def random_strings(rng: random.Random, count: int) -> list[str]:
    alphabet = "0123456789_.+-eE "
    out = []
    for _ in range(count):
        length = rng.randint(1, 7)
        out.append("".join(rng.choice(alphabet) for _ in range(length)))
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=parity_env.CONTRACTS_DIR / "compat-vectors")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    adapters = {"int": TypeAdapter(int), "float": TypeAdapter(float), "bool": TypeAdapter(bool),
                "str": TypeAdapter(str)}
    inputs = JSON_VALUES + random_strings(random.Random(SEED), 3000)
    rows, seen = [], set()
    for (name, adapter), value in itertools.product(adapters.items(), inputs):
        ok, result = outcome(adapter, value)
        row = {"type": name, "input": typed(value), "ok": ok, "value": typed(result) if ok else None}
        line = json.dumps(row, ensure_ascii=True, separators=(",", ":"))
        if line not in seen:
            seen.add(line)
            rows.append(line)
    path = args.out / "pydantic_lax.jsonl"
    path.write_text("".join(line + "\n" for line in rows), encoding="ascii", newline="\n")
    meta = {"pydantic": pydantic.VERSION, "pydantic-core": pydantic_core.__version__, "cases": len(rows)}
    (args.out / "pydantic_lax.meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="ascii")
    print(f"pydantic_lax: {len(rows)} cases (pydantic {pydantic.VERSION})")


if __name__ == "__main__":
    main()
