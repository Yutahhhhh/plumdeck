//! `plumdeck-parity startup`: start the Python oracle (A) and the Rust
//! backend (B) on separate copies of each fixture library, stop both
//! gracefully (SIGINT: CHECKPOINT and close) and compare what startup and
//! shutdown left behind (plan §4.4, S1):
//!
//! - the `PLUMDECK_STARTUP:` progress lines, in order, and whether the server
//!   came up;
//! - the files in USER_DATA_DIR (restore markers, WAL files, the queue);
//! - `db-diff` of the two libraries (schema, rows, sequences);
//! - the analysis-job queue's schema and rows.
//!
//! The `newer-schema` case runs B only and checks deviation D-14: startup
//! fails with status 3 and the file is left byte-identical.
//!
//! Unix only (stops the servers with SIGINT through kill(1)).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use plumdeck_db::rusqlite;

use crate::dbdiff;
use crate::fixture::{self, Case, DB_NAME};

/// What starts each side.
pub struct Programs {
    pub python: PathBuf,
    pub oracle: PathBuf,
    pub backend: PathBuf,
}

/// Env names either backend reads; cleared from the inherited environment
/// (any letter case) so a developer's settings never leak into a side.
const CLEARED: [&str; 16] = [
    "DB_PATH",
    "USER_DATA_DIR",
    "PLUMDECK_LOG_DIR",
    "PLUMDECK_REKORDBOX_DB",
    "PLUMDECK_PORT",
    "PLUMDECK_MANAGED_SIDECAR",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "ENV",
    "APP_NAME",
    "APP_AUTHOR",
    "MUSIC_DIR",
    "FRONTEND_PORT",
    "NUM_WORKERS",
    "NUMBA_CACHE_DIR",
    "MPLCONFIGDIR",
];

const READY_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_TIMEOUT: Duration = Duration::from_secs(60);

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// `GET /` answered 200.
fn healthy(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.starts_with("HTTP/1.1 200")
}

struct Side {
    label: &'static str,
    root: PathBuf,
    port: u16,
    child: Child,
}

impl Side {
    fn ud(&self) -> PathBuf {
        self.root.join("ud")
    }
}

fn spawn(label: &'static str, root: &Path, programs: &Programs) -> Result<Side> {
    let port = free_port()?;
    let mut command = if label == "A" {
        let mut command = Command::new(&programs.python);
        command.arg(&programs.oracle).arg("--port").arg(port.to_string());
        command
    } else {
        let mut command = Command::new(&programs.backend);
        command.arg("--parity").arg("--port").arg(port.to_string());
        command
    };
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if CLEARED.contains(&upper.as_str()) {
            command.env_remove(&name);
        }
    }
    let ud = root.join("ud");
    command
        .env("DB_PATH", ud.join(DB_NAME))
        .env("USER_DATA_DIR", &ud)
        .env("PLUMDECK_LOG_DIR", root.join("logs"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        // A missing file: no rekordbox library on either side.
        .env("PLUMDECK_REKORDBOX_DB", root.join("rb").join("master.db"))
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(root.join("stdout.txt"))?)
        .stderr(std::fs::File::create(root.join("stderr.txt"))?);
    let child = command.spawn().with_context(|| format!("start side {label}"))?;
    Ok(Side { label, root: root.to_owned(), port, child })
}

/// Wait until the side answers or exits; true when it answered.
fn wait_ready(side: &mut Side) -> Result<bool> {
    let started = Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if side.child.try_wait()?.is_some() {
            return Ok(false);
        }
        if healthy(side.port) {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("side {} did not answer within {READY_TIMEOUT:?}", side.label)
}

fn stop(side: &mut Side) -> Result<ExitStatus> {
    if let Some(status) = side.child.try_wait()? {
        return Ok(status);
    }
    Command::new("kill").arg("-INT").arg(side.child.id().to_string()).status()?;
    let started = Instant::now();
    while started.elapsed() < STOP_TIMEOUT {
        if let Some(status) = side.child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    side.child.kill()?;
    bail!("side {} did not stop within {STOP_TIMEOUT:?} after SIGINT", side.label)
}

/// The lines startup prints for the desktop and the failure report, with
/// the side's own directory replaced by `<side>`.
fn startup_lines(root: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(root.join("stdout.txt"))?;
    let root = root.to_string_lossy();
    Ok(text
        .lines()
        .filter(|line| line.starts_with("PLUMDECK_STARTUP:") || line.starts_with("Error during database initialization"))
        .map(|line| line.replace(root.as_ref(), "<side>"))
        .collect())
}

fn file_names(dir: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            names.insert(entry?.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

/// Schema and rows (rowid order) of the analysis-job queue.
fn queue_dump(path: &Path) -> Result<Option<Vec<String>>> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut out = Vec::new();
    let mut tables = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master ORDER BY type, name")?;
        for row in stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))? {
            let (name, sql) = row?;
            out.push(format!("schema {name}: {}", sql.as_deref().unwrap_or("")));
            if sql.as_deref().is_some_and(|sql| sql.starts_with("CREATE TABLE")) {
                tables.push(name);
            }
        }
    }
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))?;
        let width = stmt.column_count();
        let rows = stmt.query_map([], |row| {
            (0..width).map(|i| row.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}"))).collect::<Result<Vec<_>, _>>()
        })?;
        for row in rows {
            out.push(format!("{table}: {}", row?.join(", ")));
        }
    }
    Ok(Some(out))
}

/// One compared case.
pub struct CaseReport {
    pub name: &'static str,
    pub diffs: Vec<String>,
    pub notes: Vec<String>,
}

fn compare_case(case: Case, a: &Side, b: &Side, ready: (bool, bool)) -> Result<CaseReport> {
    let mut report = CaseReport { name: case.name(), diffs: Vec::new(), notes: Vec::new() };
    if ready.0 != ready.1 {
        report.diffs.push(format!("server came up: A {} B {}", ready.0, ready.1));
    }
    let (la, lb) = (startup_lines(&a.root)?, startup_lines(&b.root)?);
    if la != lb {
        report.diffs.push(format!("startup lines:\n    A {la:#?}\n    B {lb:#?}"));
    }
    report.notes.push(format!("{} startup lines", la.len()));
    let (fa, fb) = (file_names(&a.ud())?, file_names(&b.ud())?);
    if fa != fb {
        report.diffs.push(format!("USER_DATA_DIR files: A {fa:?} B {fb:?}"));
    }
    let (da, db) = (a.ud().join(DB_NAME), b.ud().join(DB_NAME));
    if da.exists() && db.exists() {
        match dbdiff::diff(&da, &db, 20) {
            Ok(result) if result.differences != 0 => {
                report.diffs.push(format!("db-diff:\n    {}", result.lines.join("\n    ")));
            }
            Ok(result) => report.notes.push(result.lines.last().cloned().unwrap_or_default()),
            // Not a database (a failed startup left it as it was): compare bytes.
            Err(error) if std::fs::read(&da)? == std::fs::read(&db)? => {
                report.notes.push(format!("libraries are byte-identical ({error:#})"));
            }
            Err(error) => report.diffs.push(format!("libraries differ and cannot be attached: {error:#}")),
        }
    }
    let queue = |side: &Side| plumdeck_db::restore_recovery::queue_path(&side.ud().join(DB_NAME));
    let (qa, qb) = (queue_dump(&queue(a))?, queue_dump(&queue(b))?);
    if qa != qb {
        report.diffs.push(format!("analysis-job queue:\n    A {qa:#?}\n    B {qb:#?}"));
    } else if let Some(rows) = qa {
        report.notes.push(format!("queue: {} schema/row lines equal", rows.len()));
    }
    Ok(report)
}

/// D-14: B refuses a newer library and leaves it untouched.
fn newer_schema_case(work: &Path, master: &Path, programs: &Programs) -> Result<CaseReport> {
    let mut report = CaseReport { name: Case::NewerSchema.name(), diffs: Vec::new(), notes: Vec::new() };
    let root = work.join("b");
    fixture::copy_dir(master, &root.join("ud"))?;
    let before = std::fs::read(root.join("ud").join(DB_NAME))?;
    let mut side = spawn("B", &root, programs)?;
    let ready = wait_ready(&mut side)?;
    let status = stop(&mut side)?;
    if ready || status.code() != Some(3) {
        report.diffs.push(format!("B should refuse to start with status 3; ready {ready}, {status}"));
    }
    let lines = startup_lines(&root)?;
    let refused = lines.iter().any(|line| line.contains("このライブラリは新しいバージョンの plumdeck"));
    if !refused {
        report.diffs.push(format!("no D-14 message in {lines:#?}"));
    }
    if std::fs::read(root.join("ud").join(DB_NAME))? != before {
        report.diffs.push("the refused library was modified".into());
    }
    report.notes.push("deviation D-14 (proposed): A is not run; Python opens a newer library".into());
    Ok(report)
}

pub fn run(programs: &Programs, work: &Path, cases: &[Case]) -> Result<Vec<CaseReport>> {
    let mut reports = Vec::new();
    for &case in cases {
        let dir = work.join(case.name());
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clear {}", dir.display()))?;
        }
        let master = dir.join("master");
        fixture::build(case, &master).with_context(|| format!("build fixture {}", case.name()))?;
        if case == Case::NewerSchema {
            reports.push(newer_schema_case(&dir, &master, programs)?);
            continue;
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
        let stopped = (stop(&mut a), stop(&mut b));
        let ready = (ready.0?, ready.1?);
        let (sa, sb) = (stopped.0?, stopped.1?);
        let mut report = compare_case(case, &a, &b, ready)?;
        if sa.success() != sb.success() {
            report.diffs.push(format!("exit: A {sa} B {sb}"));
        }
        reports.push(report);
    }
    Ok(reports)
}

pub fn print(reports: &[CaseReport]) -> usize {
    let mut total = 0;
    for report in reports {
        let tag = if report.diffs.is_empty() { "ok" } else { "DIFF" };
        println!("  [{tag:>4}] startup/{}", report.name);
        for note in &report.notes {
            println!("         note: {note}");
        }
        for diff in &report.diffs {
            println!("         diff: {diff}");
        }
        total += report.diffs.len();
    }
    println!("summary: {} cases, {total} unexplained diffs", reports.len());
    total
}
