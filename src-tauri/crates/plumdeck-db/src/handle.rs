//! The library database handle shared by every repository (plan §1.4).
//!
//! One root `duckdb::Connection` per open file, handed out as up to
//! [`POOL_SIZE`] `try_clone` leases (Python: SQLAlchemy QueuePool, size 8,
//! timeout 120 s). The handle is swappable: a restore closes the file under a
//! [`MaintenanceGuard`] and reopens the replacement, and every clone of the
//! handle follows. Never open the library file a second time beside it; the
//! process-wide [`crate::FileClaim`] refuses that.
//!
//! The gate replaces connection.py's `db_lock`, `database_activity` and
//! `exclusive_database()`:
//!
//! - [`DbHandle::lease`]: a pooled connection, refused while maintenance runs
//!   ("バックアップ／復元中です。完了後に再試行してください").
//! - [`DbHandle::write`]: a lease plus the process-wide write lock (`db_lock`)
//!   for read/compare/write sections. Not reentrant: never call `write` from
//!   inside a `write` closure.
//! - [`DbHandle::activity`]: held by long-running analysis/import work for its
//!   whole run; maintenance refuses while one exists
//!   ("解析・取り込みを停止してからバックアップ／復元してください").
//! - [`DbHandle::queue_access`]: short work outside DuckDB that a restore must
//!   not interleave with (the SQLite analysis queue). Maintenance waits for it.
//!
//! `activity` and `queue_access` wait while maintenance runs, as Python's
//! `database_activity` RLock blocked them. Maintenance itself waits up to
//! [`MAINTENANCE_TIMEOUT`] for leases and queue accesses to drain, then
//! refuses ("データベースを使用中です。処理の完了後に再試行してください");
//! Python refused at once (deviation D-10).
//!
//! All methods block: call them from `spawn_blocking`, not a runtime worker.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use duckdb::Connection;

use crate::{ensure_engine_version, open, DbError, FileClaim};

pub const POOL_SIZE: usize = 8;
pub const POOL_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Client code may panic while holding the write guard. No shared pool
    // mutation runs in client code, and its lease still rolls back on Drop.
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct DbInner {
    // Fields drop in order: every connection closes before the claim ends.
    root: Connection,
    idle: Vec<Connection>,
    created: usize,
    _claim: FileClaim,
}

impl DbInner {
    fn open(path: &Path) -> Result<Self, DbError> {
        let claim = FileClaim::acquire(path)?;
        let root = open(path)?;
        ensure_engine_version(&root)?;
        Ok(Self { root, idle: Vec::new(), created: 0, _claim: claim })
    }
}

struct State {
    inner: Option<DbInner>,
    /// Pooled connections currently lent out (maintenance leases included).
    leased: usize,
    /// Open [`QueueAccess`] guards.
    accesses: usize,
    /// Open [`DbActivity`] guards.
    activities: usize,
    maintenance: bool,
    /// Bumped whenever maintenance is reserved, so a pool waiter that slept
    /// through a maintenance window still refuses its stale request.
    generation: u64,
    #[cfg(test)]
    pool_waiters: usize,
}

struct Shared {
    path: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    write: Mutex<()>,
}

impl Shared {
    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait_timeout<'a>(&self, state: MutexGuard<'a, State>, timeout: Duration) -> MutexGuard<'a, State> {
        self.changed.wait_timeout(state, timeout).unwrap_or_else(|poisoned| poisoned.into_inner()).0
    }

    /// Blocks while maintenance runs (Python blocked on `database_activity`).
    fn wait_out_maintenance<'a>(&self, mut state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        while state.maintenance {
            state = self.wait(state);
        }
        state
    }

    /// Reserve the gate, then wait for leases and queue accesses to drain.
    /// Reserving first keeps a stream of new leases from starving it.
    fn reserve(self: &Arc<Self>, timeout: Duration, allow_activities: bool) -> Result<MaintenanceGuard, DbError> {
        let started = Instant::now();
        let mut state = lock(&self.state);
        if state.maintenance {
            return Err(DbError::DatabaseInUse);
        }
        if state.activities != 0 && !allow_activities {
            return Err(DbError::BackgroundActivity);
        }
        state.maintenance = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
        loop {
            if state.leased == 0 && state.accesses == 0 {
                return Ok(MaintenanceGuard(self.clone()));
            }
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                state.maintenance = false;
                self.changed.notify_all();
                return Err(DbError::DatabaseInUse);
            };
            state = self.wait_timeout(state, remaining);
        }
    }
}

/// Clones of this handle keep referring to the reopened library after a
/// restore; repositories must not retain a separate `Connection::open` handle.
#[derive(Clone)]
pub struct DbHandle(Arc<Shared>);

impl DbHandle {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        Ok(Self(Arc::new(Shared {
            path: path.to_owned(),
            state: Mutex::new(State {
                inner: Some(DbInner::open(path)?),
                leased: 0,
                accesses: 0,
                activities: 0,
                maintenance: false,
                generation: 0,
                #[cfg(test)]
                pool_waiters: 0,
            }),
            changed: Condvar::new(),
            write: Mutex::new(()),
        })))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn is_open(&self) -> bool {
        lock(&self.0.state).inner.is_some()
    }

    pub fn lease(&self) -> Result<DbLease, DbError> {
        self.lease_timeout(POOL_TIMEOUT)
    }

    /// The timeout bounds pool exhaustion; maintenance fails immediately with
    /// Python's existing message instead of queuing a stale request.
    pub fn lease_timeout(&self, timeout: Duration) -> Result<DbLease, DbError> {
        let started = Instant::now();
        let mut state = lock(&self.0.state);
        let generation = state.generation;
        loop {
            if state.maintenance || state.generation != generation {
                return Err(DbError::Maintenance);
            }
            if let Some(lease) = self.take_connection(&mut state)? {
                return Ok(lease);
            }
            let remaining = timeout.checked_sub(started.elapsed()).ok_or(DbError::PoolTimeout)?;
            #[cfg(test)]
            {
                state.pool_waiters += 1;
                self.0.changed.notify_all();
            }
            state = self.0.wait_timeout(state, remaining);
            #[cfg(test)]
            {
                state.pool_waiters -= 1;
            }
        }
    }

    /// An idle or new pooled connection, or `None` when all are lent out.
    fn take_connection(&self, state: &mut State) -> Result<Option<DbLease>, DbError> {
        let inner = state.inner.as_mut().ok_or(DbError::Closed)?;
        let conn = if let Some(conn) = inner.idle.pop() {
            conn
        } else if inner.created < POOL_SIZE {
            let conn = inner.root.try_clone()?;
            inner.created += 1;
            conn
        } else {
            return Ok(None);
        };
        state.leased += 1;
        Ok(Some(DbLease { shared: self.0.clone(), conn: Some(conn) }))
    }

    /// Serialize a read/compare/write operation. Transactions remain explicit
    /// so callers can match Python's transaction boundaries.
    pub fn write<T>(&self, operation: impl FnOnce(&Connection) -> Result<T, DbError>) -> Result<T, DbError> {
        // Count accepted writers while they wait for the write lock, so a
        // restore cannot overtake an operation accepted on the old library.
        // Tuple fields drop in order: return/rollback the lease before
        // releasing the write lock, including if the operation panics.
        let guards = (self.lease()?, lock(&self.0.write));
        operation(&guards.0)
    }

    /// Hold for the duration of a background analysis/import task, including
    /// intervals where it has no DB connection. Maintenance refuses while one
    /// exists; a new one waits for running maintenance to finish.
    pub fn activity(&self) -> DbActivity {
        let mut state = self.0.wait_out_maintenance(lock(&self.0.state));
        state.activities += 1;
        DbActivity(self.0.clone())
    }

    /// Hold while touching files a restore replaces outside DuckDB (the
    /// analysis-job queue). Waits for running maintenance to finish;
    /// maintenance waits for open accesses. Not for the maintenance holder.
    pub fn queue_access(&self) -> QueueAccess {
        let mut state = self.0.wait_out_maintenance(lock(&self.0.state));
        state.accesses += 1;
        QueueAccess(self.0.clone())
    }

    pub fn maintenance(&self) -> Result<MaintenanceGuard, DbError> {
        self.maintenance_timeout(MAINTENANCE_TIMEOUT)
    }

    pub fn maintenance_timeout(&self, timeout: Duration) -> Result<MaintenanceGuard, DbError> {
        self.0.reserve(timeout, false)
    }

    pub fn checkpoint_and_close(&self) -> Result<(), DbError> {
        let mut maintenance = self.maintenance()?;
        maintenance.checkpoint()?;
        maintenance.close()
    }

    /// Process shutdown (main.py: `checkpoint_db()` then `close_db()`):
    /// CHECKPOINT and close once open leases are returned, regardless of
    /// background activities. Closing happens even when CHECKPOINT fails;
    /// that error is returned for logging. If leases stay out past `timeout`
    /// the file is left open (the process exit releases it).
    pub fn shutdown(&self, timeout: Duration) -> Result<(), DbError> {
        let started = Instant::now();
        {
            // Let a running backup/restore finish first.
            let mut state = lock(&self.0.state);
            while state.maintenance {
                let remaining = timeout.checked_sub(started.elapsed()).ok_or(DbError::DatabaseInUse)?;
                state = self.0.wait_timeout(state, remaining);
            }
            if state.inner.is_none() {
                return Ok(());
            }
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        let mut maintenance = self.0.reserve(remaining, true)?;
        let checkpoint = maintenance.checkpoint();
        maintenance.close()?;
        checkpoint
    }
}

/// A pooled connection. Do not clone the connection out of a lease: every
/// connection must be released before maintenance can replace the file.
pub struct DbLease {
    shared: Arc<Shared>,
    conn: Option<Connection>,
}

impl Deref for DbLease {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("lease holds its connection")
    }
}

impl Drop for DbLease {
    fn drop(&mut self) {
        let conn = self.conn.take().expect("lease returned once");
        // A failed request or panic must not leak an uncommitted transaction to
        // the next borrower. Discard the clone if rollback itself fails.
        // duckdb-rs 1.10505.0's is_autocommit() is a constant true stub.
        // Always roll back; accept only DuckDB's pinned no-transaction error.
        let reusable = match conn.execute_batch("ROLLBACK") {
            Ok(()) => true,
            Err(error) => error.to_string() == "TransactionContext Error: cannot rollback - no transaction is active",
        };
        let mut state = lock(&self.shared.state);
        let discarded = match state.inner.as_mut() {
            Some(inner) if reusable => {
                inner.idle.push(conn);
                None
            }
            Some(inner) => {
                inner.created -= 1;
                Some(conn)
            }
            None => Some(conn),
        };
        state.leased -= 1;
        self.shared.changed.notify_all();
        drop(state);
        drop(discarded);
    }
}

/// See [`DbHandle::activity`].
pub struct DbActivity(Arc<Shared>);

impl Drop for DbActivity {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.activities -= 1;
        self.0.changed.notify_all();
    }
}

/// See [`DbHandle::queue_access`].
pub struct QueueAccess(Arc<Shared>);

impl Drop for QueueAccess {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.accesses -= 1;
        self.0.changed.notify_all();
    }
}

/// Exclusive access to the file lifecycle, including while the DB is closed.
/// Closing never reopens implicitly: a failed restore stays closed until the
/// caller recovers the original pair and explicitly calls `reopen`.
pub struct MaintenanceGuard(Arc<Shared>);

impl MaintenanceGuard {
    /// A pooled connection for the maintenance work itself (backup's
    /// ATTACH / COPY FROM DATABASE). Return it before [`Self::close`].
    pub fn lease(&self) -> Result<DbLease, DbError> {
        let mut state = lock(&self.0.state);
        DbHandle(self.0.clone()).take_connection(&mut state)?.ok_or(DbError::PoolTimeout)
    }

    pub fn checkpoint(&self) -> Result<(), DbError> {
        let state = lock(&self.0.state);
        state.inner.as_ref().ok_or(DbError::Closed)?.root.execute_batch("CHECKPOINT")?;
        Ok(())
    }

    /// Close every connection, so the file can be replaced. Fails while one
    /// of this guard's own leases is still out.
    pub fn close(&mut self) -> Result<(), DbError> {
        let inner = {
            let mut state = lock(&self.0.state);
            if state.leased != 0 {
                return Err(DbError::DatabaseInUse);
            }
            state.inner.take()
        };
        // Closing may checkpoint; do it without holding the state lock.
        drop(inner);
        Ok(())
    }

    pub fn reopen(&mut self) -> Result<(), DbError> {
        if lock(&self.0.state).inner.is_some() {
            return Err(DbError::AlreadyOpen);
        }
        // Only this guard opens or closes while maintenance is held.
        let inner = DbInner::open(&self.0.path)?;
        lock(&self.0.state).inner = Some(inner);
        Ok(())
    }
}

impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.maintenance = false;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn pool_waiter_rejects_even_after_maintenance_gate_has_reopened() {
        let dir = std::env::temp_dir().join(format!("plumdeck-pool-generation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.duckdb");
        let handle = DbHandle::open(&path).unwrap();
        let leases: Vec<_> = (0..POOL_SIZE).map(|_| handle.lease().unwrap()).collect();
        let (result_tx, result_rx) = mpsc::channel();
        let other = handle.clone();
        let waiter = std::thread::spawn(move || {
            result_tx.send(matches!(other.lease(), Err(DbError::Maintenance))).unwrap();
        });
        // Observe the waiter only after it has released the state lock in
        // Condvar::wait_timeout. Then reserve and clear maintenance while
        // holding that same lock, so the waiter cannot see the gate set.
        let mut state = lock(&handle.0.state);
        while state.pool_waiters == 0 {
            state = handle.0.changed.wait(state).unwrap();
        }
        state.maintenance = true;
        state.generation = state.generation.wrapping_add(1);
        state.maintenance = false;
        handle.0.changed.notify_all();
        drop(state);
        assert!(result_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        waiter.join().unwrap();
        drop(leases);
        handle.checkpoint_and_close().unwrap();
        drop(handle);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
