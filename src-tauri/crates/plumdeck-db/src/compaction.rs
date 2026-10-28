//! The part of compaction.py `ensure_healthy_db` that runs on a healthy
//! library: the `features_extra_clean` marker.
//!
//! Python scans `track_analyses.features_extra_json` once for keys outside
//! the persisted allowlist (backend/utils/features_extra.py). A clean file
//! gets `schema_info.features_extra_clean = '2'` written in place, so later
//! starts only read the marker.
//!
//! Not ported yet: the rebuilds themselves (schema migration below v5,
//! legacy JSON columns, an allowlist clean-up of a dirty file, bloat
//! compaction), the interrupted-swap recovery and the pruning of `.bak-v*`
//! files. A library that needs a rebuild is opened as it is and the marker
//! is left unset, so Python still rebuilds it after a rollback.

use std::path::Path;

use duckdb::{params, Connection, OptionalExt};

use plumdeck_core::config::parse_py_int;
use crate::{ClaimedConnection, DbError, SCHEMA_VERSION};

/// `FX_CLEAN_KEY`.
pub const FX_CLEAN_KEY: &str = "features_extra_clean";
/// `FX_CLEAN_VERSION`.
pub const FX_CLEAN_VERSION: i64 = 2;

/// `PERSISTED_KEYS`.
pub const PERSISTED_KEYS: [&str; 9] = [
    "analysis_components",
    "playback_grid",
    "playback_grid_version",
    "analysis_bpm",
    "analysis_key",
    "bpm_confidence",
    "key_strength",
    "bpm_raw",
    "audio_sha256",
];

const LOG_TARGET: &str = "infra.database.compaction";

/// What the startup check decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// No file yet (a fresh install).
    Missing,
    /// Nothing to rebuild; the marker is set.
    Healthy,
    /// Python would rebuild the file here (not ported): opened as it is.
    NeedsRebuild,
}

/// `_read_info_int`: `int(value)` of a `schema_info` row, 0 on any failure.
fn read_info_int(conn: &Connection, key: &str) -> i64 {
    let value: Result<Option<Option<String>>, _> =
        conn.query_row("SELECT value FROM schema_info WHERE key = ?", params![key], |row| row.get(0)).optional();
    match value {
        Ok(Some(Some(text))) => parse_py_int(&text).unwrap_or(0),
        _ => 0,
    }
}

/// `_has_legacy_analysis_columns`.
fn has_legacy_analysis_columns(conn: &Connection) -> bool {
    let columns: Result<Vec<String>, duckdb::Error> = conn
        .prepare(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'main' AND table_name = 'track_analyses'",
        )
        .and_then(|mut statement| statement.query_map([], |row| row.get(0))?.collect());
    columns.is_ok_and(|columns| columns.iter().any(|c| c == "waveform_peaks" || c == "beat_positions"))
}

fn persisted_keys_literal() -> String {
    let quoted: Vec<String> = PERSISTED_KEYS.iter().map(|key| format!("'{key}'")).collect();
    format!("[{}]::VARCHAR[]", quoted.join(", "))
}

/// `_needs_fx_cleanup`: true when a row holds a key outside the allowlist;
/// marks a clean file. A failed check is skipped (retried next start).
fn needs_fx_cleanup(conn: &Connection) -> bool {
    if read_info_int(conn, FX_CLEAN_KEY) >= FX_CLEAN_VERSION {
        return false;
    }
    let sql = format!(
        "SELECT 1 FROM track_analyses WHERE CASE \
         WHEN json_valid(features_extra_json) AND json_type(features_extra_json) = 'OBJECT' \
         THEN NOT list_has_all({}, json_keys(features_extra_json)) \
         ELSE false END LIMIT 1",
        persisted_keys_literal()
    );
    let checked = (|| -> Result<bool, duckdb::Error> {
        let dirty = conn.query_row(&sql, [], |_| Ok(())).optional()?.is_some();
        if !dirty {
            conn.execute(
                "INSERT INTO schema_info (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![FX_CLEAN_KEY, FX_CLEAN_VERSION.to_string()],
            )?;
        }
        Ok(dirty)
    })();
    match checked {
        Ok(dirty) => dirty,
        Err(error) => {
            tracing::warn!(target: LOG_TARGET, "features_extra の判定をスキップしました: {error}");
            false
        }
    }
}

/// `ensure_healthy_db` up to its rebuild decision.
pub fn ensure_healthy_db(db_path: &Path) -> Result<Health, DbError> {
    if !db_path.exists() {
        return Ok(Health::Missing);
    }
    let conn = ClaimedConnection::open(db_path).map_err(|error| match error {
        DbError::DuckDb(error) => DbError::Invalid(format!("起動時の DB チェックでファイルを開けませんでした: {error}")),
        other => other,
    })?;
    let version = read_info_int(&conn, "version");
    let needs_migrate = version < SCHEMA_VERSION || has_legacy_analysis_columns(&conn);
    if needs_migrate || needs_fx_cleanup(&conn) {
        tracing::warn!(target: LOG_TARGET, "DuckDB ファイルの再構築は未移植です。現在のファイルのまま起動します");
        return Ok(Health::NeedsRebuild);
    }
    Ok(Health::Healthy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(dir: &Path, extra: &str) -> std::path::PathBuf {
        let path = dir.join("plumdeck.duckdb");
        let conn = Connection::open(&path).unwrap();
        crate::schema::init_schema(&conn).unwrap();
        conn.execute("INSERT INTO track_analyses (track_id, features_extra_json) VALUES (1, ?)", params![extra])
            .unwrap();
        path
    }

    fn marker(path: &Path) -> Option<String> {
        let conn = Connection::open(path).unwrap();
        conn.query_row("SELECT value FROM schema_info WHERE key = ?", params![FX_CLEAN_KEY], |row| row.get(0))
            .optional()
            .unwrap()
    }

    #[test]
    fn marks_clean_files_and_leaves_dirty_ones() {
        let root = std::env::temp_dir().join(format!("plumdeck-compaction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, extra, health) in [
            ("clean", r#"{"analysis_components": {}, "bpm_raw": 1}"#, Health::Healthy),
            ("not-json", "not json", Health::Healthy),
            ("list", "[1, 2]", Health::Healthy),
            ("dirty", r#"{"waveform_peaks": [1]}"#, Health::NeedsRebuild),
        ] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = library(&dir, extra);
            assert_eq!(ensure_healthy_db(&path).unwrap(), health, "{name}");
            let expected = (health == Health::Healthy).then(|| "2".to_owned());
            assert_eq!(marker(&path), expected, "{name}");
            // The marker short-cuts the scan.
            if health == Health::Healthy {
                assert_eq!(ensure_healthy_db(&path).unwrap(), Health::Healthy);
            }
        }
        assert_eq!(ensure_healthy_db(&root.join("missing.duckdb")).unwrap(), Health::Missing);
        let dir = root.join("old");
        std::fs::create_dir_all(&dir).unwrap();
        let path = library(&dir, "{}");
        Connection::open(&path).unwrap().execute_batch("UPDATE schema_info SET value = '4' WHERE key = 'version'").unwrap();
        assert_eq!(ensure_healthy_db(&path).unwrap(), Health::NeedsRebuild);
        assert_eq!(marker(&path), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
