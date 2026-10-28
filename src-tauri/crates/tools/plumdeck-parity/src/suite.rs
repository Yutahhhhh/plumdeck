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

/// The instant fixture timestamps and media mtimes are pinned to.
const PINNED: &str = "2026-01-01 00:00:00";

/// Pin what building the fixture stamped with the clock: media file mtimes
/// and timestamp columns the builder left to their `CURRENT_TIMESTAMP`
/// default. Both sides start from the same copy either way; pinned, a
/// golden recording replays the same on any day.
fn pin_times(db: &Path, media: &Path, built_at: chrono::NaiveDateTime) -> Result<()> {
    let instant = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600);
    let mut stack = vec![media.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                std::fs::File::options().write(true).open(&path)?.set_modified(instant)?;
            }
        }
    }
    // A fresh fixture has no library; opening one would create it.
    if !db.is_file() {
        return Ok(());
    }
    let Ok(conn) = duckdb::Connection::open(db) else { return Ok(()) };
    let columns: Vec<(String, String)> = match conn.prepare(
        "SELECT table_name, column_name FROM duckdb_columns() WHERE schema_name = 'main' AND data_type LIKE 'TIMESTAMP%'
           AND (lower(column_default) LIKE '%current_timestamp%' OR lower(column_default) LIKE '%now()%')", // allow-engine-clock: finds the defaults
    ) {
        Ok(mut statement) => statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?,
        Err(_) => return Ok(()),
    };
    let since = built_at.format("%Y-%m-%d %H:%M:%S").to_string();
    for (table, column) in columns {
        conn.execute(
            &format!("UPDATE \"{table}\" SET \"{column}\" = TIMESTAMP '{PINNED}' WHERE \"{column}\" >= CAST(? AS TIMESTAMP)"),
            [&since],
        )?;
    }
    conn.execute_batch("CHECKPOINT")?;
    Ok(())
}

/// The work directory of golden recordings and replays.
pub const GOLDEN_WORK: &str = "/tmp/plumdeck-parity-golden";

/// Where side A comes from (see [`crate::golden`]).
pub enum Golden {
    /// The live Python oracle.
    Off,
    /// The live oracle, recorded into this directory.
    Record(PathBuf),
    /// A recording in this directory; no Python runs.
    Replay(PathBuf),
}

fn run_one(
    programs: &Programs,
    work: &Path,
    path: &Path,
    global: &NormalizeFile,
    ledger: &Ledger,
    golden: &Golden,
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
    // Both sides read the same synthetic media (never written by a route
    // ported so far).
    let media = dir.join("media");
    crate::media::write(&media)?;
    let built_at = chrono::Local::now().naive_local() - chrono::Duration::days(1);
    fixture::build(case, &master, &media).with_context(|| format!("build fixture {}", case.name()))?;
    if let Some(sql) = &loaded.fixture_sql {
        let conn = duckdb::Connection::open(master.join(DB_NAME))?;
        conn.execute_batch(&fixture::with_media(sql, &media)).context("fixture_sql")?;
        conn.execute_batch("CHECKPOINT")?;
    }
    pin_times(&master.join(DB_NAME), &media, built_at)?;
    let vars = scenario::Vars { media: media.to_string_lossy().into_owned() };
    for side in ["a", "b"] {
        fixture::copy_dir(&master, &dir.join(side).join("ud"))?;
    }
    let work_text = dir.to_string_lossy().into_owned();
    let recording = match golden {
        Golden::Replay(golden_dir) => Some(crate::golden::load(golden_dir, &loaded.name)?),
        _ => None,
    };
    let mut a = match recording {
        Some(_) => None,
        None => Some(spawn("A", &dir.join("a"), programs, loaded.freeze_now.as_deref())?),
    };
    let mut b = match spawn("B", &dir.join("b"), programs, loaded.freeze_now.as_deref()) {
        Ok(b) => b,
        Err(error) => {
            if let Some(a) = a.as_mut() {
                let _ = stop(a);
            }
            return Err(error);
        }
    };
    let ready_a = a.as_mut().map_or(Ok(true), wait_ready);
    let ready = (ready_a, wait_ready(&mut b));
    // A recorded side A answers on no port; any unused one stands in.
    let a_target = match &a {
        Some(a) => target(a),
        None => {
            let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
            Target { base: format!("http://127.0.0.1:{port}"), port }
        }
    };
    match (&recording, golden) {
        (Some(recording), _) => crate::golden::start_replay(&work_text, a_target.port, recording.answers.clone()),
        (None, Golden::Record(_)) => crate::golden::start_recording(&work_text, a_target.port),
        _ => {}
    }
    let outcome = match ready {
        (Ok(true), Ok(true)) => {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(scenario::run(&loaded, global, ledger, &a_target, &target(&b), &vars))
        }
        other => Err(anyhow::anyhow!("a side did not come up: A {:?} B {:?}", other.0, other.1)),
    };
    let answers = crate::golden::finish();
    let stopped_a = match a.as_mut() {
        Some(a) => Some(stop(a)),
        None => None,
    };
    let stopped_b = stop(&mut b);
    let run = outcome?;
    let sb = stopped_b?;
    let mut db_diffs = Vec::new();
    let a_db = dir.join("a").join("ud").join(DB_NAME);
    match stopped_a {
        Some(sa) => {
            let sa = sa?;
            if !sa.success() || !sb.success() {
                db_diffs.push(format!("exit: A {sa} B {sb}"));
            }
            if let Golden::Record(golden_dir) = golden {
                let library = crate::golden::dump(&a_db, &work_text, &dir.join("golden-export"))?;
                crate::golden::save(golden_dir, &crate::golden::Recording { scenario: loaded.name.clone(), answers, library })?;
            }
        }
        None => {
            if !sb.success() {
                db_diffs.push(format!("exit: B {sb}"));
            }
            let recording = recording.as_ref().expect("replaying");
            crate::golden::restore(&recording.library, &a_db, &work_text)?;
        }
    }
    let mut ignored = Vec::new();
    let mut ignored_note = Vec::new();
    for group in &loaded.db_deviations {
        let status = ledger.status(&group.id).with_context(|| format!("db_deviations: {} is not in deviations.yaml", group.id))?;
        for column in &group.columns {
            let (table, name) = column.split_once('.').with_context(|| format!("db_deviations: {column} is not table.column"))?;
            ignored.push((table.to_owned(), name.to_owned()));
        }
        ignored_note.push(format!("{} ({status}): {}", group.id, group.columns.join(", ")));
    }
    let result = dbdiff::diff_ignoring(&a_db, &b.ud().join(DB_NAME), 20, &ignored)?;
    let mut db_note = result.lines.last().cloned().unwrap_or_default();
    if !ignored_note.is_empty() {
        db_note = format!("{db_note}; not compared: {}", ignored_note.join("; "));
    }
    if result.differences != 0 {
        db_diffs.push(format!("db-diff:\n    {}", result.lines.join("\n    ")));
    }
    Ok(SuiteReport { run, db_diffs, db_note })
}

/// Run every scenario; returns the reports in order.
pub fn run(programs: &Programs, work: &Path, scenarios: &[PathBuf], report_dir: Option<&Path>, golden: &Golden) -> Result<Vec<SuiteReport>> {
    let mut reports = Vec::new();
    for path in scenarios {
        let global = scenario::load_normalize(scenario::default_normalize_path(path).as_deref())?;
        let ledger = scenario::load_ledger(scenario::default_ledger_path(path).as_deref())?;
        let report = run_one(programs, work, path, &global, &ledger, golden).with_context(|| format!("scenario {}", path.display()))?;
        report.run.print(if matches!(golden, Golden::Replay(_)) { "golden" } else { "python" }, "rust");
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
