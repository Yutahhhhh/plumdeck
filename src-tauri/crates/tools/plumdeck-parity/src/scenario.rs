//! Scenario files (plan §4.4) and the `run` subcommand.
//!
//! ```yaml
//! name: s0-root
//! db: fixture            # fixture | real (real is refused for now)
//! steps:
//!   - name: health check
//!     expect_status: 200 # optional: both sides must answer this
//!     http: {method: GET, path: /, query: {q: x, ids: [1, 2]}, headers: {}, body: null}
//! compare_overrides:
//!   tolerances: [{path: "$..score", mode: rel, value: 1e-9}]
//! ```
//! S0 executes `http` steps; other step kinds are rejected so a scenario
//! never silently passes without being run. Query arrays are sent as
//! repeated keys (`ids=1&ids=2`, FastAPI's list parameters); `body` is sent as
//! JSON. Raw, form and multipart bodies arrive with the slices that use them.
//!
//! `db: real` is refused until plan §4.2's safety rails exist: the runner's
//! denylist of operations that rewrite or delete the owner's audio files and
//! the matching oracle monkeypatches. A real-DB copy's file paths point at the
//! owner's actual music.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::compare::{compare_responses, Captured, JsonComparer, Tolerance, Verdict};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "default_db")]
    pub db: String,
    #[serde(default)]
    pub freeze_now: Option<String>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub compare_overrides: Overrides,
    #[serde(default)]
    pub side_effects: Vec<Value>,
}

fn default_db() -> String {
    "fixture".into()
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default)]
    pub tolerances: Vec<Tolerance>,
}

/// Global rules shared by every scenario (contracts/parity/normalize.yaml).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizeFile {
    #[serde(default)]
    pub tolerances: Vec<Tolerance>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub expect_status: Option<u16>,
    #[serde(default)]
    pub http: Option<HttpStep>,
    #[serde(default)]
    pub mcp: Option<Value>,
    #[serde(default)]
    pub ws: Option<Value>,
    #[serde(default)]
    pub wait: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpStep {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, Value>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<Value>,
}

impl Step {
    fn label(&self, index: usize) -> String {
        match (&self.name, &self.http) {
            (Some(name), Some(http)) => format!("{name}: {} {}", http.method, http.path),
            (Some(name), None) => name.clone(),
            (None, Some(http)) => format!("{} {}", http.method, http.path),
            (None, None) => format!("step {}", index + 1),
        }
    }
}

pub fn load(path: &Path) -> Result<Scenario> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let scenario: Scenario =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    match scenario.db.as_str() {
        "fixture" => {}
        "real" => bail!(
            "{}: db: real is not supported until the plan §4.2 denylist and oracle patches exist \
             (a real-DB copy's file paths point at the owner's music)",
            path.display()
        ),
        other => bail!("{}: db must be fixture or real, got {other:?}", path.display()),
    }
    if scenario.freeze_now.is_some() || !scenario.side_effects.is_empty() {
        bail!("{}: freeze_now and side_effects are not supported yet", path.display());
    }
    for (index, step) in scenario.steps.iter().enumerate() {
        let kinds = [step.http.is_some(), step.mcp.is_some(), step.ws.is_some(), step.wait.is_some()];
        if kinds.iter().filter(|k| **k).count() != 1 {
            bail!("{}: step {} needs exactly one of http/mcp/ws/wait", path.display(), index + 1);
        }
        let Some(http) = &step.http else {
            bail!("{}: step {}: only http steps are supported yet", path.display(), index + 1);
        };
        for (key, value) in &http.query {
            let scalar = |v: &Value| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_));
            let ok = match value {
                Value::Array(items) => items.iter().all(scalar),
                other => scalar(other),
            };
            if !ok {
                bail!(
                    "{}: step {}: query {key:?} must be a scalar or an array of scalars",
                    path.display(),
                    index + 1
                );
            }
        }
    }
    Ok(scenario)
}

/// Query pairs in order; an array becomes one pair per element.
fn query_pairs(query: &BTreeMap<String, Value>) -> Vec<(String, String)> {
    let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
    let mut pairs = Vec::new();
    for (key, value) in query {
        match value {
            Value::Array(items) => pairs.extend(items.iter().map(|item| (key.clone(), text(item)))),
            other => pairs.push((key.clone(), text(other))),
        }
    }
    pairs
}

/// `contracts/parity/normalize.yaml` next to `scenarios/`, if present.
pub fn default_normalize_path(scenario: &Path) -> Option<PathBuf> {
    let candidate = scenario.parent()?.parent()?.join("normalize.yaml");
    candidate.is_file().then_some(candidate)
}

pub fn load_normalize(path: Option<&Path>) -> Result<NormalizeFile> {
    match path {
        None => Ok(NormalizeFile::default()),
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))
        }
    }
}

async fn send(client: &reqwest::Client, base: &str, step: &HttpStep) -> Result<(Captured, Duration)> {
    let method = reqwest::Method::from_bytes(step.method.to_ascii_uppercase().as_bytes())
        .with_context(|| format!("method {}", step.method))?;
    let url = format!("{}{}", base.trim_end_matches('/'), step.path);
    let mut request = client.request(method, &url);
    if !step.query.is_empty() {
        request = request.query(&query_pairs(&step.query));
    }
    for (name, value) in &step.headers {
        request = request.header(name, value);
    }
    if let Some(body) = &step.body {
        request = request.json(body);
    }
    let started = Instant::now();
    let response = request.send().await.with_context(|| format!("request {url}"))?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = response.bytes().await.with_context(|| format!("read body {url}"))?.to_vec();
    Ok((Captured { status, content_type, body }, started.elapsed()))
}

/// One compared step for the report.
pub struct StepResult {
    pub label: String,
    pub a_status: u16,
    pub b_status: u16,
    pub a_ms: f64,
    pub b_ms: f64,
    pub verdict: Verdict,
}

pub struct RunReport {
    pub scenario: String,
    pub steps: Vec<StepResult>,
}

impl RunReport {
    pub fn unexplained(&self) -> usize {
        self.steps.iter().map(|s| s.verdict.diffs.len()).sum()
    }

    pub fn print(&self, a: &str, b: &str) {
        println!("scenario {}  A={a}  B={b}", self.scenario);
        for step in &self.steps {
            let tag = if !step.verdict.diffs.is_empty() {
                "DIFF"
            } else if step.verdict.skipped {
                "SKIP"
            } else {
                "ok"
            };
            println!(
                "  [{tag:>4}] {} -> {} | {}  ({:.1} ms | {:.1} ms)",
                step.label, step.a_status, step.b_status, step.a_ms, step.b_ms
            );
            for diff in &step.verdict.diffs {
                println!("         diff: {diff}");
            }
            for note in &step.verdict.notes {
                println!("         note: {note}");
            }
        }
        let skipped = self.steps.iter().filter(|s| s.verdict.skipped).count();
        println!(
            "summary: {} steps, {} skipped (N12), {} unexplained diffs",
            self.steps.len(),
            skipped,
            self.unexplained()
        );
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "scenario": self.scenario,
            "unexplained_diffs": self.unexplained(),
            "steps": self.steps.iter().map(|s| serde_json::json!({
                "step": s.label,
                "status": [s.a_status, s.b_status],
                "latency_ms": [s.a_ms, s.b_ms],
                "skipped": s.verdict.skipped,
                "diffs": s.verdict.diffs,
                "notes": s.verdict.notes,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Send every step to A then B, in order, and compare.
pub async fn run(scenario: &Scenario, global: &NormalizeFile, a: &str, b: &str) -> Result<RunReport> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        // Both sides are loopback servers; never route them through a proxy.
        .no_proxy()
        .build()?;
    let mut tolerances = global.tolerances.clone();
    tolerances.extend(scenario.compare_overrides.tolerances.iter().cloned());
    let comparer = JsonComparer::new(&tolerances);
    let mut steps = Vec::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        let http = step.http.as_ref().expect("validated in load");
        let (ra, ta) = send(&client, a, http).await?;
        let (rb, tb) = send(&client, b, http).await?;
        let mut verdict = compare_responses(&ra, &rb, &comparer);
        if let Some(expected) = step.expect_status {
            for (side, got) in [("A", ra.status), ("B", rb.status)] {
                if got != expected {
                    verdict.diffs.push(format!("{side} status {got}, expected {expected}"));
                }
            }
        }
        steps.push(StepResult {
            label: step.label(index),
            a_status: ra.status,
            b_status: rb.status,
            a_ms: ta.as_secs_f64() * 1000.0,
            b_ms: tb.as_secs_f64() * 1000.0,
            verdict,
        });
    }
    Ok(RunReport { scenario: scenario.name.clone(), steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("plumdeck-scenario-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn real_db_scenarios_are_refused() {
        let path = write("real.yaml", "name: r\ndb: real\nsteps:\n  - http: {method: GET, path: /}\n");
        let error = load(&path).unwrap_err().to_string();
        assert!(error.contains("db: real is not supported"), "{error}");
        let path = write("fixture.yaml", "name: f\nsteps:\n  - http: {method: GET, path: /}\n");
        assert_eq!(load(&path).unwrap().db, "fixture");
    }

    #[test]
    fn query_arrays_repeat_the_key() {
        let path = write(
            "query.yaml",
            "name: q\nsteps:\n  - http: {method: GET, path: /, query: {ids: [1, 2], q: x, on: true}}\n",
        );
        let scenario = load(&path).unwrap();
        let pairs = query_pairs(&scenario.steps[0].http.as_ref().unwrap().query);
        let expected = [("ids", "1"), ("ids", "2"), ("on", "true"), ("q", "x")];
        assert_eq!(pairs, expected.map(|(k, v)| (k.to_owned(), v.to_owned())));
        let path = write("bad.yaml", "name: b\nsteps:\n  - http: {method: GET, path: /, query: {f: {a: 1}}}\n");
        assert!(load(&path).is_err());
    }
}
