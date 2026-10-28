//! DuckDB access for the plumdeck backend.
//!
//! The engine is pinned to DuckDB 1.5.5 (`duckdb =1.10505.0`, bundled), the
//! version the Python backend writes the existing library files with; storage
//! and WAL files are version dependent. S0 provides opening a file with the
//! Python backend's connection settings, the version check and the v5 schema
//! DDL (`contracts/schema/v5.sql`, dumped from the Python backend).

pub mod array_codec;

use std::path::Path;

pub use duckdb;
use duckdb::{AccessMode, Config, Connection};

/// `SELECT version()` of the engine this crate links.
pub const EXPECTED_DUCKDB_VERSION: &str = "v1.5.5";

/// Schema version the DDL below creates (`schema_info.version`).
pub const SCHEMA_VERSION: i32 = 5;

/// DDL of a fresh v5 database, as the Python backend's `init_db()` leaves it.
pub const SCHEMA_V5_SQL: &str = include_str!("../../../../contracts/schema/v5.sql");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    DuckDb(#[from] duckdb::Error),
    #[error("DuckDB {found} is linked, expected {expected}")]
    Version { found: String, expected: &'static str },
}

/// The Python engine's connect args: `worker_threads=4`, `READ_WRITE`.
fn library_config() -> Result<Config, DbError> {
    Ok(Config::default().access_mode(AccessMode::ReadWrite)?.threads(4)?)
}

/// Open (or create) a library database file read-write.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    Ok(Connection::open_with_flags(path, library_config()?)?)
}

/// Open a file read-only (inspection and diffs; never takes the write lock).
pub fn open_read_only(path: &Path) -> Result<Connection, DbError> {
    Ok(Connection::open_with_flags(path, Config::default().access_mode(AccessMode::ReadOnly)?)?)
}

/// `SELECT version()`, e.g. `v1.5.5`.
pub fn engine_version(conn: &Connection) -> Result<String, DbError> {
    Ok(conn.query_row("SELECT version()", [], |row| row.get(0))?)
}

/// Fails unless the linked engine is the pinned DuckDB release.
pub fn ensure_engine_version(conn: &Connection) -> Result<(), DbError> {
    let found = engine_version(conn)?;
    if found != EXPECTED_DUCKDB_VERSION {
        return Err(DbError::Version { found, expected: EXPECTED_DUCKDB_VERSION });
    }
    Ok(())
}

/// Create every v5 sequence and table on an empty database.
pub fn create_schema_v5(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(SCHEMA_V5_SQL)?;
    Ok(())
}
