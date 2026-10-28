//! Schema initialization, ported from backend/infra/database/schema.py
//! (`init_raw_db`), plus a guard Python lacks: a library whose
//! `schema_info.version` is newer than [`SCHEMA_VERSION`] is refused before
//! anything touches it (deviation D-14, plan R1).
//!
//! The DDL is `contracts/schema/v5.sql`, the catalog Python's `init_db()`
//! leaves in a fresh file, run with `IF NOT EXISTS` so existing tables keep
//! their layout (ALTER-added columns stay appended). Libraries older than v4
//! or bloated files are rebuilt by compaction first; that is not ported yet.

use std::path::Path;

use duckdb::{params, Connection, OptionalExt};
use plumdeck_core::config::parse_py_int;

use crate::{ClaimedConnection, DbError, SCHEMA_V5_SQL, SCHEMA_VERSION};

/// `MIGRATIONS`: statements per target version. v4 (JSON to BLOB) is a file
/// rebuild in compaction.py and has no statements here.
pub const MIGRATIONS: &[(i64, &[&str])] = &[
    (
        2,
        &[
            "ALTER TABLE lyrics ADD COLUMN IF NOT EXISTS keywords_json VARCHAR",
            "ALTER TABLE lyrics ADD COLUMN IF NOT EXISTS keywords_content_hash VARCHAR",
        ],
    ),
    (3, &["DROP TABLE IF EXISTS presets", "DROP TABLE IF EXISTS prompts"]),
    (5, &[]),
];

/// `COMPATIBILITY_STATEMENTS`: additive columns run on every start, so files
/// already marked v4/v5 by older builds gain them without a rebuild.
pub const COMPATIBILITY_STATEMENTS: &[&str] = &[
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS source_section_position VARCHAR DEFAULT 'unknown'",
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS target_section_position VARCHAR DEFAULT 'unknown'",
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS source_cue_mode VARCHAR DEFAULT 'section_end'",
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS source_cue_end_timestamp DOUBLE",
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS target_intro_timestamp DOUBLE",
    "ALTER TABLE wordplay_pairs ADD COLUMN IF NOT EXISTS target_landing_timestamp DOUBLE",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS artist VARCHAR",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS title VARCHAR",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS source VARCHAR DEFAULT 'internal'",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS sample_rate_hz INTEGER",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS frame_count BIGINT",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS timeline_quality VARCHAR DEFAULT 'unavailable'",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS timeline_dropped_events BIGINT DEFAULT 0",
    "ALTER TABLE recordings ADD COLUMN IF NOT EXISTS revision INTEGER DEFAULT 1",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS in_ms DOUBLE DEFAULT 0",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS out_ms DOUBLE",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS playback_rate DOUBLE DEFAULT 1",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS extra_duration_ms DOUBLE DEFAULT 0",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS overlap_next_ms DOUBLE DEFAULT 0",
    "ALTER TABLE setlist_tracks ADD COLUMN IF NOT EXISTS revision INTEGER DEFAULT 1",
    "ALTER TABLE import_batches ADD COLUMN IF NOT EXISTS origin VARCHAR DEFAULT 'native_file_drop'",
    "ALTER TABLE import_batches ADD COLUMN IF NOT EXISTS analysis_profile VARCHAR DEFAULT 'auto'",
    "ALTER TABLE import_batches ADD COLUMN IF NOT EXISTS effective_analysis_profile VARCHAR",
    "ALTER TABLE import_items ADD COLUMN IF NOT EXISTS analysis_level VARCHAR",
    "ALTER TABLE tracks ADD COLUMN IF NOT EXISTS analysis_level VARCHAR",
];

/// The v5 DDL as idempotent statements: sequences first, then tables.
pub fn schema_statements() -> Vec<String> {
    let body: String = SCHEMA_V5_SQL
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    body.split(";\n")
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(|statement| {
            let statement = statement.trim_end_matches(';');
            for kind in ["CREATE SEQUENCE ", "CREATE TABLE "] {
                if let Some(rest) = statement.strip_prefix(kind) {
                    return format!("{kind}IF NOT EXISTS {rest}");
                }
            }
            panic!("unexpected statement in contracts/schema/v5.sql: {statement}")
        })
        .collect()
}

/// `get_current_schema_version`: `int(value)`, or 0 when the table or row is
/// missing or the value is not an integer.
pub fn read_schema_version(conn: &Connection) -> Result<i64, DbError> {
    let exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_tables()
         WHERE database_name = current_database() AND schema_name = 'main' AND table_name = 'schema_info'",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row("SELECT value FROM schema_info WHERE key = 'version'", [], |row| row.get(0))
        .optional()?;
    Ok(value.as_deref().and_then(parse_py_int).unwrap_or(0))
}

/// Refuse a library from a newer app before anything rewrites it.
pub fn ensure_supported_version(version: i64) -> Result<(), DbError> {
    if version > SCHEMA_VERSION {
        return Err(DbError::SchemaTooNew { found: version, supported: SCHEMA_VERSION });
    }
    Ok(())
}

/// The version guard for a file nothing has opened yet; a missing file passes
/// (a fresh install). Opened read-only, so a refused file stays untouched.
/// This is the first open of the file at startup, so an unreadable file fails
/// here with the message compaction.py's first open gives.
pub fn check_file_version(path: &Path) -> Result<Option<i64>, DbError> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = ClaimedConnection::open_read_only(path).map_err(|error| match error {
        DbError::DuckDb(error) => DbError::Invalid(format!("起動時の DB チェックでファイルを開けませんでした: {error}")),
        other => other,
    })?;
    let version = read_schema_version(&conn)?;
    ensure_supported_version(version)?;
    Ok(Some(version))
}

/// `init_raw_db`: in one transaction, create missing sequences and tables, run
/// the compatibility ALTERs, apply migrations above the stored version and
/// record v5.
pub fn init_schema(conn: &Connection) -> Result<(), DbError> {
    tracing::info!(target: LOG_TARGET, "Initializing DuckDB schema...");
    let result = init_schema_inner(conn);
    if let Err(error) = &result {
        tracing::error!(target: LOG_TARGET, "Failed to initialize database schema: {error}");
    }
    result
}

/// schema.py's logger name, so log lines read like the Python backend's.
const LOG_TARGET: &str = "infra.database.schema";

fn init_schema_inner(conn: &Connection) -> Result<(), DbError> {
    ensure_supported_version(read_schema_version(conn)?)?;
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        for statement in schema_statements() {
            conn.execute_batch(&statement)?;
        }
        for statement in COMPATIBILITY_STATEMENTS {
            conn.execute_batch(statement)?;
        }
        let current = read_schema_version(conn)?;
        if current < SCHEMA_VERSION {
            for (version, statements) in MIGRATIONS {
                if *version > current {
                    for statement in *statements {
                        tracing::info!(target: LOG_TARGET, "Applying migration v{version}: {statement}");
                        conn.execute_batch(statement)?;
                    }
                }
            }
            let version = SCHEMA_VERSION.to_string();
            conn.execute(
                "INSERT INTO schema_info (key, value) VALUES ('version', ?)
                 ON CONFLICT (key) DO UPDATE SET value = ?",
                params![version, version],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_idempotent_sequences_then_tables() {
        let statements = schema_statements();
        assert_eq!(statements.len(), 8 + 31);
        assert!(statements[..8].iter().all(|s| s.starts_with("CREATE SEQUENCE IF NOT EXISTS ")));
        assert!(statements[8..].iter().all(|s| s.starts_with("CREATE TABLE IF NOT EXISTS ")));
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 5);
    }

    #[test]
    fn version_text_parses_like_python_int() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);
        conn.execute_batch("CREATE TABLE schema_info (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL)").unwrap();
        assert_eq!(read_schema_version(&conn).unwrap(), 0);
        for (text, expected) in [(" 4\n", 4), ("+3", 3), ("1_0", 10), ("v5", 0), ("4.0", 0), ("", 0), ("-2", -2)] {
            conn.execute("INSERT OR REPLACE INTO schema_info VALUES ('version', ?)", [text]).unwrap();
            assert_eq!(read_schema_version(&conn).unwrap(), expected, "{text:?}");
        }
    }
}
