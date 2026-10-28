"""Export deterministic S1 array-storage vectors from the real NumPy codec.

    backend/.venv/bin/python tools/parity/export_array_vectors.py

Only imports backend/utils/array_codec.py: no backend startup or user data.
IEEE-754 bit strings preserve signed zero, infinities and NaN in the inputs.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import platform
import random
import struct
import warnings
from pathlib import Path

import numpy as np


def bits(value: float) -> str:
    return struct.pack(">d", value).hex()


def from_bits(value: int) -> float:
    return struct.unpack(">d", value.to_bytes(8, "big"))[0]


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=root / "contracts/compat-vectors")
    args = parser.parse_args()
    source = root / "backend/utils/array_codec.py"
    spec = importlib.util.spec_from_file_location("plumdeck_array_codec", source)
    codec = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(codec)
    rng = random.Random(20261028)
    specials = [0.0, -0.0, 0.1, 0.5, -0.5, 1.0, -1.0, 127.9, 1.0e40,
                float("nan"), float("inf"), -float("inf")]
    inputs = [None, [], specials]
    inputs += [[from_bits(rng.getrandbits(64)) for _ in range(rng.randrange(1, 25))]
               for _ in range(100)]
    inputs += [[rng.uniform(-2, 2) for _ in range(length)]
               for length in [499, 500, 501, 999, 1000, 1001]]
    rows = []
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)
        for values in inputs:
            encoded = None if values is None else [bits(v) for v in values]
            blob = codec.pack_f32(values)
            rows.append({"op": "pack_f32", "values": encoded,
                         "out": None if blob is None else blob.hex()})
            for points in [0, 1, 2, 500]:
                row = {"op": "pack_u8", "values": encoded, "points": points}
                try:
                    blob = codec.pack_u8_waveform(values, points)
                    row["out"] = None if blob is None else blob.hex()
                except ZeroDivisionError:
                    row["error"] = "ZeroDivisionError"
                rows.append(row)
        for blob in [None, b"", bytes(range(256))]:
            rows.append({"op": "unpack_u8", "blob": None if blob is None else blob.hex(),
                         "out": [bits(v) for v in codec.unpack_u8_waveform(blob)]})
        blobs = [None, b"", b"\x00", b"\x00\x01\x02", b"\x00" * 5]
        blobs += [rng.randbytes(length * 4) for length in range(1, 50)]
        for blob in blobs:
            row = {"op": "unpack_f32", "blob": None if blob is None else blob.hex()}
            try:
                row["out"] = [bits(v) for v in codec.unpack_f32(blob)]
            except ValueError:
                row["error"] = "ValueError"
            rows.append(row)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "array_codec.jsonl").write_text(
        "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows), encoding="ascii")
    meta = {"python": platform.python_version(), "numpy": np.__version__, "cases": len(rows),
            "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest()}
    (args.out / "array_codec.meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="ascii")
    print(f"array_codec: {len(rows)} Python-generated cases")


if __name__ == "__main__":
    main()
