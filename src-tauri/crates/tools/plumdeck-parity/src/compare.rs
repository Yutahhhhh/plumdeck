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
//! - N6: `ties` rules sort runs of equal sort keys (see [`Ties`]).
//!
//! Deviations (contracts/parity/deviations.yaml) are applied per step by the
//! scenario runner, not here.

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

/// N6: an array whose order is undefined only among elements that agree on
/// `by` (e.g. `ORDER BY count DESC` without a tie-breaker). Each run of
/// consecutive elements with the same `by` value is sorted by its canonical
/// JSON (keys sorted, see [`canonical`]) on both sides; the order between
/// runs is still compared.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ties {
    /// JSONPath subset (see [`Tolerance::path`]) of the array.
    pub path: String,
    /// Member of each element that the query orders by.
    pub by: String,
}

/// Apply [`Ties`] rules to `value` in place.
pub fn normalize_ties(value: &mut Value, rules: &[Ties]) {
    let patterns: Vec<(Vec<Pat>, &str)> = rules.iter().map(|rule| (parse_pattern(&rule.path), rule.by.as_str())).collect();
    walk_ties(value, &mut Vec::new(), &patterns);
}

fn walk_ties(value: &mut Value, path: &mut Vec<Seg>, rules: &[(Vec<Pat>, &str)]) {
    match value {
        Value::Array(items) => {
            for (pattern, by) in rules {
                if matches(pattern, path) {
                    sort_runs(items, by);
                }
            }
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Seg::Index(index));
                walk_ties(item, path, rules);
                path.pop();
            }
        }
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                path.push(Seg::Key(key.clone()));
                walk_ties(item, path, rules);
                path.pop();
            }
        }
        _ => {}
    }
}

fn sort_runs(items: &mut [Value], by: &str) {
    let mut start = 0;
    while start < items.len() {
        let key = items[start].get(by).cloned();
        let mut end = start + 1;
        while end < items.len() && items[end].get(by).cloned() == key {
            end += 1;
        }
        items[start..end].sort_by_cached_key(canonical);
        start = end;
    }
}

/// JSON text with object keys sorted at every level: the same value gives
/// the same text whatever key order a side wrote (N1 ignores key order).
pub fn canonical(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(pairs.into_iter().map(|(key, item)| (key.clone(), sorted(item))).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}

/// One side's HTTP response.
#[derive(Clone, Debug)]
pub struct Captured {
    pub status: u16,
    pub content_type: String,
    /// Lowercased names, in arrival order.
    pub headers: Vec<(String, String)>,
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

    /// A copy whose header values read `:{port}` where they had `:<port>`.
    pub fn with_port_placeholder(&self, port: u16) -> Self {
        let needle = format!(":{port}");
        let headers = self.headers.iter().map(|(name, value)| (name.clone(), value.replace(&needle, ":{port}"))).collect();
        Self { headers, ..self.clone() }
    }

    /// Every value of header `name`, joined with ", "; `None` when absent.
    pub fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        let values: Vec<&str> =
            self.headers.iter().filter(|(key, _)| *key == name).map(|(_, value)| value.as_str()).collect();
        (!values.is_empty()).then(|| values.join(", "))
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

/// N14: headers compared only where a step asks (CORS scenarios).
pub fn compare_headers(a: &Captured, b: &Captured, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| {
            let (va, vb) = (a.header(name), b.header(name));
            (va != vb).then(|| format!("header {name}: {va:?} vs {vb:?}"))
        })
        .collect()
}

/// Compare A (Python) with B (Rust).
#[cfg(test)]
pub fn compare_responses(a: &Captured, b: &Captured, comparer: &JsonComparer) -> Verdict {
    compare_responses_with(a, b, comparer, &[])
}

/// [`compare_responses`] with N6 tie rules applied to JSON bodies.
pub fn compare_responses_with(a: &Captured, b: &Captured, comparer: &JsonComparer, ties: &[Ties]) -> Verdict {
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
            if !ties.is_empty() {
                normalize_ties(&mut ja, ties);
                normalize_ties(&mut jb, ties);
            }
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
        Captured { status, content_type: content_type.into(), headers: Vec::new(), body: body.as_bytes().to_vec() }
    }

    #[test]
    fn ties_ignore_each_sides_key_order() {
        let rules = [Ties { path: "$".into(), by: "t".into() }];
        let mut a = json!([{"d": "x", "id": 2, "t": 1}, {"d": null, "id": 1, "t": 1}]);
        let mut b: Value = serde_json::from_str(r#"[{"id": 1, "d": null, "t": 1}, {"id": 2, "d": "x", "t": 1}]"#).unwrap();
        normalize_ties(&mut a, &rules);
        normalize_ties(&mut b, &rules);
        assert_eq!(a, b);
        assert_eq!(canonical(&json!({"b": [{"z": 1, "a": 2}], "a": null})), r#"{"a":null,"b":[{"a":2,"z":1}]}"#);
    }

    #[test]
    fn ties_sort_only_within_equal_keys() {
        let rules = [Ties { path: "$.genres".into(), by: "count".into() }];
        let comparer = JsonComparer::new(&[]);
        let a = captured(200, "application/json", r#"{"genres":[{"name":"b","count":3},{"name":"x","count":2},{"name":"a","count":2}]}"#);
        let b = captured(200, "application/json", r#"{"genres":[{"name":"b","count":3},{"name":"a","count":2},{"name":"x","count":2}]}"#);
        assert!(compare_responses_with(&a, &b, &comparer, &rules).diffs.is_empty());
        assert_eq!(compare_responses(&a, &b, &comparer).diffs.len(), 2);
        // The order between different counts still matters.
        let c = captured(200, "application/json", r#"{"genres":[{"name":"x","count":2},{"name":"a","count":2},{"name":"b","count":3}]}"#);
        assert!(!compare_responses_with(&a, &c, &comparer, &rules).diffs.is_empty());
    }

    #[test]
    fn headers_compare_all_values() {
        let mut a = captured(200, "text/plain", "");
        let mut b = a.clone();
        a.headers = vec![("vary".into(), "Origin".into())];
        b.headers = vec![("vary".into(), "Accept".into()), ("vary".into(), "Origin".into())];
        assert_eq!(b.header("Vary").as_deref(), Some("Accept, Origin"));
        assert_eq!(compare_headers(&a, &b, &["vary".into(), "x-missing".into()]).len(), 1);
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
