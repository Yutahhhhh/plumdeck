//! The library part of startup and shutdown, in the Python backend's order
//! (main.py lifespan and connection.py `init_db`):
//!
//! 1. stage `database`: recover an interrupted restore, check the file is not
//!    from a newer app (new, D-14), let compaction rebuild it if needed, open
//!    it, initialize the schema, delete retired LLM settings;
//! 2. stage `queue`: put interrupted Play imports back in the queue;
//! 3. stage `ready`.
//!
//! The analysis-job queue is recovered lazily on first use, as Python's
//! `AnalysisJobService.repository` does, not here.
//!
//! On shutdown the database is CHECKPOINTed and closed (`checkpoint_db()`,
//! `close_db()`).
//!
//! Everything here blocks; run it on a blocking thread.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use plumdeck_core::time::Clock;
use plumdeck_db::handle::DbHandle;
use plumdeck_db::jobs_sqlite::AnalysisJobQueue;
use plumdeck_db::repos::imports::reset_interrupted_imports;
use plumdeck_db::repos::settings::delete_retired_llm_settings;
use plumdeck_db::restore_recovery::recover_pending_restore;
use plumdeck_db::schema::{check_file_version, init_schema};
use plumdeck_db::DbError;

use crate::StartupStage;

/// How long shutdown waits for open leases before leaving the file to the
/// process exit.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// compaction.py `ensure_healthy_db`: may rebuild the file (schema below v4,
/// legacy JSON columns, bloat) before anything else opens it.
pub trait StartupCompaction: Send + Sync {
    fn ensure_healthy(&self, db_path: &Path) -> Result<(), DbError>;
}

/// Stand-in until compaction.py is ported: never rebuilds. Libraries that
/// Python would rebuild (schema below v5, legacy analysis columns, bloated
/// files) are opened as they are.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactionNotPorted;

impl StartupCompaction for CompactionNotPorted {
    fn ensure_healthy(&self, _db_path: &Path) -> Result<(), DbError> {
        Ok(())
    }
}

/// The open library: the DuckDB handle and its analysis-job queue.
#[derive(Clone)]
pub struct Library {
    pub db: DbHandle,
    pub jobs: AnalysisJobQueue,
}

/// A startup failure, printed the way the Python backend reports it.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// connection.py `init_db` prints this before re-raising.
    #[error("Error during database initialization: {0}")]
    Database(DbError),
    #[error("Error while restoring the import queue: {0}")]
    Queue(DbError),
}

/// `init_db`: everything up to an open, initialized library.
pub fn init_database(db_path: &Path, compaction: &dyn StartupCompaction) -> Result<DbHandle, DbError> {
    recover_pending_restore(db_path)?;
    check_file_version(db_path)?;
    compaction.ensure_healthy(db_path)?;
    let db = DbHandle::open(db_path)?;
    let initialized = db.write(|conn| {
        init_schema(conn)?;
        delete_retired_llm_settings(conn)?;
        Ok(())
    });
    if let Err(error) = initialized {
        // Release the file; the process is about to exit.
        let _ = db.shutdown(SHUTDOWN_TIMEOUT);
        return Err(error);
    }
    Ok(db)
}

/// Stages `database` and `queue`, reported through `progress` before each
/// one starts, as main.py's lifespan does.
pub fn open_library(
    db_path: &Path,
    compaction: &dyn StartupCompaction,
    clock: Arc<dyn Clock>,
    progress: &mut dyn FnMut(StartupStage),
) -> Result<Library, StartupError> {
    progress(StartupStage::Database);
    let db = init_database(db_path, compaction).map_err(StartupError::Database)?;
    progress(StartupStage::Queue);
    let reset = db.lease().and_then(|conn| reset_interrupted_imports(&conn));
    if let Err(error) = reset {
        let _ = db.shutdown(SHUTDOWN_TIMEOUT);
        return Err(StartupError::Queue(error));
    }
    let jobs = AnalysisJobQueue::for_library(&db, clock);
    Ok(Library { db, jobs })
}

/// `checkpoint_db()` then `close_db()`. A failed CHECKPOINT is reported the
/// way Python prints it; the file is closed either way.
pub fn close_library(library: &Library) -> Option<String> {
    library.db.shutdown(SHUTDOWN_TIMEOUT).err().map(|error| format!("DEBUG: CHECKPOINT skipped: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumdeck_core::time::SystemClock;
    use plumdeck_db::duckdb::Connection;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("plumdeck-startup-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Records what compaction saw, to check the order of the steps.
    struct Probe(Mutex<Vec<String>>);

    impl StartupCompaction for Probe {
        fn ensure_healthy(&self, db_path: &Path) -> Result<(), DbError> {
            let marker = plumdeck_db::restore_recovery::marker_path(db_path);
            self.0.lock().unwrap().push(format!("compaction marker={}", marker.exists()));
            Ok(())
        }
    }

    fn query<T: plumdeck_db::duckdb::types::FromSql>(db: &DbHandle, sql: &str) -> Vec<T> {
        let conn = db.lease().unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn startup_runs_python_steps_in_order_and_shutdown_checkpoints() {
        let dir = Dir::new("order");
        let path = dir.0.join("plumdeck.duckdb");
        {
            let conn = Connection::open(&path).unwrap();
            init_schema(&conn).unwrap();
            conn.execute_batch(
                "INSERT INTO settings VALUES ('llm_provider', 'openai'), ('root_path', '/music');
                 INSERT INTO import_batches (id, request_id, target_kind, state, created_at, updated_at)
                    VALUES ('b1', 'r1', 'library', 'processing', '2026-01-01 00:00:00', '2026-01-01 00:00:00'),
                           ('b2', 'r2', 'library', 'completed', '2026-01-01 00:00:00', '2026-01-01 00:00:00');
                 INSERT INTO import_items (id, batch_id, input_order, source_path, state)
                    VALUES ('i1', 'b1', 0, '/a.mp3', 'analyzing'), ('i2', 'b1', 1, '/b.mp3', 'failed');",
            )
            .unwrap();
        }
        let rollback = dir.0.join("plumdeck.duckdb.pre-restore-x");
        std::fs::copy(&path, &rollback).unwrap();
        plumdeck_db::restore_recovery::begin_restore(&path, &rollback, false).unwrap();
        std::fs::write(&path, b"interrupted").unwrap();

        let probe = Probe(Mutex::new(Vec::new()));
        let mut stages = Vec::new();
        let library = open_library(&path, &probe, Arc::new(SystemClock), &mut |stage| stages.push(stage)).unwrap();
        assert_eq!(stages, [StartupStage::Database, StartupStage::Queue]);
        // Recovery ran before compaction saw the file.
        assert_eq!(*probe.0.lock().unwrap(), ["compaction marker=false"]);
        assert_eq!(query::<String>(&library.db, "SELECT key FROM settings ORDER BY key"), ["root_path"]);
        assert_eq!(
            query::<String>(&library.db, "SELECT state || ':' || paused FROM import_batches ORDER BY id"),
            ["paused:true", "completed:false"]
        );
        assert_eq!(query::<String>(&library.db, "SELECT state FROM import_items ORDER BY id"), ["queued", "failed"]);
        assert!(!library.jobs.path().exists(), "the job queue is recovered lazily, not at startup");
        assert_eq!(close_library(&library), None);
        assert!(!library.db.is_open());
        assert!(!dir.0.join("plumdeck.duckdb.wal").exists());
    }

    #[test]
    fn a_newer_library_stops_startup_before_compaction() {
        let dir = Dir::new("newer");
        let path = dir.0.join("plumdeck.duckdb");
        {
            let conn = Connection::open(&path).unwrap();
            init_schema(&conn).unwrap();
            conn.execute_batch("UPDATE schema_info SET value = '9'").unwrap();
        }
        let probe = Probe(Mutex::new(Vec::new()));
        let mut stages = Vec::new();
        let error = open_library(&path, &probe, Arc::new(SystemClock), &mut |stage| stages.push(stage))
            .err()
            .unwrap();
        assert!(matches!(error, StartupError::Database(DbError::SchemaTooNew { found: 9, .. })));
        assert!(error.to_string().starts_with("Error during database initialization: "));
        assert_eq!(stages, [StartupStage::Database]);
        assert!(probe.0.lock().unwrap().is_empty());
        // The file is released: it can be opened again.
        drop(Connection::open(&path).unwrap());
    }

    #[test]
    fn a_fresh_install_creates_the_v5_library() {
        let dir = Dir::new("fresh");
        let path = dir.0.join("plumdeck.duckdb");
        let library = open_library(&path, &CompactionNotPorted, Arc::new(SystemClock), &mut |_| {}).unwrap();
        assert_eq!(query::<String>(&library.db, "SELECT value FROM schema_info WHERE key = 'version'"), ["5"]);
        assert_eq!(close_library(&library), None);
    }
}
