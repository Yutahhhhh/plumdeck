//! Response normalization and comparison (plan §4.5).
//!
//! Implemented rules:
//! - N1: JSON bodies (`application/json`, `*+json`) are parsed; object key
//!   order is ignored, key sets must match. Every other body must match byte
//!   for byte (N8).
//! - N2: numbers compare exactly (and must agree on int vs float) unless a
//!   tolerance rule for their JSON path says otherwise. HTTP JSON is compared
//!   by value, not by spelling: FastAPI writes response-model floats with
//!   pydantic-core (`0.00001`, `1e-7`) and dict routes with `json.dumps`
//!   (`1e-05`), so the text differs by route while the value does not.
//!   serde_json parses with `float_roundtrip`, so values are exact doubles.
//! - N7: errors compare the whole body like any other response; only a 422
//!   whose `detail` is an array on both sides compares that array by shape.
//! - N12: a 500 from side A (Python) is undefined behavior and skipped.
//! - N14: Content-Type must match in full (media type and parameters);
//!   other headers (date, content-length, server) are ignored.
//!
//! There is no deviation mechanism yet: until the owner approves entries in
//! contracts/parity/deviations.yaml, every difference is unexplained.

use serde::Deserialize;
use serde_json::Value;

/// How numbers at matching paths compare (N2).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToleranceMode {
    /// Same kind (int/float) and same value.
    Exact,
    /// Equal after rounding both to f32 (DuckDB FLOAT columns).
    F32,
    /// `|a - b| <= value * max(|a|, |b|)`.
    Rel,
    /// `|a - b| <= value`.
    Abs,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Tolerance {
    /// JSONPath subset: `$`, `.key`, `.*`, `[*]`, `[n]`, `..key`.
    pub path: String,
    pub mode: ToleranceMode,
    #[serde(default)]
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
}

#[derive(Clone, Debug, PartialEq)]
enum Pat {
    Key(String),
    AnyKey,
    AnyIndex,
    Index(usize),
    /// `..`: zero or more segments.
    Descend,
}

fn parse_pattern(path: &str) -> Vec<Pat> {
    let mut out = Vec::new();
    let mut rest = path.strip_prefix('$').unwrap_or(path);
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("..") {
            out.push(Pat::Descend);
            rest = after;
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            if end > 0 {
                out.push(if &rest[..end] == "*" { Pat::AnyKey } else { Pat::Key(rest[..end].into()) });
            }
            rest = &rest[end..];
        } else if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let name = &after[..end];
            out.push(if name == "*" { Pat::AnyKey } else { Pat::Key(name.into()) });
            rest = &after[end..];
        } else if let Some(after) = rest.strip_prefix('[') {
            let end = after.find(']').unwrap_or(after.len());
            let inner = &after[..end];
            out.push(match inner.parse::<usize>() {
                Ok(index) => Pat::Index(index),
                Err(_) if inner == "*" => Pat::AnyIndex,
                Err(_) => Pat::Key(inner.trim_matches(['\'', '"']).into()),
            });
            rest = after.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            out.push(Pat::Key(rest[..end].into()));
            rest = &rest[end..];
        }
    }
    out
}

fn matches(pattern: &[Pat], path: &[Seg]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(Pat::Descend), _) => {
            matches(&pattern[1..], path) || (!path.is_empty() && matches(pattern, &path[1..]))
        }
        (None, Some(_)) | (Some(_), None) => false,
        (Some(p), Some(s)) => {
            let ok = match (p, s) {
                (Pat::Key(k), Seg::Key(name)) => k == name,
                (Pat::AnyKey, Seg::Key(_)) => true,
                (Pat::AnyIndex, Seg::Index(_)) => true,
                (Pat::Index(i), Seg::Index(j)) => i == j,
                _ => false,
            };
            ok && matches(&pattern[1..], &path[1..])
        }
    }
}

fn render(path: &[Seg]) -> String {
    let mut out = String::from("$");
    for seg in path {
        match seg {
            Seg::Key(k) => {
                out.push('.');
                out.push_str(k);
            }
            Seg::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

/// Compares two JSON documents under N1/N2 and returns the differences.
pub struct JsonComparer {
    rules: Vec<(Vec<Pat>, Tolerance)>,
}

impl JsonComparer {
    pub fn new(tolerances: &[Tolerance]) -> Self {
        Self { rules: tolerances.iter().map(|t| (parse_pattern(&t.path), t.clone())).collect() }
    }

    fn rule_for(&self, path: &[Seg]) -> Option<&Tolerance> {
        // Later rules (scenario overrides) win over earlier (global) ones.
        self.rules.iter().rev().find(|(pattern, _)| matches(pattern, path)).map(|(_, rule)| rule)
    }

    pub fn diff(&self, a: &Value, b: &Value) -> Vec<String> {
        let mut out = Vec::new();
        self.walk(a, b, &mut Vec::new(), &mut out);
        out
    }

    fn walk(&self, a: &Value, b: &Value, path: &mut Vec<Seg>, out: &mut Vec<String>) {
        match (a, b) {
            (Value::Object(ma), Value::Object(mb)) => {
                for key in ma.keys().filter(|k| !mb.contains_key(*k)) {
                    out.push(format!("{}.{key}: only in A", render(path)));
                }
                for key in mb.keys().filter(|k| !ma.contains_key(*k)) {
                    out.push(format!("{}.{key}: only in B", render(path)));
                }
                for (key, va) in ma {
                    if let Some(vb) = mb.get(key) {
                        path.push(Seg::Key(key.clone()));
                        self.walk(va, vb, path, out);
                        path.pop();
                    }
                }
            }
            (Value::Array(la), Value::Array(lb)) => {
                if la.len() != lb.len() {
                    out.push(format!("{}: length {} vs {}", render(path), la.len(), lb.len()));
                }
                for (index, (va, vb)) in la.iter().zip(lb).enumerate() {
                    path.push(Seg::Index(index));
                    self.walk(va, vb, path, out);
                    path.pop();
                }
            }
            (Value::Number(na), Value::Number(nb)) => {
                if let Some(problem) = self.numbers(na, nb, path) {
                    out.push(format!("{}: {problem}", render(path)));
                }
            }
            _ if a == b => {}
            _ => out.push(format!("{}: {} vs {}", render(path), short(a), short(b))),
        }
    }

    fn numbers(&self, a: &serde_json::Number, b: &serde_json::Number, path: &[Seg]) -> Option<String> {
        let is_int = |n: &serde_json::Number| n.is_i64() || n.is_u64();
        let (fa, fb) = (a.as_f64().unwrap_or(f64::NAN), b.as_f64().unwrap_or(f64::NAN));
        let mode = self.rule_for(path).map(|r| (r.mode.clone(), r.value)).unwrap_or((ToleranceMode::Exact, 0.0));
        let equal = match mode.0 {
            ToleranceMode::Exact => {
                if is_int(a) != is_int(b) {
                    return Some(format!("number kind differs: {a} vs {b}"));
                }
                if is_int(a) { a == b } else { fa.to_bits() == fb.to_bits() }
            }
            ToleranceMode::F32 => (fa as f32) == (fb as f32),
            ToleranceMode::Rel => (fa - fb).abs() <= mode.1 * fa.abs().max(fb.abs()),
            ToleranceMode::Abs => (fa - fb).abs() <= mode.1,
        };
        (!equal).then(|| format!("{a} vs {b} ({:?})", mode.0))
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 120 {
        format!("{}...", text.chars().take(120).collect::<String>())
    } else {
        text
    }
}

/// One side's HTTP response.
#[derive(Clone, Debug)]
pub struct Captured {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Captured {
    fn media_type(&self) -> String {
        self.content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
    }

    fn is_json(&self) -> bool {
        let media = self.media_type();
        media == "application/json" || media.ends_with("+json")
    }

    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn snippet(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let mut out: String = text.chars().take(200).collect();
        if text.chars().count() > 200 {
            out.push_str("...");
        }
        out
    }
}

/// The comparison verdict for one step.
#[derive(Debug, Default)]
pub struct Verdict {
    /// Unexplained differences.
    pub diffs: Vec<String>,
    /// Differences absorbed by a normalization rule, kept for the report.
    pub notes: Vec<String>,
    /// N12: side A answered 500, so the step was not compared.
    pub skipped: bool,
}

/// N7: a pydantic validation 422 compares `detail` by shape only (an array
/// on both sides); the rest of the body still compares under N1.
fn without_validation_detail(a: &mut Value, b: &mut Value) -> bool {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb))
            if ma.get("detail").is_some_and(Value::is_array) && mb.get("detail").is_some_and(Value::is_array) =>
        {
            ma.insert("detail".into(), Value::Null);
            mb.insert("detail".into(), Value::Null);
            true
        }
        _ => false,
    }
}

/// Compare A (Python) with B (Rust).
pub fn compare_responses(a: &Captured, b: &Captured, comparer: &JsonComparer) -> Verdict {
    let mut verdict = Verdict::default();
    if a.status == 500 {
        verdict.skipped = true;
        verdict.notes.push(format!("N12: A returned 500; B returned {} {}", b.status, b.snippet()));
        return verdict;
    }
    if a.status != b.status {
        verdict.diffs.push(format!("status {} vs {}", a.status, b.status));
        return verdict;
    }
    if a.content_type != b.content_type {
        verdict.diffs.push(format!("content-type {:?} vs {:?}", a.content_type, b.content_type));
    }
    if a.is_json() && b.is_json() {
        if let (Some(mut ja), Some(mut jb)) = (a.json(), b.json()) {
            if a.status == 422 && without_validation_detail(&mut ja, &mut jb) {
                verdict.notes.push("N7: 422 validation detail compared by shape only".into());
            }
            verdict.diffs.extend(comparer.diff(&ja, &jb));
            return verdict;
        }
    }
    if a.body != b.body {
        verdict.diffs.push(format!("body bytes differ: {} vs {}", a.snippet(), b.snippet()));
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn captured(status: u16, content_type: &str, body: &str) -> Captured {
        Captured { status, content_type: content_type.into(), body: body.as_bytes().to_vec() }
    }

    #[test]
    fn key_order_and_exponent_spelling_are_ignored() {
        let comparer = JsonComparer::new(&[]);
        let a: Value = serde_json::from_str(r#"{"a": 1e-05, "b": [1, 2]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b": [1, 2], "a": 1e-5}"#).unwrap();
        assert!(comparer.diff(&a, &b).is_empty());
        assert_eq!(comparer.diff(&json!({"x": 1}), &json!({"x": 1.0})).len(), 1);
        assert_eq!(comparer.diff(&json!({"x": 1}), &json!({"y": 1})).len(), 2);
    }

    #[test]
    fn tolerance_rules_follow_paths() {
        let rules = [
            Tolerance { path: "$.items[*].score".into(), mode: ToleranceMode::Rel, value: 1e-9 },
            Tolerance { path: "$..bpm".into(), mode: ToleranceMode::F32, value: 0.0 },
        ];
        let comparer = JsonComparer::new(&rules);
        let a = json!({"items": [{"score": 1.0, "bpm": 127.9000015258789}], "bpm": 128});
        let b = json!({"items": [{"score": 1.0000000000001, "bpm": 127.9}], "bpm": 128.0});
        assert!(comparer.diff(&a, &b).is_empty(), "{:?}", comparer.diff(&a, &b));
        let c = json!({"items": [{"score": 1.1, "bpm": 127.9}], "bpm": 128});
        assert_eq!(comparer.diff(&a, &c).len(), 1);
    }

    #[test]
    fn exact_numbers_see_one_ulp() {
        // Python's text and a one-ULP-off re-encoding parse to different
        // doubles only with a correctly rounding parser (float_roundtrip).
        let comparer = JsonComparer::new(&[]);
        let a: Value = serde_json::from_str("[-0.11218375712633133]").unwrap();
        let b: Value = serde_json::from_str("[-0.11218375712633132]").unwrap();
        assert_eq!(a[0].as_f64().unwrap().to_bits(), 0xbfbcb81320000000);
        assert_eq!(comparer.diff(&a, &b).len(), 1);
    }

    #[test]
    fn errors_compare_the_whole_body_and_content_type() {
        let comparer = JsonComparer::new(&[]);
        let python = captured(404, "text/plain; charset=utf-8", "Not Found");
        let same = captured(404, "text/plain; charset=utf-8", "Not Found");
        assert!(compare_responses(&python, &same, &comparer).diffs.is_empty());
        // FastAPI's JSON spelling of the same message is still a difference.
        let json = captured(404, "application/json", r#"{"detail":"Not Found"}"#);
        assert_eq!(compare_responses(&python, &json, &comparer).diffs.len(), 2);
        let detail = captured(404, "application/json", r#"{"detail":"Lyrics not found"}"#);
        let extra = captured(404, "application/json", r#"{"detail":"Lyrics not found","extra":1}"#);
        assert_eq!(compare_responses(&detail, &extra, &comparer).diffs.len(), 1);
        let crash = captured(500, "text/plain", "Internal Server Error");
        assert!(compare_responses(&crash, &json, &comparer).skipped);
        assert_eq!(compare_responses(&json, &crash, &comparer).diffs.len(), 1);
    }

    #[test]
    fn validation_errors_compare_detail_by_shape() {
        let comparer = JsonComparer::new(&[]);
        let a = captured(422, "application/json", r#"{"detail":[{"type":"missing","loc":["body"]}]}"#);
        let b = captured(422, "application/json", r#"{"detail":[{"type":"int_parsing"}]}"#);
        let verdict = compare_responses(&a, &b, &comparer);
        assert!(verdict.diffs.is_empty(), "{:?}", verdict.diffs);
        let c = captured(422, "application/json", r#"{"detail":"Track is locked"}"#);
        assert_eq!(compare_responses(&a, &c, &comparer).diffs.len(), 1);
    }

    #[test]
    fn content_type_parameters_matter() {
        let comparer = JsonComparer::new(&[]);
        let a = captured(200, "application/json", "{}");
        let b = captured(200, "application/json; charset=latin-1", "{}");
        assert_eq!(compare_responses(&a, &b, &comparer).diffs.len(), 1);
        let m3u = captured(200, "application/x-mpegurl", "#EXTM3U\n");
        let m3u_utf8 = captured(200, "application/x-mpegurl; charset=utf-8", "#EXTM3U\n");
        assert_eq!(compare_responses(&m3u, &m3u_utf8, &comparer).diffs.len(), 1);
        let text = captured(200, "text/plain", "1.0");
        let other = captured(200, "text/plain", "1");
        assert_eq!(compare_responses(&text, &other, &comparer).diffs.len(), 1);
    }
}
