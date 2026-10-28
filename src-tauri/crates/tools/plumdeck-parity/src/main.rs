//! Differential testing between the Python backend (side A, the oracle) and
//! the Rust backend (side B). Not shipped.
//!
//! - `plumdeck-parity run --a URL --b URL --scenario FILE` sends each step to
//!   both sides in order, normalizes (N1, N2, N7, N12) and prints a report.
//! - `plumdeck-parity db-diff A.duckdb B.duckdb` compares two databases.
//!
//! Exit status: 0 when nothing unexplained differs, 1 on differences, 2 when
//! the comparison itself could not run.

mod compare;
mod dbdiff;
mod scenario;

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
