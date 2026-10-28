"""Write Python-generated input/output vectors for plumdeck-core::compat.

    python tools/parity/export_vectors.py [--out contracts/compat-vectors]

Each *.jsonl file holds one case per line; the Rust unit tests replay every
case and require 100% agreement. Floats travel as their IEEE-754 bit pattern
("0x..."), Python ints as decimal strings, so nothing is lost in JSON;
float_parse.jsonl is the exception and carries float text on purpose. The run
is seeded and deterministic: rerunning it must not change the files.
meta.json records the interpreter: the vectors are CPython 3.12 behaviour
(Unicode 15.0.0), which 3.11 builds do not reproduce.

Where the backend has the function, the backend's own implementation is called
(normalize_text, normalize_path, path_variants). Expressions that live inline in
backend code are copied verbatim and checked against the source text.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import inspect
import json
import math
import ntpath
import os
import platform
import random
import re
import statistics
import struct
import sys
import tempfile
import unicodedata
from pathlib import Path, PurePosixPath, PureWindowsPath
from types import SimpleNamespace
from unittest import mock

import parity_env

SEED = 20251027
TOKEN_PATTERN = r"[^\W_]+(?:'[^\W_]+)?"
TOKEN_SKIP = {"mp3", "wav", "aiff", "aif", "flac", "m4a", "aac", "ogg"}
FLOAT_SPECS = ["g", "+g", "+.1f", "+.2f", ".2f", ".6f", ".0f", ".3f", ".1f", ".4f",
               ".10g", ".3g", "e", ".2e", "+.3e"]
PERCENT_SPECS = ["%.2f", "%.6f", "%.1f", "%.0f"]


# ---------------------------------------------------------------- encoding

def fbits(x: float) -> str:
    return "0x%016x" % struct.unpack("<Q", struct.pack("<d", x))[0]


def from_bits(text: str) -> float:
    return struct.unpack("<d", struct.pack("<Q", int(text, 16)))[0]


def typed(value):
    """Encode a Python value so its exact type survives JSON."""
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, int):
        return {"int": str(value)}
    if isinstance(value, float):
        return {"float": fbits(value)}
    if isinstance(value, str):
        return {"str": value}
    if isinstance(value, (list, tuple)):
        return {"list": [typed(v) for v in value]}
    if isinstance(value, dict):
        return {"dict": [[typed(k), typed(v)] for k, v in value.items()]}
    if isinstance(value, dt.datetime):
        return {"datetime": value.isoformat()}
    if isinstance(value, dt.date):
        return {"date": value.isoformat()}
    raise TypeError(type(value))


class Writer:
    def __init__(self, out: Path):
        self.out = out
        self.counts: dict[str, int] = {}

    def write(self, name: str, rows) -> None:
        path = self.out / f"{name}.jsonl"
        seen = set()
        count = 0
        with path.open("w", encoding="ascii", newline="\n") as handle:
            for row in rows:
                line = json.dumps(row, ensure_ascii=True, separators=(",", ":"))
                if line in seen:
                    continue
                seen.add(line)
                handle.write(line + "\n")
                count += 1
        self.counts[name] = count


# ---------------------------------------------------------------- inputs

def float_pool(rng: random.Random, size: int) -> list[float]:
    specials = [0.0, -0.0, math.nan, -math.nan, math.inf, -math.inf, 1.0, -1.0, 0.5, -0.5,
                1.5, 2.5, -2.5, 0.125, 0.375, 2.675, 1.005, 0.045, 1e16, 1e15, 9999999999999998.0,
                1e17, 1e22, 1e23, 1e-4, 1e-5, 0.0001, 0.00001, 1.5e-5, 0.1 + 0.2, 0.1, 0.2, 0.3,
                1 / 3, 2 / 3, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308,
                float(2 ** 53), float(2 ** 53 + 2), float(2 ** 63), -float(2 ** 63),
                123456789012345678.0, 128.0, 127.9, 126.25, 174.0, 99.99, 0.95, 9.5, 99.5,
                999999.5, 1234567.0, 12345678.9, 0.000123456, 9.9999995, 0.99999995,
                99999.95, 999999.95, 1e6, 1e-6, 1e100, 1e-100, 1e300, -1e-7, 4.35, 2.345,
                0.0005, 0.00049999999999999999, 299792458.0, 6.02214076e23]
    pool = list(specials)
    for _ in range(size // 6):
        # Raw bit patterns cover subnormals and every exponent.
        pool.append(from_bits("0x%016x" % rng.getrandbits(64)))
    for _ in range(size // 6):
        # DuckDB FLOAT columns widen f32 to f64 (127.9 -> 127.9000015258789).
        bits = rng.getrandbits(32)
        value = struct.unpack("<f", struct.pack("<I", bits))[0]
        pool.append(value)
    for _ in range(size // 6):
        pool.append(struct.unpack("<f", struct.pack("<f", rng.uniform(60, 200)))[0])
    for _ in range(size // 6):
        pool.append(rng.randint(-10 ** 7, 10 ** 7) / 10 ** rng.randint(0, 8))
    for _ in range(size // 6):
        # Exact ties at a decimal digit: half-even rounding must match.
        k = rng.randint(0, 6)
        pool.append((rng.randint(-10 ** 6, 10 ** 6) * 2 + 1) / 2 ** rng.randint(1, 4) / 10 ** k)
    while len(pool) < size:
        pool.append(rng.uniform(-1e3, 1e3) * 10 ** rng.randint(-12, 20))
    return pool


STRING_SAMPLES = [
    "", " ", "  a  b  ", "\t\nabc\r\n", "\x1c\x1d\x1e\x1fx\x85\xa0", "\u3000全角\u3000スペース\u3000",
    "\u2028line\u2029", "\u180e\u200b\ufeffzero", "Straße", "STRASSE", "ß", "ẞ", "ﬁ ﬂ ﬀ", "İstanbul",
    "ǅ ǈ ǋ", "ΣΑΣ ΟΔΟΣ", "ὈΔΥΣΣΕΎΣ", "Σ", "aΣ", "AΣb", "A\u0345Σ", "Ａｂｃ　１２３", "０１２３４５６７８９",
    "①②③", "㍻", "ｶﾀｶﾅ ﾊﾟﾋﾟ", "カタカナ ひらがな 漢字", "日本語 #%&.wav", "café", "cafe\u0301",
    "Ä\u0308", "가각", "\u1100\u1161\u11a8", "Ω Å K", "𝔘𝔫𝔦𝔠𝔬𝔡𝔢", "😀🎧🔥", "x\U0010ffffy", "\U0001d7ce\U0001d7cf",
    "rock'n'roll", "don't stop", "'quoted'", "a_b_c", "under_score test", "MP3 wav Flac m4a",
    "track01.mp3", "DJ Snake & Lil Jon - Turn Down for What (Dirty)", "Beyoncé — Halo",
    "  Mixed\u00a0NBSP\u2007figure\u202fnarrow  ", "tab\tsep\vvert\fform", "Ⅻ ⅻ ⅷ", "²³¹ ½ ¾",
    "٣٤٥ ۴۵۶ ४५६", "ǰ ΐ ΰ և ẖ", "ᾈ ᾳ ῼ", "\u0130\u0307", "Ⓐⓑ", "Ꭰꭰ", "ﬃ", "ŉ", "Ǆ",
    "\x00\x01\x7f", "\"quote\" \\back\\", "line\nbreak", "/Users/x/Music/日本語/曲.mp3",
]


def random_string(rng: random.Random) -> str:
    kind = rng.random()
    if kind < 0.3:
        return rng.choice(STRING_SAMPLES)
    if kind < 0.55:
        parts = rng.sample(STRING_SAMPLES, rng.randint(1, 3))
        return rng.choice([" ", "", "\u3000", "-", "'", "\t"]).join(parts)
    blocks = [(0x20, 0x7e), (0xa0, 0x24f), (0x300, 0x36f), (0x370, 0x3ff), (0x400, 0x4ff),
              (0x1e00, 0x1fff), (0x2000, 0x206f), (0x2150, 0x218f), (0x2460, 0x24ff),
              (0x3000, 0x30ff), (0x3300, 0x33ff), (0x4e00, 0x4e80), (0xac00, 0xac40),
              (0xfb00, 0xfb4f), (0xff00, 0xffef), (0x1d400, 0x1d7ff), (0x1f300, 0x1f64f),
              (0x10400, 0x1044f), (0x0, 0x1f), (0x80, 0x9f)]
    out = []
    for _ in range(rng.randint(0, 12)):
        lo, hi = rng.choice(blocks)
        out.append(chr(rng.randint(lo, hi)))
    return "".join(out)


# ---------------------------------------------------------------- numbers

def numeric_vectors(writer: Writer, rng: random.Random) -> None:
    pool = float_pool(rng, 6000)
    writer.write("float_repr", ({"x": fbits(x), "repr": repr(x)} for x in pool))

    # Specials plus an even sample of every category (BPM-like f32, decimals, ties).
    fmt_pool = pool[:70] + pool[70::4]

    def formats():
        for x in fmt_pool:
            for spec in FLOAT_SPECS:
                yield {"x": fbits(x), "spec": spec, "out": format(x, spec)}
            for spec in PERCENT_SPECS:
                yield {"x": fbits(x), "spec": spec, "out": spec % x}
        for value in [0, 1, -1, 5, 126, 10 ** 15, 10 ** 16, 10 ** 17, -(10 ** 16), 123456789]:
            # f"{int:g}" formats the int as a float.
            for spec in ("g", "+g"):
                yield {"x": fbits(float(value)), "spec": spec, "out": format(value, spec)}
    writer.write("float_format", formats())

    def rounds():
        for x in pool[:70] + pool[70::3]:
            for ndigits in (None, 0, 1, 2, 3, 4, 10):
                row = {"x": fbits(x), "ndigits": ndigits}
                try:
                    result = round(x, ndigits) if ndigits is not None else round(x)
                except (ValueError, OverflowError) as exc:
                    row.update(out=None, error=type(exc).__name__)
                else:
                    if ndigits is None:
                        if abs(result) >= 2 ** 63:
                            continue  # beyond i64; the Rust API reports out of range
                        row.update(out=str(result), error=None)
                    else:
                        row.update(out=fbits(result), error=None)
                yield row
    writer.write("round", rounds())

    def truncs():
        for x in pool:
            try:
                result = int(x)
            except (ValueError, OverflowError) as exc:
                yield {"x": fbits(x), "out": None, "error": type(exc).__name__}
                continue
            if abs(result) < 2 ** 63:
                yield {"x": fbits(x), "out": str(result), "error": None}
    writer.write("int_trunc", truncs())

    def float_divmods():
        operands = pool[:70] + pool[70::25] + [0.0, -0.0, 3.0, -3.0, 0.1, 1e-300, math.inf, -math.inf, math.nan]
        for _ in range(6000):
            a = rng.choice(operands)
            b = rng.choice(operands)
            row = {"a": fbits(a), "b": fbits(b)}
            try:
                row.update(floordiv=fbits(a // b), mod=fbits(a % b), error=None)
            except ZeroDivisionError:
                row.update(floordiv=None, mod=None, error="ZeroDivisionError")
            yield row
        for a in (7.0, -7.0, 7.5, -7.5, 0.0, -0.0):
            for b in (2.0, -2.0, 0.5, -0.5, 1e300, -1e300):
                yield {"a": fbits(a), "b": fbits(b), "floordiv": fbits(a // b),
                       "mod": fbits(a % b), "error": None}
    writer.write("divmod_float", float_divmods())

    def int_divmods():
        edges = [0, 1, -1, 2, -2, 3, -3, 7, -7, 10, -10, 1000, -1000, 2 ** 62, -(2 ** 62),
                 2 ** 63 - 1, -(2 ** 63) + 1]
        for _ in range(6000):
            a = rng.choice(edges) if rng.random() < 0.3 else rng.randint(-10 ** 6, 10 ** 6)
            b = rng.choice(edges) if rng.random() < 0.3 else rng.randint(-1000, 1000)
            row = {"a": str(a), "b": str(b)}
            try:
                row.update(floordiv=str(a // b), mod=str(a % b), error=None)
            except ZeroDivisionError:
                row.update(floordiv=None, mod=None, error="ZeroDivisionError")
            yield row
    writer.write("divmod_int", int_divmods())

    def medians():
        finite = [x for x in pool if math.isfinite(x) and abs(x) < 1e300]
        yield {"values": [], "out": None, "error": "StatisticsError"}
        for _ in range(3000):
            n = rng.randint(1, 9)
            if rng.random() < 0.5:
                values = [float(rng.randint(5000, 20000)) for _ in range(n)]  # PQTZ tempos
            else:
                values = [rng.choice(finite) for _ in range(n)]
            yield {"values": [fbits(v) for v in values],
                   "out": fbits(float(statistics.median(values))), "error": None}
    writer.write("median", medians())


def float_parse_vectors(writer: Writer, rng: random.Random) -> None:
    """Float text as Python writes it, with the bits Python reads back.

    Request bodies and stored JSON reach Rust as text, so serde_json must parse
    it to the same double as json.loads and pydantic (correct rounding) and
    the encoder must write the same text again.
    """
    from pydantic import TypeAdapter

    pydantic_float = TypeAdapter(float)

    def rows():
        for x in float_pool(rng, 24000):
            if not math.isfinite(x):
                continue
            text = json.dumps(x)
            assert fbits(json.loads(text)) == fbits(x), text
            assert fbits(pydantic_float.validate_json(text)) == fbits(x), text
            yield {"text": text, "x": fbits(x)}
    writer.write("float_parse", rows())


# ---------------------------------------------------------------- json

def json_vectors(writer: Writer, rng: random.Random) -> None:
    floats = float_pool(rng, 800)
    stamps = [dt.datetime(2024, 1, 2, 3, 4, 5), dt.datetime(2024, 1, 2, 3, 4, 5, 123000),
              dt.datetime(1, 1, 1), dt.datetime(9999, 12, 31, 23, 59, 59, 999999),
              dt.datetime(5, 6, 7, 8, 9, 10, 11), dt.date(2025, 10, 27), dt.date(999, 1, 9)]

    def scalar():
        r = rng.random()
        if r < 0.08:
            return None
        if r < 0.14:
            return rng.random() < 0.5
        if r < 0.3:
            return rng.choice([0, 1, -1, 2 ** 31, -(2 ** 63), 2 ** 64, 10 ** 30,
                               rng.randint(-10 ** 12, 10 ** 12)])
        if r < 0.55:
            return rng.choice(floats)
        if r < 0.62:
            return rng.choice(stamps)
        return random_string(rng)

    def key():
        r = rng.random()
        if r < 0.8:
            return random_string(rng)
        return rng.choice([0, -5, 10 ** 20, 1.5, -0.0, 1e16, 1e-05, math.inf, math.nan, True, False,
                           None])

    def value(depth=0):
        r = rng.random()
        if depth >= 3 or r < 0.5:
            return scalar()
        if r < 0.75:
            return [value(depth + 1) for _ in range(rng.randint(0, 4))]
        out = {}
        for _ in range(rng.randint(0, 4)):
            out[key()] = value(depth + 1)
        return out

    option_sets = []
    for ensure_ascii in (True, False):
        for separators in (None, (",", ":"), (", ", ": "), (",", ": ")):
            for indent in (None, 0, 2):
                for allow_nan in (True, False):
                    for default_str in (False, True):
                        option_sets.append({
                            "ensure_ascii": ensure_ascii,
                            "separators": list(separators) if separators else None,
                            "indent": indent, "allow_nan": allow_nan, "default_str": default_str,
                        })

    def dumps(obj, opts):
        kwargs = {"ensure_ascii": opts["ensure_ascii"], "indent": opts["indent"],
                  "allow_nan": opts["allow_nan"]}
        if opts["separators"]:
            kwargs["separators"] = tuple(opts["separators"])
        if opts["default_str"]:
            kwargs["default"] = str
        return json.dumps(obj, **kwargs)

    # The call shapes the backend actually uses, first.
    backend_shapes = [
        {"ensure_ascii": False, "separators": [",", ":"], "indent": None, "allow_nan": True,
         "default_str": True},   # workflow_service._json (USB snapshot hash)
        {"ensure_ascii": False, "separators": None, "indent": None, "allow_nan": True,
         "default_str": False},  # wordplay_json
        {"ensure_ascii": True, "separators": None, "indent": None, "allow_nan": True,
         "default_str": False},  # grid fingerprint, provenance
        {"ensure_ascii": False, "separators": None, "indent": 2, "allow_nan": True,
         "default_str": True},   # USB manifest.json
        {"ensure_ascii": False, "separators": None, "indent": None, "allow_nan": False,
         "default_str": False},  # analysis features/embeddings
    ]

    def cases():
        fixed = [
            ["/Users/x/a.wav", 123, 1700000000123456789],
            ["/Users/x/a.wav", "missing"],
            {"b": 1, "a": [1.0, 2.5, 1e16, 1e-05, 127.9000015258789]},
            {"text": "日本語 😀", "nested": {"k": None, "t": True}},
            [math.nan, math.inf, -math.inf], {}, [], "", 0, -0.0, 1e-7, {1: "int", 2.5: "float"},
            {"when": dt.datetime(2025, 10, 27, 12, 0)}, [dt.date(2025, 1, 1)],
        ]
        for obj in fixed:
            for opts in option_sets:
                yield obj, opts
        for _ in range(4000):
            yield value(), rng.choice(backend_shapes if rng.random() < 0.5 else option_sets)

    def rows():
        for obj, opts in cases():
            row = {"value": typed(obj), "opts": opts}
            try:
                row.update(out=dumps(obj, opts), error=None)
            except (ValueError, TypeError) as exc:
                row.update(out=None, error=type(exc).__name__)
            yield row
    writer.write("pyjson", rows())


# ---------------------------------------------------------------- text

def text_vectors(writer: Writer, rng: random.Random) -> None:
    from infra import rekordbox_library
    from domain.services import assist_filters, phased_setlist_builder
    from infra.repositories import track_repository

    source = inspect.getsource(track_repository)
    assert f'r"{TOKEN_PATTERN}"' in source, "track search token regex changed"
    for ext in TOKEN_SKIP:
        assert f'"{ext}"' in source, ext
    token_re = re.compile(TOKEN_PATTERN, flags=re.UNICODE)

    def search_tokens(q: str) -> list[str]:
        raw = q.strip()
        return [t for t in token_re.findall(raw) if t.lower() not in TOKEN_SKIP]

    def record(s: str) -> dict:
        assert rekordbox_library.normalize_text(s) == assist_filters.normalize_text(s) \
            == phased_setlist_builder._fold(s)
        return {
            "s": s,
            "nfc": unicodedata.normalize("NFC", s), "nfd": unicodedata.normalize("NFD", s),
            "nfkc": unicodedata.normalize("NFKC", s), "nfkd": unicodedata.normalize("NFKD", s),
            "casefold": s.casefold(), "lower": s.lower(), "split": s.split(), "strip": s.strip(),
            "normalize_text": rekordbox_library.normalize_text(s),
            "tokens": token_re.findall(s), "search_tokens": search_tokens(s),
        }

    strings = list(STRING_SAMPLES) + [random_string(rng) for _ in range(5000)]
    writer.write("text", (record(s) for s in strings))

    def codepoints():
        for cp in range(0x110000):
            if 0xD800 <= cp <= 0xDFFF:
                continue
            ch = chr(cp)
            changed = (ch.casefold() != ch or ch.lower() != ch
                       or unicodedata.normalize("NFKD", ch) != ch)
            hangul = 0xAC00 <= cp <= 0xD7A3
            if (changed and (not hangul or cp % 97 == 0)) or ch.isspace() or cp % 4099 == 0:
                yield {"cp": cp, "casefold": ch.casefold(), "lower": ch.lower(),
                       "nfc": unicodedata.normalize("NFC", ch),
                       "nfd": unicodedata.normalize("NFD", ch),
                       "nfkc": unicodedata.normalize("NFKC", ch),
                       "nfkd": unicodedata.normalize("NFKD", ch)}
    writer.write("text_codepoints", codepoints())

    word, space = [], []
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        ch = chr(cp)
        if token_re.fullmatch(ch):
            if word and word[-1][1] + 1 == cp:
                word[-1][1] = cp
            else:
                word.append([cp, cp])
        if ch.isspace():
            space.append(cp)
    (writer.out / "text_classes.json").write_text(
        json.dumps({"unicode": unicodedata.unidata_version, "word_ranges": word, "space": space},
                   separators=(",", ":")) + "\n", encoding="ascii")


# ---------------------------------------------------------------- paths

PATH_SAMPLES = [
    "/", "//", "///", "/a//b/./c/", "//server/share/x", "/Users/x/Music/日本語 #%&.wav",
    "/Users/x/Music/cafe\u0301.mp3", "/Users/x/Music/café.mp3", "/tmp/a b/c?d#e%f",
    "/tmp/~user/.hidden/-_.!*'();:@&=+$,", "/tmp/üml/ŝ/😀.flac", "/a/../b/./c", "relative/path",
    "", ".", "..", "C:\\Users\\x\\Music\\曲.mp3", "C:/Users/x/Music/a b.mp3", "c:\\", "C:",
    "C:relative\\x", "\\\\server\\share\\dir\\f.wav", "\\\\server\\share", "//server/share/",
    "\\\\?\\C:\\long\\path", "\\\\?\\UNC\\server\\share\\x", "\\\\.\\device\\x", "\\rooted\\x",
    "C:\\a\\..\\..\\b", "C:\\a\\.\\b\\\\c\\", "a\\..\\..\\b", "..\\x", "C:\\Ä\\ß\\İ\\Σ",
    "D:\\DJ\\ＡＢＣ\\ｶﾀｶﾅ.mp3", "/Volumes/USB/PIONEER/rekordbox/master.db", "C:\\x\\Straße.MP3",
]


def path_vectors(writer: Writer, rng: random.Random) -> None:
    from infra import rekordbox_library

    def on(platform):
        return mock.patch.object(rekordbox_library, "sys", SimpleNamespace(platform=platform))

    def uri(cls, p):
        try:
            return cls(p).as_uri()
        except ValueError:
            return None

    def segment():
        return rng.choice(["a", "b c", "日本", "ß", "..", ".", "", "x.mp3", "cafe\u0301", "%20",
                           "C:", "?", "#", "😀", "Ａ", "UNC", "server", "share", "İ"])

    samples = list(PATH_SAMPLES)
    for _ in range(3000):
        sep = rng.choice(["/", "\\"])
        prefix = rng.choice(["", "/", "//", "\\\\", "C:", "C:\\", "c:/", "\\\\?\\", "\\\\srv\\shr\\"])
        samples.append(prefix + sep.join(segment() for _ in range(rng.randint(0, 5))))

    def rows():
        for p in samples:
            with on("darwin"):
                posix_norm = rekordbox_library.normalize_path(p)
                posix_variants = rekordbox_library.path_variants(p)
            with on("win32"):
                win_norm = rekordbox_library.normalize_path(p)
                win_variants = rekordbox_library.path_variants(p)
            yield {
                "path": p,
                "posix_str": str(PurePosixPath(p)), "windows_str": str(PureWindowsPath(p)),
                "posix_uri": uri(PurePosixPath, p), "windows_uri": uri(PureWindowsPath, p),
                "ntpath_normpath": ntpath.normpath(p), "ntpath_normcase": ntpath.normcase(p),
                "normalize_path_posix": posix_norm, "normalize_path_windows": win_norm,
                "path_variants_posix": posix_variants, "path_variants_windows": win_variants,
            }
    writer.write("pathnorm", rows())


# ---------------------------------------------------------------- time, hash

def time_vectors(writer: Writer, rng: random.Random) -> None:
    def rows():
        fixed = [dt.datetime(2024, 1, 2, 3, 4, 5), dt.datetime(2024, 1, 2, 3, 4, 5, 1),
                 dt.datetime(1, 1, 1), dt.datetime(9999, 12, 31, 23, 59, 59, 999999),
                 dt.datetime(999, 9, 9, 9, 9, 9, 90000)]
        for _ in range(3000):
            fixed.append(dt.datetime(rng.randint(1, 9999), rng.randint(1, 12), rng.randint(1, 28),
                                     rng.randint(0, 23), rng.randint(0, 59), rng.randint(0, 59),
                                     rng.choice([0, 0, rng.randint(0, 999999)])))
        for value in fixed:
            yield {"dt": [value.year, value.month, value.day, value.hour, value.minute,
                          value.second, value.microsecond],
                   "isoformat": value.isoformat(), "str": str(value),
                   "date_isoformat": value.date().isoformat()}
    writer.write("time", rows())


def sha_content(length: int) -> bytes:
    return bytes((i * 31 + 7) % 256 for i in range(length))


def hash_vectors(writer: Writer) -> None:
    mib = 1024 * 1024
    from app.services import workflow_service

    def rows():
        with tempfile.TemporaryDirectory() as tmp:
            for length in (0, 1, 31, mib - 1, mib, mib + 1, 2 * mib, 3 * mib + 17):
                path = Path(tmp) / f"f{length}"
                path.write_bytes(sha_content(length))
                digest = workflow_service._sha256(path)
                assert digest == hashlib.sha256(sha_content(length)).hexdigest()
                for limit in (None, length, length - 1 if length else None, mib):
                    try:
                        out = workflow_service._sha256(path, limit)
                        yield {"len": length, "limit": limit, "sha256": out, "error": None}
                    except ValueError:
                        yield {"len": length, "limit": limit, "sha256": None,
                               "error": "ValueError"}
    writer.write("sha256", rows())


# ---------------------------------------------------------------- main

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=parity_env.CONTRACTS_DIR / "compat-vectors")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    sandbox = Path(tempfile.mkdtemp(prefix="plumdeck-vectors-"))
    parity_env.sandbox_env(sandbox)
    parity_env.require_sandbox_env()
    parity_env.use_backend()
    os.chdir(sandbox)
    parity_env.require_sandbox_settings()

    writer = Writer(args.out)
    numeric_vectors(writer, random.Random(SEED))
    json_vectors(writer, random.Random(SEED + 1))
    text_vectors(writer, random.Random(SEED + 2))
    path_vectors(writer, random.Random(SEED + 3))
    time_vectors(writer, random.Random(SEED + 4))
    hash_vectors(writer)
    float_parse_vectors(writer, random.Random(SEED + 5))
    # The interpreter is part of the contract: str/unicodedata/pathlib rules
    # differ between CPython minor versions (3.11 ships Unicode 14.0.0).
    meta = {"implementation": platform.python_implementation(),
            "python": platform.python_version(), "unicode": unicodedata.unidata_version}
    (args.out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="ascii")
    print(f"python {sys.version.split()[0]} unicode {unicodedata.unidata_version}")
    for name, count in writer.counts.items():
        print(f"{name}: {count}")


if __name__ == "__main__":
    main()
