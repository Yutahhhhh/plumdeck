//! The durable analysis-job queue: a small SQLite database beside the library
//! (`<library>.analysis-jobs.sqlite3`), ported from
//! backend/infra/repositories/analysis_job_repository.py with the same file
//! name, schema, statements and transaction boundaries.
//!
//! Every operation opens its own connection (busy timeout 30 s, WAL), as the
//! Python repository did, while holding the library's
//! [`DbHandle::queue_access`] gate (Python: `database_activity`) so a restore
//! never replaces the file mid-operation.
//!
//! Python's sqlite3 began a transaction implicitly before the first
//! INSERT/UPDATE and committed when the block ended; the write methods here
//! use one explicit transaction for the same statements.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plumdeck_core::compat::pyfmt::round_ndigits;
use plumdeck_core::compat::pyjson::{dumps_json, DumpOptions, PyValue};
use plumdeck_core::compat::pyjson_loads::loads;
use plumdeck_core::time::Clock;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::handle::{DbHandle, QueueAccess};
use crate::restore_recovery::queue_path;
use crate::DbError;

/// `sqlite3.connect(path, timeout=30)`.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

pub const CREATE_JOBS: &str = "CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, status TEXT, config TEXT, created REAL, updated REAL, elapsed REAL DEFAULT 0, error TEXT)";
pub const CREATE_ITEMS: &str = "CREATE TABLE IF NOT EXISTS items (job_id TEXT, track_id INTEGER, filepath TEXT, status TEXT DEFAULT 'pending', seconds REAL DEFAULT 0, error TEXT, PRIMARY KEY(job_id, track_id))";

/// Item statuses `get` always reports, in Python's order.
pub const ITEM_STATUSES: [&str; 5] = ["pending", "running", "completed", "skipped", "failed"];

/// A claimed queue item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueItem {
    pub track_id: i64,
    pub filepath: Option<String>,
}

/// `AnalysisJobRepository`.
#[derive(Clone)]
pub struct AnalysisJobQueue {
    path: PathBuf,
    gate: Option<DbHandle>,
    clock: Arc<dyn Clock>,
    recovered: Arc<Mutex<bool>>,
}

/// A queue connection plus the gate it was opened under. Fields drop in
/// order: the connection closes before the gate is released.
struct Opened {
    conn: Connection,
    _access: Option<QueueAccess>,
}

fn sql_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(x) => serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// `dict(row)` for every row of `sql`.
fn dict_rows(conn: &Connection, sql: &str, job_id: &str) -> Result<Vec<Value>, DbError> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().into_iter().map(str::to_owned).collect();
    let rows = stmt.query_map(params![job_id], |row| {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            object.insert(name.clone(), sql_to_json(row.get_ref(index)?));
        }
        Ok(Value::Object(object))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `bool(json.loads(config).get("background"))`; Python raised (and the
/// recovery rolled back) when the config is not a JSON object.
fn is_background(config: Option<&str>) -> Result<bool, DbError> {
    let invalid = || DbError::Invalid("analysis job config is not a JSON object".into());
    let parsed = loads(config.ok_or_else(invalid)?).map_err(|_| invalid())?;
    let PyValue::Dict(pairs) = parsed else { return Err(invalid()) };
    let value = pairs.iter().rev().find(|(key, _)| matches!(key, PyValue::Str(k) if k == "background"));
    Ok(match value.map(|(_, value)| value) {
        None | Some(PyValue::None) => false,
        Some(PyValue::Bool(flag)) => *flag,
        Some(PyValue::Int(number)) => *number != 0,
        Some(PyValue::Float(number)) => *number != 0.0,
        Some(PyValue::Str(text)) => !text.is_empty(),
        Some(PyValue::List(items)) => !items.is_empty(),
        Some(PyValue::Dict(items)) => !items.is_empty(),
        Some(_) => true,
    })
}

fn py_dumps(value: &Value) -> Result<String, DbError> {
    dumps_json(value, &DumpOptions::default()).map_err(|error| DbError::Invalid(error.to_string()))
}

impl AnalysisJobQueue {
    pub fn new(path: impl Into<PathBuf>, gate: Option<DbHandle>, clock: Arc<dyn Clock>) -> Self {
        Self { path: path.into(), gate, clock, recovered: Arc::new(Mutex::new(false)) }
    }

    /// The queue of `library` (Python: `DB_PATH + ".analysis-jobs.sqlite3"`).
    pub fn for_library(library: &DbHandle, clock: Arc<dyn Clock>) -> Self {
        Self::new(queue_path(library.path()), Some(library.clone()), clock)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connect(&self) -> Result<Opened, DbError> {
        if let Some(parent) = self.path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let access = self.gate.as_ref().map(DbHandle::queue_access);
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch(CREATE_JOBS)?;
        conn.execute_batch(CREATE_ITEMS)?;
        Ok(Opened { conn, _access: access })
    }

    fn write<T>(&self, operation: impl FnOnce(&Connection) -> Result<T, DbError>) -> Result<T, DbError> {
        let mut opened = self.connect()?;
        let tx = opened.conn.transaction()?;
        let value = operation(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// After a crash nothing is running: running, pausing and queued jobs go
    /// back to `queued` when they are automatic (`config.background`), to
    /// `paused` otherwise; running items return to pending.
    pub fn recover(&self) -> Result<(), DbError> {
        self.write(|conn| {
            let rows: Vec<(String, Option<String>)> = conn
                .prepare("SELECT id,config FROM jobs WHERE status IN ('running','pausing','queued')")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (id, config) in rows {
                let status = if is_background(config.as_deref())? { "queued" } else { "paused" };
                conn.execute("UPDATE jobs SET status=? WHERE id=?", params![status, id])?;
            }
            conn.execute("UPDATE items SET status='pending' WHERE status='running'", [])?;
            Ok(())
        })
    }

    /// `AnalysisJobService.repository`: recover on first use only. Python
    /// recovers lazily, not at startup, and marks the repository as created
    /// before recovering, so a failed recovery is not retried.
    pub fn recover_once(&self) -> Result<(), DbError> {
        let mut recovered = self.recovered.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *recovered {
            return Ok(());
        }
        *recovered = true;
        self.recover()
    }

    /// Insert a paused job and its items; returns the new uuid4 hex id.
    pub fn create(&self, config: &Value, tracks: &[QueueItem]) -> Result<String, DbError> {
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let now = self.clock.epoch_seconds();
        let config = py_dumps(config)?;
        self.write(|conn| {
            conn.execute(
                "INSERT INTO jobs (id,status,config,created,updated) VALUES (?,?,?,?,?)",
                params![job_id, "paused", config, now, now],
            )?;
            let mut insert = conn.prepare("INSERT INTO items (job_id,track_id,filepath) VALUES (?,?,?)")?;
            for track in tracks {
                insert.execute(params![job_id, track.track_id, track.filepath])?;
            }
            Ok(())
        })?;
        Ok(job_id)
    }

    /// The job (`None` or "": the most recently created) as Python's dict:
    /// the row, `config` parsed, `counts`, `total`, up to 20 `errors`,
    /// `current_tracks`, `remaining` and `estimated_remaining_seconds`.
    /// Without any job: `{"status": "idle"}`.
    pub fn get(&self, job_id: Option<&str>) -> Result<Value, DbError> {
        let opened = self.connect()?;
        let conn = &opened.conn;
        let job_id = job_id.filter(|id| !id.is_empty());
        let mut rows = match job_id {
            Some(id) => dict_rows(conn, "SELECT * FROM jobs WHERE id=?", id)?,
            None => {
                let mut stmt = conn.prepare("SELECT * FROM jobs ORDER BY created DESC LIMIT 1")?;
                let names: Vec<String> = stmt.column_names().into_iter().map(str::to_owned).collect();
                let rows = stmt.query_map([], |row| {
                    let mut object = Map::new();
                    for (index, name) in names.iter().enumerate() {
                        object.insert(name.clone(), sql_to_json(row.get_ref(index)?));
                    }
                    Ok(Value::Object(object))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            }
        };
        if rows.is_empty() {
            return match job_id {
                Some(_) => Err(DbError::JobNotFound),
                None => Ok(json!({"status": "idle"})),
            };
        }
        let Value::Object(mut result) = rows.swap_remove(0) else { unreachable!("rows are objects") };
        let id = result.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let config_text = result.get("config").and_then(Value::as_str).unwrap_or("null").to_owned();
        result.insert("config".into(), serde_json::from_str(&config_text)?);
        let mut counts: Vec<(Option<String>, i64)> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT status,count(*) FROM items WHERE job_id=? GROUP BY status")?;
            let found = stmt.query_map(params![id], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)))?;
            for row in found {
                counts.push(row?);
            }
        }
        let count = |status: &str| {
            counts.iter().find(|(name, _)| name.as_deref() == Some(status)).map_or(0, |(_, n)| *n)
        };
        let mut by_status = Map::new();
        for status in ITEM_STATUSES {
            by_status.insert(status.into(), json!(count(status)));
        }
        let total: i64 = counts.iter().map(|(_, n)| n).sum();
        result.insert("counts".into(), Value::Object(by_status));
        result.insert("total".into(), json!(total));
        result.insert(
            "errors".into(),
            Value::Array(dict_rows(
                conn,
                "SELECT track_id,filepath,error FROM items WHERE job_id=? AND status='failed' LIMIT 20",
                &id,
            )?),
        );
        result.insert(
            "current_tracks".into(),
            Value::Array(dict_rows(conn, "SELECT track_id,filepath FROM items WHERE job_id=? AND status='running'", &id)?),
        );
        let done = count("completed") + count("skipped") + count("failed");
        let remaining = total - done;
        result.insert("remaining".into(), json!(remaining));
        let estimate = if done != 0 {
            let elapsed = result.get("elapsed").and_then(Value::as_f64).unwrap_or(0.0);
            let value = round_ndigits(elapsed / done as f64 * remaining as f64, 1);
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
        } else {
            Value::Null
        };
        result.insert("estimated_remaining_seconds".into(), estimate);
        Ok(Value::Object(result))
    }

    /// Set status and `updated`; `elapsed` only when given; `error` always.
    pub fn update_job(&self, job_id: &str, status: &str, elapsed: Option<f64>, error: Option<&str>) -> Result<(), DbError> {
        let now = self.clock.epoch_seconds();
        self.write(|conn| {
            conn.execute(
                "UPDATE jobs SET status=?,updated=?,elapsed=COALESCE(?,elapsed),error=? WHERE id=?",
                params![status, now, elapsed, error, job_id],
            )?;
            Ok(())
        })
    }

    /// Store the new worker count and requeue running (and, if asked,
    /// failed) items of `job_id`.
    pub fn prepare_resume(&self, job_id: &str, workers: i64, retry_failed: bool) -> Result<(), DbError> {
        let job = self.get(Some(job_id))?;
        let mut config = job.get("config").cloned().unwrap_or(Value::Null);
        match &mut config {
            Value::Object(map) => {
                map.insert("workers".into(), json!(workers));
            }
            // Python: TypeError on item assignment.
            _ => return Err(DbError::Invalid("job config is not an object".into())),
        }
        let config = py_dumps(&config)?;
        self.write(|conn| {
            conn.execute("UPDATE jobs SET config=? WHERE id=?", params![config, job_id])?;
            conn.execute("UPDATE items SET status='pending' WHERE job_id=? AND status='running'", params![job_id])?;
            if retry_failed {
                conn.execute(
                    "UPDATE items SET status='pending',error=NULL WHERE job_id=? AND status='failed'",
                    params![job_id],
                )?;
            }
            Ok(())
        })
    }

    /// The next pending item of `job_id` in insertion order, marked running.
    pub fn claim(&self, job_id: &str) -> Result<Option<QueueItem>, DbError> {
        self.write(|conn| {
            let row = conn
                .query_row(
                    "SELECT track_id,filepath FROM items WHERE job_id=? AND status='pending' ORDER BY rowid LIMIT 1",
                    params![job_id],
                    |row| Ok(QueueItem { track_id: row.get(0)?, filepath: row.get(1)? }),
                )
                .optional()?;
            if let Some(item) = &row {
                conn.execute(
                    "UPDATE items SET status='running' WHERE job_id=? AND track_id=?",
                    params![job_id, item.track_id],
                )?;
            }
            Ok(row)
        })
    }

    pub fn finish(&self, job_id: &str, track_id: i64, status: &str, seconds: f64, error: Option<&str>) -> Result<(), DbError> {
        self.write(|conn| {
            conn.execute(
                "UPDATE items SET status=?,seconds=?,error=? WHERE job_id=? AND track_id=?",
                params![status, seconds, error, job_id, track_id],
            )?;
            Ok(())
        })
    }
}

/// workflow_service `_snapshot_sqlite`: a standalone, transactionally
/// consistent copy through SQLite's backup API (a raw copy would lose pages
/// still in the WAL), checked with `PRAGMA quick_check`.
pub fn snapshot_sqlite(source: &Path, destination: &Path) -> Result<(), DbError> {
    let live = Connection::open(source)?;
    let mut saved = Connection::open(destination)?;
    {
        let backup = rusqlite::backup::Backup::new(&live, &mut saved)?;
        backup.step(-1)?;
    }
    let check: String = saved.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(DbError::Invalid("解析ジョブDBの検査に失敗しました".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumdeck_core::time::FixedClock;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("plumdeck-jobs-{name}-{}", std::process::id()));
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

    fn clock(epoch: f64) -> Arc<dyn Clock> {
        let naive = chrono::DateTime::from_timestamp(epoch as i64, 0).unwrap().naive_utc();
        Arc::new(FixedClock { naive, epoch })
    }

    fn item(track_id: i64, path: &str) -> QueueItem {
        QueueItem { track_id, filepath: Some(path.into()) }
    }

    #[test]
    fn lifecycle_matches_python_repository() {
        let dir = Dir::new("lifecycle");
        let queue = AnalysisJobQueue::new(dir.0.join("lib.duckdb.analysis-jobs.sqlite3"), None, clock(1_700_000_000.25));
        assert_eq!(queue.get(None).unwrap(), json!({"status": "idle"}));
        assert!(matches!(queue.get(Some("missing")), Err(DbError::JobNotFound)));
        let config = json!({"features": ["rhythm"], "only_outdated": true, "workers": 2, "ratio": 1.0});
        let job = queue.create(&config, &[item(3, "/a.mp3"), item(1, "/b.mp3"), item(2, "/c.mp3")]).unwrap();
        assert_eq!(job.len(), 32);
        let stored: String = Connection::open(queue.path())
            .unwrap()
            .query_row("SELECT config FROM jobs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, r#"{"features": ["rhythm"], "only_outdated": true, "workers": 2, "ratio": 1.0}"#);
        assert_eq!(queue.claim(&job).unwrap(), Some(item(3, "/a.mp3")));
        queue.finish(&job, 3, "failed", 1.5, Some("boom")).unwrap();
        assert_eq!(queue.claim(&job).unwrap(), Some(item(1, "/b.mp3")));
        queue.finish(&job, 1, "completed", 2.0, None).unwrap();
        assert_eq!(queue.claim(&job).unwrap(), Some(item(2, "/c.mp3")));
        queue.update_job(&job, "running", Some(9.0), None).unwrap();
        let state = queue.get(None).unwrap();
        let keys: Vec<&str> = state.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "id", "status", "config", "created", "updated", "elapsed", "error", "counts", "total", "errors",
                "current_tracks", "remaining", "estimated_remaining_seconds"
            ]
        );
        assert_eq!(state["config"], config);
        assert_eq!(state["created"], json!(1_700_000_000.25));
        assert_eq!(state["counts"], json!({"pending": 0, "running": 1, "completed": 1, "skipped": 0, "failed": 1}));
        assert_eq!((state["total"].clone(), state["remaining"].clone()), (json!(3), json!(1)));
        assert_eq!(state["errors"], json!([{"track_id": 3, "filepath": "/a.mp3", "error": "boom"}]));
        assert_eq!(state["current_tracks"], json!([{"track_id": 2, "filepath": "/c.mp3"}]));
        // round(9.0 / 2 * 1, 1)
        assert_eq!(state["estimated_remaining_seconds"], json!(4.5));
        queue.recover().unwrap();
        let state = queue.get(Some(&job)).unwrap();
        assert_eq!(state["status"], "paused");
        assert_eq!(state["counts"]["pending"], 1);
        queue.prepare_resume(&job, 1, true).unwrap();
        let state = queue.get(Some(&job)).unwrap();
        assert_eq!(state["counts"]["pending"], 2);
        assert_eq!(state["config"]["workers"], 1);
        assert_eq!(state["errors"], json!([]));
        let mode: String = Connection::open(queue.path())
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn recover_once_does_not_retry() {
        let dir = Dir::new("once");
        let queue = AnalysisJobQueue::new(dir.0.join("q.sqlite3"), None, clock(1.0));
        let job = queue.create(&json!({}), &[item(1, "/x")]).unwrap();
        queue.update_job(&job, "running", None, None).unwrap();
        queue.recover_once().unwrap();
        assert_eq!(queue.get(None).unwrap()["status"], "paused");
        queue.update_job(&job, "running", None, None).unwrap();
        queue.clone().recover_once().unwrap();
        assert_eq!(queue.get(None).unwrap()["status"], "running");
    }

    #[test]
    fn recovery_requeues_background_jobs_only() {
        let dir = Dir::new("background");
        let queue = AnalysisJobQueue::new(dir.0.join("q.sqlite3"), None, clock(1.0));
        let background = queue.create(&json!({"background": true}), &[item(1, "/x")]).unwrap();
        let user = queue.create(&json!({"background": 0, "features": ["key"]}), &[item(2, "/y")]).unwrap();
        let waiting = queue.create(&json!({}), &[]).unwrap();
        let done = queue.create(&json!({"background": true}), &[]).unwrap();
        queue.update_job(&background, "running", None, None).unwrap();
        queue.update_job(&user, "pausing", None, None).unwrap();
        queue.update_job(&waiting, "queued", None, None).unwrap();
        queue.update_job(&done, "completed", None, None).unwrap();
        queue.recover().unwrap();
        let status = |job: &str| queue.get(Some(job)).unwrap()["status"].clone();
        assert_eq!([status(&background), status(&user), status(&waiting), status(&done)], ["queued", "paused", "paused", "completed"]);
        Connection::open(queue.path()).unwrap().execute("UPDATE jobs SET config='[]' WHERE id=?", params![user]).unwrap();
        queue.update_job(&user, "running", None, None).unwrap();
        assert!(queue.recover().is_err());
        assert_eq!(status(&user), "running", "the failed recovery rolled back");
    }

    /// The same build must later open rekordbox's encrypted master.db.
    #[test]
    fn sqlite_is_sqlcipher_and_round_trips_a_keyed_database() {
        let dir = Dir::new("cipher");
        let path = dir.0.join("keyed.db");
        let conn = Connection::open(&path).unwrap();
        // The key comes before anything reads the file.
        conn.pragma_update(None, "key", "not-the-real-key").unwrap();
        let version: String = conn.query_row("PRAGMA cipher_version", [], |row| row.get(0)).unwrap();
        assert!(!version.is_empty());
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('secret')").unwrap();
        drop(conn);
        assert!(!std::fs::read(&path).unwrap().starts_with(b"SQLite format 3"), "file is encrypted");
        let plain = Connection::open(&path).unwrap();
        assert!(plain.query_row("SELECT v FROM t", [], |row| row.get::<_, String>(0)).is_err());
        drop(plain);
        let keyed = Connection::open(&path).unwrap();
        keyed.execute_batch("PRAGMA key = 'not-the-real-key'").unwrap();
        let value: String = keyed.query_row("SELECT v FROM t", [], |row| row.get(0)).unwrap();
        assert_eq!(value, "secret");
    }

    #[test]
    fn snapshot_includes_committed_pages_still_in_the_wal() {
        let dir = Dir::new("snapshot");
        let (source, saved) = (dir.0.join("queue"), dir.0.join("copy"));
        let live = Connection::open(&source).unwrap();
        let _: String = live.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0)).unwrap();
        live.execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE test (value TEXT); INSERT INTO test VALUES ('committed in WAL');")
            .unwrap();
        assert!(std::fs::metadata(dir.0.join("queue-wal")).unwrap().len() > 0);
        snapshot_sqlite(&source, &saved).unwrap();
        let value: String =
            Connection::open(&saved).unwrap().query_row("SELECT value FROM test", [], |row| row.get(0)).unwrap();
        assert_eq!(value, "committed in WAL");
    }
}
