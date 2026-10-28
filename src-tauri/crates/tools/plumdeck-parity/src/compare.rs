//! Response normalization and comparison (plan §4.5).
//!
//! Implemented rules:
//! - N1: bodies are parsed as JSON; object key order is ignored, key sets
//!   must match.
//! - N2: numbers compare exactly (and must agree on int vs float) unless a
//!   tolerance rule for their JSON path says otherwise.
//! - N7: error responses compare status and `detail` only; 422 validation
//!   errors only need an array detail on both sides.
//! - N12: a 500 from side A (Python) is undefined behavior and skipped.

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
    fn media_type(&self) -> &str {
        self.content_type.split(';').next().unwrap_or("").trim()
    }

    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// FastAPI's `{"detail": ...}`, or the text of a Starlette plain-text
    /// error (unmatched routes fall through to the MCP mount in Python).
    pub fn detail(&self) -> Option<Value> {
        if let Some(Value::Object(map)) = self.json() {
            return map.get("detail").cloned();
        }
        if self.media_type() == "text/plain" {
            return Some(Value::String(String::from_utf8_lossy(&self.body).into_owned()));
        }
        None
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
    if a.status >= 400 {
        match (a.detail(), b.detail()) {
            (Some(Value::Array(_)), Some(Value::Array(_))) if a.status == 422 => {
                verdict.notes.push("N7: 422 validation detail compared by shape only".into());
            }
            (Some(da), Some(db)) => {
                let diffs = comparer.diff(&da, &db);
                if diffs.is_empty() {
                    verdict.notes.push(format!("N7: status and detail match ({})", short(&da)));
                } else {
                    verdict.diffs.extend(diffs.into_iter().map(|d| format!("detail {d}")));
                }
            }
            (da, db) => verdict.diffs.push(format!(
                "error body without detail: {} vs {}",
                da.map_or_else(|| a.snippet(), |v| short(&v)),
                db.map_or_else(|| b.snippet(), |v| short(&v)),
            )),
        }
        if a.media_type() != b.media_type() {
            verdict.notes.push(format!(
                "N7: error content-type {:?} vs {:?} (detail compared instead)",
                a.content_type, b.content_type
            ));
        }
        return verdict;
    }
    if a.media_type() != b.media_type() {
        verdict.diffs.push(format!("content-type {:?} vs {:?}", a.content_type, b.content_type));
    }
    match (a.json(), b.json()) {
        (Some(ja), Some(jb)) => verdict.diffs.extend(comparer.diff(&ja, &jb)),
        _ if a.body == b.body => {}
        _ => verdict.diffs.push(format!("body bytes differ: {} vs {}", a.snippet(), b.snippet())),
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
    fn errors_compare_status_and_detail() {
        let comparer = JsonComparer::new(&[]);
        let python = captured(404, "text/plain; charset=utf-8", "Not Found");
        let rust = captured(404, "application/json", r#"{"detail":"Not Found"}"#);
        let verdict = compare_responses(&python, &rust, &comparer);
        assert!(verdict.diffs.is_empty(), "{:?}", verdict.diffs);
        let other = captured(404, "application/json", r#"{"detail":"Track not found"}"#);
        assert_eq!(compare_responses(&python, &other, &comparer).diffs.len(), 1);
        let crash = captured(500, "text/plain", "Internal Server Error");
        assert!(compare_responses(&crash, &rust, &comparer).skipped);
        assert_eq!(compare_responses(&rust, &crash, &comparer).diffs.len(), 1);
    }
}
