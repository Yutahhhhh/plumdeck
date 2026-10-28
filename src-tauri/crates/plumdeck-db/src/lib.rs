//! DuckDB access for the plumdeck backend.
//!
//! The engine is pinned to DuckDB 1.5.5 (`duckdb =1.10505.0`, bundled), the
//! version the Python backend writes the existing library files with; storage
//! and WAL files are version dependent.
//!
//! - [`handle`]: the pooled, swappable library handle and its gates.
//! - [`schema`]: `init_raw_db` (v5 DDL, compatibility ALTERs, migrations) and
//!   the guard against libraries from a newer app.
//! - [`restore_recovery`]: crash recovery of the two-file restore unit.
//! - [`jobs_sqlite`]: the SQLite analysis-job queue beside the library.
//! - [`array_codec`]: the BLOB layout of waveforms and beat positions.
//! - [`compaction`]: the startup health check and its allowlist marker.
//! - [`repos`]: repositories (S1: what startup touches).
//!
//! Startup compaction (compaction.py) is ported up to its rebuild decision
//! ([`compaction`]): the `features_extra_clean` marker; the rebuilds are not.
//!
//! SQL in this crate never calls `now()`/`CURRENT_TIMESTAMP`: the bundled
//! engine has no ICU, so those are UTC, while stored timestamps are local
//! naive time. Bind `plumdeck_core::time::Clock` values instead (plan R3).

pub mod array_codec;
pub mod compaction;
pub mod handle;
pub mod jobs_sqlite;
pub mod repos;
pub mod restore_recovery;
pub mod schema;

use std::collections::BTreeSet;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use rusqlite;

pub use duckdb;
use duckdb::{AccessMode, Config, Connection};

/// `SELECT version()` of the engine this crate links.
pub const EXPECTED_DUCKDB_VERSION: &str = "v1.5.5";

/// Schema version the DDL below creates (`schema_info.version`).
pub const SCHEMA_VERSION: i64 = 5;

/// DDL of a fresh v5 database, as the Python backend's `init_db()` leaves it.
pub const SCHEMA_V5_SQL: &str = include_str!("../../../../contracts/schema/v5.sql");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("データベースを使用中です。処理の完了後に再試行してください")]
    DatabaseInUse,
    #[error("解析・取り込みを停止してからバックアップ／復元してください")]
    BackgroundActivity,
    #[error("バックアップ／復元中です。完了後に再試行してください")]
    Maintenance,
    #[error("database connection pool timed out")]
    PoolTimeout,
    #[error("database is closed")]
    Closed,
    #[error("close the database before reopening it")]
    AlreadyOpen,
    #[error("復元の回復記録が不正です")]
    InvalidRestoreState,
    #[error("復元前の退避データが見つかりません")]
    MissingRollback,
    /// The library was written by a newer plumdeck (deviation D-14: Python
    /// opened it and ran its own DDL against it).
    #[error("このライブラリは新しいバージョンの plumdeck で更新されています（スキーマ v{found}、このバージョンは v{supported} まで）。plumdeck を最新版に更新してください")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("{0} is already open in this process")]
    FileAlreadyOpen(PathBuf),
    #[error("Analysis job not found")]
    JobNotFound,
    /// A ValueError-style message shown to the user as is.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
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

static OPEN_FILES: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// The absolute, symlink-resolved name of a database file that may not exist
/// yet: resolves the parent directory and keeps the file name.
fn claim_key(path: &Path) -> Result<PathBuf, DbError> {
    if let Ok(resolved) = path.canonicalize() {
        return Ok(resolved);
    }
    let absolute = std::path::absolute(path)?;
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => Ok(parent.canonicalize().unwrap_or_else(|_| parent.to_owned()).join(name)),
        _ => Ok(absolute),
    }
}

/// A process-wide claim on one database file.
///
/// Two DuckDB instances on one file inside one process do not see each
/// other's POSIX lock and can corrupt it (duckdb-rs has no instance cache,
/// unlike Python's `duckdb.connect`). Everything that opens the library holds
/// a claim; a second claim on the same file fails.
#[derive(Debug)]
pub struct FileClaim(PathBuf);

impl FileClaim {
    pub fn acquire(path: &Path) -> Result<Self, DbError> {
        let key = claim_key(path)?;
        let mut open = OPEN_FILES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !open.insert(key.clone()) {
            return Err(DbError::FileAlreadyOpen(key));
        }
        Ok(Self(key))
    }
}

impl Drop for FileClaim {
    fn drop(&mut self) {
        OPEN_FILES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&self.0);
    }
}

/// A connection that holds the file's [`FileClaim`] until it closes.
pub struct ClaimedConnection {
    // Drops first: the claim outlives the connection.
    conn: Connection,
    _claim: FileClaim,
}

impl ClaimedConnection {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let claim = FileClaim::acquire(path)?;
        Ok(Self { conn: open(path)?, _claim: claim })
    }

    pub fn open_read_only(path: &Path) -> Result<Self, DbError> {
        let claim = FileClaim::acquire(path)?;
        Ok(Self { conn: open_read_only(path)?, _claim: claim })
    }
}

impl Deref for ClaimedConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.conn
    }
}
