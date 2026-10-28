//! `plumdeck-parity suite`: for each scenario, build its fixture library
//! (`fixture:`, default `populated`), start the Python oracle (A) and the
//! Rust backend (B) on separate copies, run the steps, stop both
//! gracefully, and `db-diff` the libraries they leave (plan §4.4: every
//! scenario ends with a database comparison, so a write that answers alike
//! but stores differently is caught).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::dbdiff;
use crate::fixture::{self, Case, DB_NAME};
use crate::scenario::{self, Ledger, NormalizeFile, RunReport, Target};
use crate::startup::{spawn, stop, wait_ready, Programs, Side};

/// One scenario's outcome.
pub struct SuiteReport {
    pub run: RunReport,
    /// Database differences after both sides stopped.
    pub db_diffs: Vec<String>,
    pub db_note: String,
}

impl SuiteReport {
    pub fn unexplained(&self) -> usize {
        self.run.unexplained() + self.db_diffs.len()
    }
}

fn case_named(name: Option<&str>) -> Result<Case> {
    let name = name.unwrap_or("populated");
    Case::ALL.into_iter().find(|case| case.name() == name).with_context(|| format!("unknown fixture case {name:?}"))
}

fn target(side: &Side) -> Target {
    Target { base: format!("http://127.0.0.1:{}", side.port), port: side.port }
}

fn run_one(
    programs: &Programs,
    work: &Path,
    path: &Path,
    global: &NormalizeFile,
    ledger: &Ledger,
) -> Result<SuiteReport> {
    let loaded = scenario::load(path)?;
    let case = case_named(loaded.fixture.as_deref())?;
    if matches!(case, Case::Unreadable | Case::NewerSchema) {
        bail!("{}: fixture {} does not start", path.display(), case.name());
    }
    let dir = work.join(&loaded.name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("clear {}", dir.display()))?;
    }
    let master = dir.join("master");
    fixture::build(case, &master).with_context(|| format!("build fixture {}", case.name()))?;
    if let Some(sql) = &loaded.fixture_sql {
        let conn = duckdb::Connection::open(master.join(DB_NAME))?;
        conn.execute_batch(sql).context("fixture_sql")?;
        conn.execute_batch("CHECKPOINT")?;
    }
    for side in ["a", "b"] {
        fixture::copy_dir(&master, &dir.join(side).join("ud"))?;
    }
    let mut a = spawn("A", &dir.join("a"), programs)?;
    let mut b = match spawn("B", &dir.join("b"), programs) {
        Ok(b) => b,
        Err(error) => {
            let _ = stop(&mut a);
            return Err(error);
        }
    };
    let ready = (wait_ready(&mut a), wait_ready(&mut b));
    let outcome = match ready {
        (Ok(true), Ok(true)) => {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(scenario::run(&loaded, global, ledger, &target(&a), &target(&b)))
        }
        other => Err(anyhow::anyhow!("a side did not come up: A {:?} B {:?}", other.0, other.1)),
    };
    let stopped = (stop(&mut a), stop(&mut b));
    let run = outcome?;
    let (sa, sb) = (stopped.0?, stopped.1?);
    let mut db_diffs = Vec::new();
    if !sa.success() || !sb.success() {
        db_diffs.push(format!("exit: A {sa} B {sb}"));
    }
    let result = dbdiff::diff(&a.ud().join(DB_NAME), &b.ud().join(DB_NAME), 20)?;
    let db_note = result.lines.last().cloned().unwrap_or_default();
    if result.differences != 0 {
        db_diffs.push(format!("db-diff:\n    {}", result.lines.join("\n    ")));
    }
    Ok(SuiteReport { run, db_diffs, db_note })
}

/// Run every scenario; returns the reports in order.
pub fn run(programs: &Programs, work: &Path, scenarios: &[PathBuf], report_dir: Option<&Path>) -> Result<Vec<SuiteReport>> {
    let mut reports = Vec::new();
    for path in scenarios {
        let global = scenario::load_normalize(scenario::default_normalize_path(path).as_deref())?;
        let ledger = scenario::load_ledger(scenario::default_ledger_path(path).as_deref())?;
        let report = run_one(programs, work, path, &global, &ledger).with_context(|| format!("scenario {}", path.display()))?;
        report.run.print("python", "rust");
        println!("  db: {}", report.db_note);
        for diff in &report.db_diffs {
            println!("  DIFF {diff}");
        }
        if let Some(dir) = report_dir {
            std::fs::create_dir_all(dir)?;
            let mut json = report.run.to_json();
            json["db_diffs"] = serde_json::json!(report.db_diffs);
            let file = dir.join(format!("{}.json", report.run.scenario));
            std::fs::write(&file, serde_json::to_string_pretty(&json)? + "\n")?;
        }
        reports.push(report);
    }
    let total: usize = reports.iter().map(SuiteReport::unexplained).sum();
    let steps: usize = reports.iter().map(|r| r.run.steps.len()).sum();
    let proposed: usize = reports.iter().map(|r| r.run.deviations("proposed")).sum();
    let approved: usize = reports.iter().map(|r| r.run.deviations("approved")).sum();
    println!(
        "suite: {} scenarios, {steps} steps, deviation steps approved {approved} / proposed {proposed}, {total} unexplained diffs",
        reports.len()
    );
    Ok(reports)
}
