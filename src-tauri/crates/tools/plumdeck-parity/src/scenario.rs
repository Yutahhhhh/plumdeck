//! Scenario files (plan §4.4) and the `run` subcommand.
//!
//! ```yaml
//! name: s0-root
//! db: fixture            # fixture | real (real is refused for now)
//! fixture: populated     # plumdeck-parity suite: the fixture case both sides start from
//! steps:
//!   - name: health check
//!     expect_status: 200 # optional: both sides must answer this
//!     http: {method: GET, path: /, query: {q: x, ids: [1, 2]}, headers: {}, body: null}
//!   - name: raw body
//!     http: {method: POST, path: /api/settings, headers: {content-type: text/plain}, raw: "{bad"}
//!   - name: preflight
//!     compare_headers: [access-control-allow-origin, vary]   # N14
//!     http: {method: OPTIONS, path: /, headers: {origin: "http://127.0.0.1:{port}"}}
//!   - name: intentional difference
//!     deviation: {id: D-06, b_status: 421}
//!     http: {method: GET, path: /, headers: {host: evil.example}}
//! compare_overrides:
//!   tolerances: [{path: "$..score", mode: rel, value: 1e-9}]
//!   ties: [{path: "$.genre_distribution", by: count}]       # N6
//! ```
//! S0 executes `http` steps; other step kinds are rejected so a scenario
//! never silently passes without being run. Query arrays are sent as
//! repeated keys (`ids=1&ids=2`, FastAPI's list parameters); `body` is sent as
//! JSON, `raw` (text) or `raw_hex` (bytes) as they are with only the step's
//! headers. `{port}` in a header value is the side's own port (CORS origins
//! that main.py derives from PLUMDECK_PORT); compared headers map each
//! side's `:<port>` back to `:{port}`. `fixture_sql` (suite only) runs on the
//! fixture library before the sides start. `{media}` in `fixture_sql`, a
//! path, a query value, a header or a JSON body string is the scenario's synthetic media
//! directory (suite only; both sides read the same files).
//!
//! `compare_overrides.unordered` (or a step's `unordered`) lists JSON arrays
//! whose order is undefined (no ORDER BY in Python): both sides are sorted
//! before comparing (N6).
//!
//! A `deviation` step names an entry of contracts/parity/deviations.yaml:
//! A's answer is recorded but not compared, and B must answer `b_status`
//! (and `b_body`, when given). The report counts these by the entry's
//! status; only an approved entry is accepted, a proposed one is listed for
//! the owner (it is not an unexplained difference: the step pins B's
//! intended behavior).
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

use crate::compare::{compare_headers, compare_responses_with, Captured, JsonComparer, Ties, Tolerance, Verdict};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "default_db")]
    pub db: String,
    /// The fixture case `plumdeck-parity suite` builds for this scenario.
    #[serde(default)]
    pub fixture: Option<String>,
    /// SQL `suite` runs on the built fixture library before both sides
    /// start (rows no route can create yet).
    #[serde(default)]
    pub fixture_sql: Option<String>,
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
    #[serde(default)]
    pub ties: Vec<Ties>,
    /// JSONPaths of arrays compared as multisets.
    #[serde(default)]
    pub unordered: Vec<String>,
}

/// Values substituted into a scenario's requests.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    /// `{media}`: the synthetic media directory.
    pub media: String,
}

impl Vars {
    fn apply(&self, text: &str) -> String {
        if self.media.is_empty() {
            text.to_owned()
        } else {
            text.replace("{media}", &self.media)
        }
    }

    /// [`Self::apply`] to every string in a JSON body.
    fn apply_json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.apply(text)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.apply_json(item)).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), self.apply_json(v))).collect()),
            other => other.clone(),
        }
    }
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
    /// N14: headers to compare on this step (CORS).
    #[serde(default)]
    pub compare_headers: Vec<String>,
    /// N6: arrays of this step's answer compared as multisets (added to
    /// the scenario's `unordered`).
    #[serde(default)]
    pub unordered: Vec<String>,
    /// N6 tie rules for this step only (added to the scenario's `ties`).
    #[serde(default)]
    pub ties: Vec<Ties>,
    #[serde(default)]
    pub deviation: Option<StepDeviation>,
    #[serde(default)]
    pub http: Option<HttpStep>,
    #[serde(default)]
    pub mcp: Option<Value>,
    #[serde(default)]
    pub ws: Option<Value>,
    #[serde(default)]
    pub wait: Option<Value>,
}

/// An intentional difference this step demonstrates.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDeviation {
    pub id: String,
    pub b_status: u16,
    #[serde(default)]
    pub b_body: Option<Value>,
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
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub raw_hex: Option<String>,
}

/// contracts/parity/deviations.yaml.
#[derive(Debug, Default, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub deviations: Vec<LedgerEntry>,
}

#[derive(Debug, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub status: String,
}

impl Ledger {
    fn status(&self, id: &str) -> Option<&str> {
        self.deviations.iter().find(|entry| entry.id == id).map(|entry| entry.status.as_str())
    }
}

/// `deviations.yaml` next to `scenarios/`, if present.
pub fn default_ledger_path(scenario: &Path) -> Option<PathBuf> {
    let candidate = scenario.parent()?.parent()?.join("deviations.yaml");
    candidate.is_file().then_some(candidate)
}

pub fn load_ledger(path: Option<&Path>) -> Result<Ledger> {
    let Some(path) = path else { return Ok(Ledger::default()) };
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let ledger: Ledger = serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    for entry in &ledger.deviations {
        if !matches!(entry.status.as_str(), "proposed" | "approved") {
            bail!("{}: {} has status {:?} (proposed | approved)", path.display(), entry.id, entry.status);
        }
    }
    Ok(ledger)
}

/// A server under test: its base URL and port.
#[derive(Clone, Debug)]
pub struct Target {
    pub base: String,
    pub port: u16,
}

impl Target {
    pub fn from_url(base: &str) -> Result<Self> {
        let url = reqwest::Url::parse(base).with_context(|| format!("parse {base}"))?;
        let port = url.port_or_known_default().with_context(|| format!("{base} has no port"))?;
        Ok(Self { base: base.to_owned(), port })
    }
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
    if !scenario.side_effects.is_empty() {
        bail!("{}: side_effects are not supported yet", path.display());
    }
    if let Some(now) = &scenario.freeze_now {
        let parsed = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
            .iter()
            .any(|format| chrono::NaiveDateTime::parse_from_str(now, format).is_ok());
        if !parsed {
            bail!("{}: freeze_now must be a naive local datetime, got {now:?}", path.display());
        }
    }
    for (index, step) in scenario.steps.iter().enumerate() {
        let kinds = [step.http.is_some(), step.mcp.is_some(), step.ws.is_some(), step.wait.is_some()];
        if kinds.iter().filter(|k| **k).count() != 1 {
            bail!("{}: step {} needs exactly one of http/mcp/ws/wait", path.display(), index + 1);
        }
        let Some(http) = &step.http else {
            bail!("{}: step {}: only http steps are supported yet", path.display(), index + 1);
        };
        let bodies = [http.body.is_some(), http.raw.is_some(), http.raw_hex.is_some()];
        if bodies.iter().filter(|b| **b).count() > 1 {
            bail!("{}: step {}: use one of body, raw, raw_hex", path.display(), index + 1);
        }
        if let Some(hex) = &http.raw_hex {
            decode_hex(hex).with_context(|| format!("{}: step {}: raw_hex", path.display(), index + 1))?;
        }
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

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() % 2 != 0 {
        bail!("odd number of hex digits");
    }
    digits
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect()
}

/// Query pairs in order; an array becomes one pair per element.
fn query_pairs(query: &BTreeMap<String, Value>, vars: &Vars) -> Vec<(String, String)> {
    let text = |v: &Value| vars.apply(&v.as_str().map_or_else(|| v.to_string(), str::to_owned));
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

async fn send(client: &reqwest::Client, target: &Target, step: &HttpStep, vars: &Vars) -> Result<(Captured, Duration)> {
    let method = reqwest::Method::from_bytes(step.method.to_ascii_uppercase().as_bytes())
        .with_context(|| format!("method {}", step.method))?;
    let url = format!("{}{}", target.base.trim_end_matches('/'), vars.apply(&step.path));
    let mut request = client.request(method, &url);
    if !step.query.is_empty() {
        request = request.query(&query_pairs(&step.query, vars));
    }
    for (name, value) in &step.headers {
        request = request.header(name, vars.apply(&value.replace("{port}", &target.port.to_string())));
    }
    if let Some(body) = &step.body {
        request = request.json(&vars.apply_json(body));
    } else if let Some(raw) = &step.raw {
        request = request.body(raw.clone());
    } else if let Some(hex) = &step.raw_hex {
        request = request.body(decode_hex(hex)?);
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
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    let body = response.bytes().await.with_context(|| format!("read body {url}"))?.to_vec();
    Ok((Captured { status, content_type, headers, body }, started.elapsed()))
}

/// One compared step for the report.
pub struct StepResult {
    pub label: String,
    /// `D-06 (proposed)` for a deviation step.
    pub deviation: Option<(String, String)>,
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

    /// Deviation steps whose B answer matched, by ledger status.
    pub fn deviations(&self, status: &str) -> usize {
        self.steps
            .iter()
            .filter(|s| s.verdict.diffs.is_empty() && s.deviation.as_ref().is_some_and(|(_, st)| st == status))
            .count()
    }

    pub fn print(&self, a: &str, b: &str) {
        println!("scenario {}  A={a}  B={b}", self.scenario);
        for step in &self.steps {
            let tag = if !step.verdict.diffs.is_empty() {
                "DIFF"
            } else if step.deviation.is_some() {
                "DEV"
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
            "summary: {} steps, {} skipped (N12), {} deviation steps (approved {}, proposed {}), {} unexplained diffs",
            self.steps.len(),
            skipped,
            self.steps.iter().filter(|s| s.deviation.is_some()).count(),
            self.deviations("approved"),
            self.deviations("proposed"),
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
                "deviation": s.deviation.as_ref().map(|(id, status)| serde_json::json!({"id": id, "status": status})),
                "diffs": s.verdict.diffs,
                "notes": s.verdict.notes,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Send every step to A then B, in order, and compare.
pub async fn run(
    scenario: &Scenario,
    global: &NormalizeFile,
    ledger: &Ledger,
    a: &Target,
    b: &Target,
    vars: &Vars,
) -> Result<RunReport> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        // Both sides are loopback servers; never route them through a proxy.
        .no_proxy()
        .build()?;
    let mut tolerances = global.tolerances.clone();
    tolerances.extend(scenario.compare_overrides.tolerances.iter().cloned());
    let comparer = JsonComparer::new(&tolerances);
    let ties = &scenario.compare_overrides.ties;
    let unordered = &scenario.compare_overrides.unordered;
    let mut steps = Vec::new();
    for (index, step) in scenario.steps.iter().enumerate() {
        let http = step.http.as_ref().expect("validated in load");
        let deviation = match &step.deviation {
            Some(deviation) => {
                let status = ledger
                    .status(&deviation.id)
                    .with_context(|| format!("step {}: {} is not in deviations.yaml", index + 1, deviation.id))?;
                Some((deviation.id.clone(), status.to_owned()))
            }
            None => None,
        };
        let (ra, ta) = send(&client, a, http, vars).await?;
        let (rb, tb) = send(&client, b, http, vars).await?;
        let verdict = match &step.deviation {
            Some(expected) => deviation_verdict(expected, &ra, &rb),
            None => {
                let paths: Vec<String> = unordered.iter().chain(&step.unordered).cloned().collect();
                let step_ties: Vec<Ties> = ties.iter().chain(&step.ties).cloned().collect();
                let mut verdict = compare_responses_with(&ra, &rb, &comparer, &step_ties, &paths);
                if !verdict.skipped {
                    // Each side echoes its own port (CORS origins): compare as `{port}`.
                    let (ha, hb) = (ra.with_port_placeholder(a.port), rb.with_port_placeholder(b.port));
                    verdict.diffs.extend(compare_headers(&ha, &hb, &step.compare_headers));
                }
                if let Some(expected) = step.expect_status {
                    for (side, got) in [("A", ra.status), ("B", rb.status)] {
                        if got != expected {
                            verdict.diffs.push(format!("{side} status {got}, expected {expected}"));
                        }
                    }
                }
                verdict
            }
        };
        steps.push(StepResult {
            label: step.label(index),
            deviation,
            a_status: ra.status,
            b_status: rb.status,
            a_ms: ta.as_secs_f64() * 1000.0,
            b_ms: tb.as_secs_f64() * 1000.0,
            verdict,
        });
    }
    Ok(RunReport { scenario: scenario.name.clone(), steps })
}

/// A deviation step: B must answer as the step declares; A is only noted.
fn deviation_verdict(expected: &StepDeviation, a: &Captured, b: &Captured) -> Verdict {
    let mut verdict = Verdict::default();
    verdict.notes.push(format!("{}: A answered {} {}", expected.id, a.status, a.snippet()));
    if b.status != expected.b_status {
        verdict.diffs.push(format!("{}: B status {}, expected {}", expected.id, b.status, expected.b_status));
    }
    if let Some(body) = &expected.b_body {
        let got: Option<Value> = serde_json::from_slice(&b.body).ok();
        if got.as_ref() != Some(body) {
            verdict.diffs.push(format!("{}: B body {}, expected {body}", expected.id, b.snippet()));
        }
    }
    verdict
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
    fn raw_bodies_and_deviations_parse() {
        let path = write(
            "raw.yaml",
            "name: r\nfixture: catalog\nsteps:\n  - deviation: {id: D-06, b_status: 421}\n    compare_headers: [vary]\n    http: {method: POST, path: /, raw_hex: 'ff 00'}\n",
        );
        let scenario = load(&path).unwrap();
        assert_eq!(scenario.fixture.as_deref(), Some("catalog"));
        assert_eq!(scenario.steps[0].deviation.as_ref().unwrap().b_status, 421);
        assert_eq!(decode_hex("ff 00").unwrap(), [0xff, 0]);
        let path = write("both.yaml", "name: b\nsteps:\n  - http: {method: POST, path: /, raw: x, body: 1}\n");
        assert!(load(&path).is_err());
        let ledger: Ledger = serde_yaml::from_str("deviations:\n  - {id: D-06, status: proposed, summary: x}\n").unwrap();
        assert_eq!(ledger.status("D-06"), Some("proposed"));
        assert_eq!(ledger.status("D-99"), None);
    }

    #[test]
    fn query_arrays_repeat_the_key() {
        let path = write(
            "query.yaml",
            "name: q\nsteps:\n  - http: {method: GET, path: /, query: {ids: [1, 2], q: x, on: true}}\n",
        );
        let scenario = load(&path).unwrap();
        let pairs = query_pairs(&scenario.steps[0].http.as_ref().unwrap().query, &Vars::default());
        let expected = [("ids", "1"), ("ids", "2"), ("on", "true"), ("q", "x")];
        assert_eq!(pairs, expected.map(|(k, v)| (k.to_owned(), v.to_owned())));
        let path = write("bad.yaml", "name: b\nsteps:\n  - http: {method: GET, path: /, query: {f: {a: 1}}}\n");
        assert!(load(&path).is_err());
    }
}
