//! Replays every case in contracts/compat-vectors (written by CPython via
//! tools/parity/export_vectors.py) and requires 100% agreement.

use std::fs;
use std::path::PathBuf;

use chrono::NaiveDate;
use plumdeck_core::compat::pathnorm::{
    nt_normcase, nt_normpath, normalize_path, path_variants, posix_file_uri, windows_file_uri,
    PureParts,
};
use plumdeck_core::compat::pyfmt::{
    float_divmod, float_floordiv, float_mod, float_repr, format_float, int_floordiv, int_mod,
    int_trunc, median, percent_format, round_ndigits, round_to_int, PyNumError,
};
use plumdeck_core::compat::pyjson::{dumps, dumps_json, DumpError, DumpOptions, PyValue};
use plumdeck_core::compat::text::{
    casefold, is_py_space, is_word_char, nfc, nfd, nfkc, nfkd, normalize_text, py_lower, py_split,
    py_strip, search_tokens, tokens,
};
use plumdeck_core::time::{py_date_isoformat, py_isoformat, py_str_datetime};
use serde_json::Value;

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/compat-vectors")
}

fn load(name: &str) -> Vec<Value> {
    let path = vectors_dir().join(format!("{name}.jsonl"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let rows: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert!(rows.len() >= 20, "{name}: only {} cases", rows.len());
    rows
}

fn bits(value: &Value) -> f64 {
    let text = value.as_str().expect("bit pattern");
    f64::from_bits(u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap())
}

fn same_float(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn str_of(value: &Value) -> &str {
    value.as_str().expect("string")
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| str_of(v).to_owned()).collect()
}

/// Collects mismatches and fails once with a sample, so one run shows the
/// size of a regression instead of only its first case.
struct Check {
    name: &'static str,
    total: usize,
    failures: Vec<String>,
}

impl Check {
    fn new(name: &'static str) -> Self {
        Self { name, total: 0, failures: Vec::new() }
    }

    fn eq<T: PartialEq + std::fmt::Debug>(&mut self, context: impl std::fmt::Debug, got: T, want: T) {
        self.total += 1;
        if got != want {
            self.failures.push(format!("{context:?}: got {got:?}, want {want:?}"));
        }
    }

    fn finish(self) {
        assert!(self.total > 0, "{}: no checks ran", self.name);
        if !self.failures.is_empty() {
            let sample: Vec<_> = self.failures.iter().take(15).collect();
            panic!("{}: {}/{} mismatches\n{:#?}", self.name, self.failures.len(), self.total, sample);
        }
    }
}

#[test]
fn float_repr_matches_python() {
    let mut check = Check::new("float_repr");
    for row in load("float_repr") {
        let x = bits(&row["x"]);
        check.eq(&row["x"], float_repr(x), str_of(&row["repr"]).to_owned());
    }
    check.finish();
}

#[test]
fn float_text_round_trips_like_python() {
    // Python reads its own float text back exactly (json.loads, pydantic);
    // serde_json must too (float_roundtrip) and the encoder must write the
    // same bytes again.
    let mut check = Check::new("float_parse");
    for row in load("float_parse") {
        let text = str_of(&row["text"]);
        let want = bits(&row["x"]);
        let parsed: Value = serde_json::from_str(text).unwrap();
        check.eq((text, "parse"), parsed.as_f64().map(f64::to_bits), Some(want.to_bits()));
        check.eq((text, "dumps"), dumps_json(&parsed, &DumpOptions::default()).ok(), Some(text.to_owned()));
        check.eq((text, "repr"), float_repr(want), text.to_owned());
    }
    check.finish();
}

#[test]
fn vectors_come_from_the_reference_interpreter() {
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(vectors_dir().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["implementation"], "CPython");
    let python = str_of(&meta["python"]);
    assert!(
        python.starts_with(&format!("{}.", plumdeck_core::compat::REFERENCE_PYTHON)),
        "vectors from Python {python}, compat layer targets {}",
        plumdeck_core::compat::REFERENCE_PYTHON
    );
    assert_eq!(meta["unicode"], plumdeck_core::compat::UNICODE_VERSION);
    let (major, minor, patch) = unicode_normalization::UNICODE_VERSION;
    assert_eq!(meta["unicode"], format!("{major}.{minor}.{patch}"), "unicode-normalization tables");
}

#[test]
fn float_format_matches_python() {
    let mut check = Check::new("float_format");
    for row in load("float_format") {
        let x = bits(&row["x"]);
        let spec = str_of(&row["spec"]);
        let got = if spec.starts_with('%') { percent_format(spec, x) } else { format_float(x, spec) };
        check.eq((&row["x"], spec), got.unwrap(), str_of(&row["out"]).to_owned());
    }
    check.finish();
}

#[test]
fn round_matches_python() {
    let mut check = Check::new("round");
    for row in load("round") {
        let x = bits(&row["x"]);
        let context = (&row["x"], &row["ndigits"]);
        match row["ndigits"].as_u64() {
            None => {
                let want = match row["error"].as_str() {
                    Some("ValueError") => Err(PyNumError::ValueError),
                    Some("OverflowError") => Err(PyNumError::OverflowError),
                    _ => Ok(str_of(&row["out"]).parse::<i64>().unwrap()),
                };
                check.eq(context, round_to_int(x), want);
            }
            Some(ndigits) => {
                let got = round_ndigits(x, ndigits as u32);
                let want = bits(&row["out"]);
                check.eq(context, same_float(got, want), true);
            }
        }
    }
    check.finish();
}

#[test]
fn int_trunc_matches_python() {
    let mut check = Check::new("int_trunc");
    for row in load("int_trunc") {
        let want = match row["error"].as_str() {
            Some("ValueError") => Err(PyNumError::ValueError),
            Some("OverflowError") => Err(PyNumError::OverflowError),
            _ => Ok(str_of(&row["out"]).parse::<i64>().unwrap()),
        };
        check.eq(&row["x"], int_trunc(bits(&row["x"])), want);
    }
    check.finish();
}

#[test]
fn float_floordiv_and_mod_match_python() {
    let mut check = Check::new("divmod_float");
    for row in load("divmod_float") {
        let (a, b) = (bits(&row["a"]), bits(&row["b"]));
        let context = (&row["a"], &row["b"]);
        if row["error"].is_string() {
            check.eq(context, float_divmod(a, b).is_err() && float_mod(a, b).is_err(), true);
            continue;
        }
        let (div, modulo) = float_divmod(a, b).unwrap();
        let want_div = bits(&row["floordiv"]);
        let want_mod = bits(&row["mod"]);
        check.eq(context, same_float(div, want_div), true);
        check.eq(context, same_float(float_floordiv(a, b).unwrap(), want_div), true);
        check.eq(context, same_float(modulo, want_mod), true);
        check.eq(context, same_float(float_mod(a, b).unwrap(), want_mod), true);
    }
    check.finish();
}

#[test]
fn int_floordiv_and_mod_match_python() {
    let mut check = Check::new("divmod_int");
    for row in load("divmod_int") {
        let a: i64 = str_of(&row["a"]).parse().unwrap();
        let b: i64 = str_of(&row["b"]).parse().unwrap();
        if row["error"].is_string() {
            check.eq((a, b), int_floordiv(a, b), Err(PyNumError::ZeroDivisionError));
            check.eq((a, b), int_mod(a, b), Err(PyNumError::ZeroDivisionError));
            continue;
        }
        check.eq((a, b), int_floordiv(a, b), Ok(str_of(&row["floordiv"]).parse().unwrap()));
        check.eq((a, b), int_mod(a, b), Ok(str_of(&row["mod"]).parse().unwrap()));
    }
    check.finish();
}

#[test]
fn median_matches_python() {
    let mut check = Check::new("median");
    for row in load("median") {
        let values: Vec<f64> = row["values"].as_array().unwrap().iter().map(bits).collect();
        match row["error"].as_str() {
            Some(_) => check.eq(&row["values"], median(&values), Err(PyNumError::StatisticsError)),
            None => {
                let got = median(&values).unwrap();
                check.eq(&row["values"], same_float(got, bits(&row["out"])), true);
            }
        }
    }
    check.finish();
}

fn decode(value: &Value) -> PyValue {
    match value {
        Value::Null => PyValue::None,
        Value::Bool(b) => PyValue::Bool(*b),
        Value::Object(map) => {
            let (tag, inner) = map.iter().next().expect("tagged value");
            match tag.as_str() {
                "int" => PyValue::Int(str_of(inner).parse().unwrap()),
                "float" => PyValue::Float(bits(inner)),
                "str" => PyValue::Str(str_of(inner).to_owned()),
                "list" => PyValue::List(inner.as_array().unwrap().iter().map(decode).collect()),
                "dict" => PyValue::Dict(
                    inner
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|pair| (decode(&pair[0]), decode(&pair[1])))
                        .collect(),
                ),
                "datetime" => PyValue::DateTime(
                    chrono::NaiveDateTime::parse_from_str(str_of(inner), "%Y-%m-%dT%H:%M:%S%.f")
                        .unwrap(),
                ),
                "date" => {
                    PyValue::Date(NaiveDate::parse_from_str(str_of(inner), "%Y-%m-%d").unwrap())
                }
                other => panic!("unknown tag {other}"),
            }
        }
        other => panic!("untagged value {other}"),
    }
}

#[test]
fn json_dumps_matches_python() {
    let mut check = Check::new("pyjson");
    for row in load("pyjson") {
        let value = decode(&row["value"]);
        let opts = &row["opts"];
        let options = DumpOptions {
            ensure_ascii: opts["ensure_ascii"].as_bool().unwrap(),
            separators: opts["separators"]
                .as_array()
                .map(|pair| (str_of(&pair[0]).to_owned(), str_of(&pair[1]).to_owned())),
            indent: opts["indent"].as_u64().map(|n| n as usize),
            allow_nan: opts["allow_nan"].as_bool().unwrap(),
            default_str: opts["default_str"].as_bool().unwrap(),
        };
        let got = dumps(&value, &options);
        let context = (&row["value"], opts);
        match row["error"].as_str() {
            Some("ValueError") => check.eq(context, matches!(got, Err(DumpError::ValueError(_))), true),
            Some("TypeError") => check.eq(context, matches!(got, Err(DumpError::TypeError(_))), true),
            Some(other) => panic!("unexpected error kind {other}"),
            None => check.eq(context, got, Ok(str_of(&row["out"]).to_owned())),
        }
    }
    check.finish();
}

#[test]
fn text_functions_match_python() {
    let mut check = Check::new("text");
    for row in load("text") {
        let s = str_of(&row["s"]);
        check.eq((s, "nfc"), nfc(s), str_of(&row["nfc"]).to_owned());
        check.eq((s, "nfd"), nfd(s), str_of(&row["nfd"]).to_owned());
        check.eq((s, "nfkc"), nfkc(s), str_of(&row["nfkc"]).to_owned());
        check.eq((s, "nfkd"), nfkd(s), str_of(&row["nfkd"]).to_owned());
        check.eq((s, "casefold"), casefold(s), str_of(&row["casefold"]).to_owned());
        check.eq((s, "lower"), py_lower(s), str_of(&row["lower"]).to_owned());
        check.eq((s, "split"), py_split(s), strings(&row["split"]).iter().map(String::as_str).collect());
        check.eq((s, "strip"), py_strip(s), str_of(&row["strip"]));
        check.eq((s, "normalize_text"), normalize_text(Some(s)), str_of(&row["normalize_text"]).to_owned());
        check.eq((s, "tokens"), tokens(s), strings(&row["tokens"]).iter().map(String::as_str).collect());
        check.eq(
            (s, "search_tokens"),
            search_tokens(s),
            strings(&row["search_tokens"]).iter().map(String::as_str).collect(),
        );
    }
    check.finish();
}

#[test]
fn per_codepoint_mappings_match_python() {
    let mut check = Check::new("text_codepoints");
    for row in load("text_codepoints") {
        let cp = row["cp"].as_u64().unwrap() as u32;
        let s = char::from_u32(cp).unwrap().to_string();
        for (key, got) in [
            ("casefold", casefold(&s)),
            ("lower", py_lower(&s)),
            ("nfc", nfc(&s)),
            ("nfd", nfd(&s)),
            ("nfkc", nfkc(&s)),
            ("nfkd", nfkd(&s)),
        ] {
            check.eq((cp, key), got, str_of(&row[key]).to_owned());
        }
    }
    check.finish();
}

#[test]
fn character_classes_match_python_exhaustively() {
    let path = vectors_dir().join("text_classes.json");
    let classes: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(classes["unicode"], plumdeck_core::compat::UNICODE_VERSION);
    let ranges: Vec<(u32, u32)> = classes["word_ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32))
        .collect();
    let spaces: Vec<u32> =
        classes["space"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
    let mut word = vec![false; 0x110000];
    for (lo, hi) in ranges {
        word[lo as usize..=hi as usize].fill(true);
    }
    let mut space = vec![false; 0x110000];
    for code in spaces {
        space[code as usize] = true;
    }
    let mut check = Check::new("text_classes");
    for c in (0..=0x10ffffu32).filter_map(char::from_u32) {
        let code = c as usize;
        check.eq((code, "word"), is_word_char(c), word[code]);
        check.eq((code, "space"), is_py_space(c), space[code]);
    }
    check.finish();
}

#[test]
fn path_rules_match_python() {
    let mut check = Check::new("pathnorm");
    let opt = |v: &Value| v.as_str().map(str::to_owned);
    for row in load("pathnorm") {
        let p = str_of(&row["path"]);
        check.eq((p, "posix_str"), PureParts::posix(p).to_str(), str_of(&row["posix_str"]).to_owned());
        check.eq((p, "windows_str"), PureParts::windows(p).to_str(), str_of(&row["windows_str"]).to_owned());
        check.eq((p, "posix_uri"), posix_file_uri(p), opt(&row["posix_uri"]));
        check.eq((p, "windows_uri"), windows_file_uri(p), opt(&row["windows_uri"]));
        check.eq((p, "normpath"), nt_normpath(p), str_of(&row["ntpath_normpath"]).to_owned());
        check.eq((p, "normcase"), nt_normcase(p), str_of(&row["ntpath_normcase"]).to_owned());
        check.eq((p, "norm_posix"), normalize_path(p, false), str_of(&row["normalize_path_posix"]).to_owned());
        check.eq((p, "norm_win"), normalize_path(p, true), str_of(&row["normalize_path_windows"]).to_owned());
        check.eq((p, "variants_posix"), path_variants(p, false), strings(&row["path_variants_posix"]));
        check.eq((p, "variants_win"), path_variants(p, true), strings(&row["path_variants_windows"]));
    }
    check.finish();
}

#[test]
fn datetime_strings_match_python() {
    let mut check = Check::new("time");
    for row in load("time") {
        let f: Vec<u32> = row["dt"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
        let stamp = NaiveDate::from_ymd_opt(f[0] as i32, f[1], f[2])
            .unwrap()
            .and_hms_micro_opt(f[3], f[4], f[5], f[6])
            .unwrap();
        check.eq(&row["dt"], py_isoformat(&stamp), str_of(&row["isoformat"]).to_owned());
        check.eq(&row["dt"], py_str_datetime(&stamp), str_of(&row["str"]).to_owned());
        check.eq(&row["dt"], py_date_isoformat(&stamp.date()), str_of(&row["date_isoformat"]).to_owned());
    }
    check.finish();
}

#[test]
fn sha256_chunks_and_limit_match_python() {
    use plumdeck_core::hash::{sha256_file, HashError};
    let dir = std::env::temp_dir().join(format!("plumdeck-core-sha-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut check = Check::new("sha256");
    for row in load_small("sha256") {
        let len = row["len"].as_u64().unwrap() as usize;
        let content: Vec<u8> = (0..len).map(|i| ((i * 31 + 7) % 256) as u8).collect();
        let path = dir.join(format!("f{len}"));
        fs::write(&path, &content).unwrap();
        let got = sha256_file(&path, row["limit"].as_u64());
        match row["error"].as_str() {
            Some(_) => check.eq((len, &row["limit"]), matches!(got, Err(HashError::TooLarge)), true),
            None => check.eq((len, &row["limit"]), got.unwrap(), str_of(&row["sha256"]).to_owned()),
        }
    }
    fs::remove_dir_all(&dir).ok();
    check.finish();
}

fn load_small(name: &str) -> Vec<Value> {
    let path = vectors_dir().join(format!("{name}.jsonl"));
    fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}
