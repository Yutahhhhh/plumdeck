//! Differential testing between the Python backend (side A, the oracle) and
//! the Rust backend (side B). Not shipped.
//!
//! - `plumdeck-parity run --a URL --b URL --scenario FILE` sends each step to
//!   both sides in order, normalizes (N1, N2, N7, N12) and prints a report.
//! - `plumdeck-parity db-diff A.duckdb B.duckdb` compares two databases.
//! - `plumdeck-parity startup --work DIR` starts both backends on copies of
//!   each fixture library, stops them, and compares the files they leave.
//!
//! Exit status: 0 when nothing unexplained differs, 1 on differences, 2 when
//! the comparison itself could not run.

mod compare;
mod dbdiff;
mod fixture;
mod scenario;
mod startup;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "plumdeck-parity", about = "Compare the Python and Rust plumdeck backends")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a scenario against both servers and compare the responses.
    Run {
        /// Side A base URL (the Python oracle), e.g. http://127.0.0.1:18001.
        #[arg(long)]
        a: String,
        /// Side B base URL (the Rust backend), e.g. http://127.0.0.1:18002.
        #[arg(long)]
        b: String,
        /// Scenario YAML file.
        #[arg(long)]
        scenario: PathBuf,
        /// Global normalization rules (default: ../normalize.yaml next to the scenarios dir).
        #[arg(long)]
        normalize: Option<PathBuf>,
        /// Also write the report as JSON to this file.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Compare two DuckDB files table by table in primary-key order.
    DbDiff {
        a: PathBuf,
        b: PathBuf,
        /// Differing rows to print per table.
        #[arg(long, default_value_t = 20)]
        max_rows: usize,
    },
    /// Start A and B on copies of each fixture library, stop them with
    /// SIGINT and compare startup output, files, databases and job queues.
    Startup {
        /// Scratch directory; each case gets <work>/<case>/{master,a,b}.
        #[arg(long)]
        work: PathBuf,
        /// Python with the backend's dependencies (the oracle's interpreter).
        #[arg(long, env = "PLUMDECK_ORACLE_PYTHON", default_value = "python3")]
        python: PathBuf,
        /// tools/parity/oracle_server.py.
        #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../tools/parity/oracle_server.py"))]
        oracle: PathBuf,
        /// The Rust backend (default: plumdeck-backend next to this binary).
        #[arg(long)]
        backend: Option<PathBuf>,
        /// Run only these cases (default: all).
        #[arg(long = "case", value_parser = parse_case)]
        cases: Vec<fixture::Case>,
    },
}

fn parse_case(name: &str) -> Result<fixture::Case, String> {
    fixture::Case::ALL.into_iter().find(|case| case.name() == name).ok_or_else(|| {
        let names: Vec<&str> = fixture::Case::ALL.iter().map(|case| case.name()).collect();
        format!("unknown case {name:?}; one of {}", names.join(", "))
    })
}

fn run_command(command: Command) -> Result<bool> {
    match command {
        Command::Run { a, b, scenario, normalize, report } => {
            let loaded = scenario::load(&scenario)?;
            let normalize = normalize.or_else(|| scenario::default_normalize_path(&scenario));
            let global = scenario::load_normalize(normalize.as_deref())?;
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let result = runtime.block_on(scenario::run(&loaded, &global, &a, &b))?;
            result.print(&a, &b);
            if let Some(path) = report {
                let text = serde_json::to_string_pretty(&result.to_json())?;
                std::fs::write(&path, text + "\n").with_context(|| format!("write {}", path.display()))?;
            }
            Ok(result.unexplained() == 0)
        }
        Command::DbDiff { a, b, max_rows } => {
            let result = dbdiff::diff(&a, &b, max_rows)?;
            for line in &result.lines {
                println!("{line}");
            }
            Ok(result.differences == 0)
        }
        Command::Startup { work, python, oracle, backend, cases } => {
            let backend = match backend {
                Some(path) => path,
                None => std::env::current_exe()?.with_file_name(format!("plumdeck-backend{}", std::env::consts::EXE_SUFFIX)),
            };
            let programs = startup::Programs { python, oracle, backend };
            let cases = if cases.is_empty() { fixture::Case::ALL.to_vec() } else { cases };
            std::fs::create_dir_all(&work)?;
            let work = work.canonicalize()?;
            let reports = startup::run(&programs, &work, &cases)?;
            Ok(startup::print(&reports) == 0)
        }
    }
}

fn main() -> ExitCode {
    match run_command(Cli::parse().command) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("plumdeck-parity: {error:#}");
            ExitCode::from(2)
        }
    }
}
