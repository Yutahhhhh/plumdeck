//! Synthetic storage tests: never open the user's library or queue.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant};

use plumdeck_db::handle::{DbHandle, POOL_SIZE};
use plumdeck_db::restore_recovery::{begin_restore, marker_path, queue_path, recover_pending_restore};
use plumdeck_db::{open, ClaimedConnection, DbError};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("plumdeck-storage-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn db(&self) -> PathBuf {
        self.0.join("library.duckdb")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn initialize(handle: &DbHandle) {
    handle
        .lease()
        .unwrap()
        .execute_batch("CREATE TABLE counter (n INTEGER); INSERT INTO counter VALUES (0)")
        .unwrap();
}

fn value(handle: &DbHandle) -> i32 {
    handle
        .lease()
        .unwrap()
        .query_row("SELECT n FROM counter", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn bounded_pool_reuses_a_released_connection() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    let mut leases: Vec<_> = (0..POOL_SIZE).map(|_| handle.lease().unwrap()).collect();
    assert!(matches!(
        handle.lease_timeout(Duration::ZERO),
        Err(DbError::PoolTimeout)
    ));
    leases
        .last()
        .unwrap()
        .execute_batch("CREATE TEMP TABLE lease_marker AS SELECT 7 AS n")
        .unwrap();
    leases.pop();
    {
        let reused = handle.lease().unwrap();
        let marker: i32 = reused
            .query_row("SELECT n FROM lease_marker", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            marker, 7,
            "the same connection, not a fresh clone, returned to the pool"
        );
    }
    assert_eq!(value(&handle), 0);
    drop(leases);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn unfinished_transaction_is_rolled_back_before_reuse() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    {
        let lease = handle.lease().unwrap();
        lease.execute_batch("BEGIN; UPDATE counter SET n=99").unwrap();
    }
    assert_eq!(value(&handle), 0);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn compare_and_write_is_serialized_across_repositories() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let handle = handle.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..10 {
                    handle
                        .write(|conn| {
                            conn.execute_batch("BEGIN")?;
                            let count: i32 = conn.query_row("SELECT n FROM counter", [], |row| row.get(0))?;
                            conn.execute("UPDATE counter SET n=?", [count + 1])?;
                            conn.execute_batch("COMMIT")?;
                            Ok(())
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(value(&handle), 80);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_timeout_releases_the_gate_and_background_tasks_block_it() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let lease = handle.lease().unwrap();
    assert!(matches!(
        handle.maintenance_timeout(Duration::ZERO),
        Err(DbError::DatabaseInUse)
    ));
    assert!(
        handle.lease().is_ok(),
        "timed-out maintenance must release the gate"
    );
    drop(lease);
    let activity = handle.activity();
    assert!(matches!(handle.maintenance(), Err(DbError::BackgroundActivity)));
    drop(activity);
    {
        let maintenance = handle.maintenance().unwrap();
        assert!(matches!(handle.lease(), Err(DbError::Maintenance)));
        assert!(matches!(handle.maintenance(), Err(DbError::DatabaseInUse)));
        // Background work waits for maintenance (Python: database_activity).
        let (entered_tx, entered_rx) = mpsc::channel();
        let waiting = handle.clone();
        let worker = std::thread::spawn(move || {
            let activity = waiting.activity();
            entered_tx.send(()).unwrap();
            drop(activity);
            let access = waiting.queue_access();
            drop(access);
        });
        assert!(entered_rx.recv_timeout(Duration::from_millis(200)).is_err(), "activity entered during maintenance");
        drop(maintenance);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_waits_for_queue_access_and_refuses_after_the_timeout() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let access = handle.queue_access();
    let started = Instant::now();
    assert!(matches!(
        handle.maintenance_timeout(Duration::from_millis(150)),
        Err(DbError::DatabaseInUse)
    ));
    assert!(started.elapsed() >= Duration::from_millis(150), "maintenance waited before refusing");
    let worker_handle = handle.clone();
    let worker = std::thread::spawn(move || worker_handle.maintenance().map(drop));
    std::thread::sleep(Duration::from_millis(50));
    drop(access);
    worker.join().unwrap().unwrap();
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_runs_sql_through_its_own_lease_before_closing() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    let mut maintenance = handle.maintenance().unwrap();
    let lease = maintenance.lease().unwrap();
    let n: i32 = lease.query_row("SELECT n FROM counter", [], |row| row.get(0)).unwrap();
    assert_eq!(n, 0);
    assert!(matches!(maintenance.close(), Err(DbError::DatabaseInUse)), "own lease still out");
    drop(lease);
    maintenance.checkpoint().unwrap();
    maintenance.close().unwrap();
    assert!(!handle.is_open());
    drop(maintenance);
    assert!(matches!(handle.lease(), Err(DbError::Closed)));
}

#[test]
fn the_same_file_cannot_be_opened_twice_in_one_process() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    assert!(matches!(DbHandle::open(&sandbox.db()), Err(DbError::FileAlreadyOpen(_))));
    // A relative or non-normalized spelling is the same file.
    let alias = sandbox.0.join(".").join("library.duckdb");
    assert!(matches!(ClaimedConnection::open(&alias), Err(DbError::FileAlreadyOpen(_))));
    handle.checkpoint_and_close().unwrap();
    // Closing releases the claim.
    drop(ClaimedConnection::open(&alias).unwrap());
    let mut maintenance = handle.maintenance().unwrap();
    maintenance.reopen().unwrap();
    assert!(matches!(ClaimedConnection::open(&sandbox.db()), Err(DbError::FileAlreadyOpen(_))));
    drop(maintenance);
    handle.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
fn shutdown_checkpoints_despite_background_activity_and_waits_for_leases() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    let activity = handle.activity();
    let lease = handle.lease().unwrap();
    assert!(matches!(handle.shutdown(Duration::from_millis(50)), Err(DbError::DatabaseInUse)));
    assert!(handle.is_open(), "an outstanding lease keeps the file open");
    let worker_handle = handle.clone();
    let worker = std::thread::spawn(move || worker_handle.shutdown(Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(50));
    drop(lease);
    worker.join().unwrap().unwrap();
    assert!(!handle.is_open());
    assert!(!PathBuf::from(format!("{}.wal", sandbox.db().display())).exists(), "CHECKPOINT folded the WAL");
    handle.shutdown(Duration::ZERO).unwrap();
    drop(activity);
}

// backend/tests/test_database_concurrency.py

#[test]
fn library_connection_reuses_handles_during_search_and_analysis_writes() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    handle
        .write(|conn| {
            // Match production: stored JSON text, cast only during similarity search.
            conn.execute_batch(
                "BEGIN; CREATE TABLE vectors (id INTEGER PRIMARY KEY, value VARCHAR);
                 INSERT INTO vectors VALUES (1, '[1,2,3]'), (2, '[3,2,1]'); COMMIT",
            )?;
            Ok(())
        })
        .unwrap();
    let barrier = Arc::new(Barrier::new(6));
    let workers: Vec<_> = (0..6)
        .map(|worker| {
            let handle = handle.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for index in 0..40 {
                    if worker < 2 {
                        handle
                            .write(|conn| {
                                conn.execute_batch("BEGIN")?;
                                conn.execute(
                                    "UPDATE vectors SET value=? WHERE id=?",
                                    plumdeck_db::duckdb::params![format!("[1,2,{}]", index + 1), worker + 1],
                                )?;
                                conn.execute_batch("COMMIT")?;
                                Ok(())
                            })
                            .unwrap();
                    } else {
                        let lease = handle.lease().unwrap();
                        let mut stmt = lease
                            .prepare(
                                "SELECT id, array_cosine_similarity(value::FLOAT[3], [1,2,3]::FLOAT[3]) \
                                 FROM vectors ORDER BY 2 DESC",
                            )
                            .unwrap();
                        let rows: Vec<(i32, f32)> = stmt
                            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                            .unwrap()
                            .collect::<Result<_, _>>()
                            .unwrap();
                        assert_eq!(rows.len(), 2);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn library_connection_recovers_after_rolled_back_query() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    for _ in 0..POOL_SIZE + 1 {
        let lease = handle.lease().unwrap();
        lease.execute_batch("BEGIN").unwrap();
        assert!(lease.execute_batch("SELECT * FROM absent_table").is_err());
    }
    let answer: i32 = handle.lease().unwrap().query_row("SELECT 42", [], |row| row.get(0)).unwrap();
    assert_eq!(answer, 42);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn long_session_does_not_block_an_independent_search() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let held = handle.lease().unwrap();
    let one: i32 = held.query_row("SELECT 1", [], |row| row.get(0)).unwrap();
    assert_eq!(one, 1);
    let other = handle.clone();
    let search = std::thread::spawn(move || -> i32 {
        other.lease_timeout(Duration::from_secs(10)).unwrap().query_row("SELECT 2", [], |row| row.get(0)).unwrap()
    });
    assert_eq!(search.join().unwrap(), 2);
    drop(held);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_waits_for_an_existing_lease() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let lease = handle.lease().unwrap();
    let worker_handle = handle.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let mut maintenance = worker_handle.maintenance().unwrap();
        maintenance.checkpoint().unwrap();
        maintenance.close().unwrap();
    });
    started_rx.recv().unwrap();
    let started = Instant::now();
    loop {
        if matches!(handle.lease_timeout(Duration::ZERO), Err(DbError::Maintenance)) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "maintenance reserves gate before waiting"
        );
        std::thread::yield_now();
    }
    assert!(
        handle.is_open(),
        "maintenance cannot close while the old lease exists"
    );
    drop(lease);
    worker.join().unwrap();
    assert!(!handle.is_open());
    assert!(matches!(handle.lease(), Err(DbError::Closed)));
}

#[test]
fn shared_handle_reopens_the_replacement_and_keeps_failed_reopen_closed() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let repository_handle = handle.clone();
    initialize(&handle);
    let replacement = sandbox.0.join("replacement.duckdb");
    {
        let conn = open(&replacement).unwrap();
        conn.execute_batch("CREATE TABLE counter (n INTEGER); INSERT INTO counter VALUES (42); CHECKPOINT")
            .unwrap();
    }
    {
        let mut maintenance = handle.maintenance().unwrap();
        assert!(matches!(maintenance.reopen(), Err(DbError::AlreadyOpen)));
        maintenance.checkpoint().unwrap();
        maintenance.close().unwrap();
        fs::write(sandbox.db(), b"broken replacement").unwrap();
        assert!(maintenance.reopen().is_err());
        assert!(!repository_handle.is_open());
        fs::copy(replacement, sandbox.db()).unwrap();
        maintenance.reopen().unwrap();
    }
    assert_eq!(value(&repository_handle), 42);
    handle.checkpoint_and_close().unwrap();
}

fn prepare_rollback(sandbox: &Sandbox, queue_existed: bool) -> PathBuf {
    let rollback = sandbox.0.join("library.duckdb.pre-restore-test");
    fs::write(&rollback, b"original library").unwrap();
    if queue_existed {
        fs::write(queue_path(&rollback), b"original queue").unwrap();
    }
    begin_restore(&sandbox.db(), &rollback, queue_existed).unwrap();
    fs::write(sandbox.db(), b"interrupted replacement").unwrap();
    fs::write(queue_path(&sandbox.db()), b"future queue").unwrap();
    for path in [
        sandbox.0.join("library.duckdb.wal"),
        sandbox.0.join("library.duckdb.analysis-jobs.sqlite3-wal"),
        sandbox.0.join("library.duckdb.analysis-jobs.sqlite3-shm"),
    ] {
        fs::write(path, b"future sidecar").unwrap();
    }
    rollback
}

fn assert_clean_sidecars(db: &Path) {
    for suffix in [".wal", ".analysis-jobs.sqlite3-wal", ".analysis-jobs.sqlite3-shm"] {
        assert!(!PathBuf::from(format!("{}{suffix}", db.display())).exists());
    }
}

#[test]
fn interrupted_restore_recovers_both_original_files_and_removes_future_wals() {
    let sandbox = Sandbox::new();
    let rollback = prepare_rollback(&sandbox, true);
    assert!(recover_pending_restore(&sandbox.db()).unwrap());
    assert_eq!(fs::read(sandbox.db()).unwrap(), b"original library");
    assert_eq!(fs::read(queue_path(&sandbox.db())).unwrap(), b"original queue");
    assert!(rollback.exists(), "rollback evidence remains available");
    assert!(!marker_path(&sandbox.db()).exists());
    assert_clean_sidecars(&sandbox.db());
    assert!(!recover_pending_restore(&sandbox.db()).unwrap());
}

#[test]
fn original_absent_queue_removes_future_jobs() {
    let sandbox = Sandbox::new();
    prepare_rollback(&sandbox, false);
    assert!(recover_pending_restore(&sandbox.db()).unwrap());
    assert!(!queue_path(&sandbox.db()).exists());
    assert_clean_sidecars(&sandbox.db());
}

#[test]
fn missing_rollback_retains_marker_and_current_files_for_retry() {
    let sandbox = Sandbox::new();
    let rollback = prepare_rollback(&sandbox, true);
    fs::remove_file(queue_path(&rollback)).unwrap();
    assert!(matches!(
        recover_pending_restore(&sandbox.db()),
        Err(DbError::MissingRollback)
    ));
    assert_eq!(fs::read(sandbox.db()).unwrap(), b"interrupted replacement");
    assert!(marker_path(&sandbox.db()).exists());
    fs::write(queue_path(&rollback), b"recovered original queue").unwrap();
    assert!(recover_pending_restore(&sandbox.db()).unwrap());
    assert_eq!(
        fs::read(queue_path(&sandbox.db())).unwrap(),
        b"recovered original queue"
    );
}

#[test]
fn malformed_restore_markers_cannot_choose_other_files() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.db(), b"current library").unwrap();
    for name in [
        "../library.duckdb.pre-restore-other",
        "/tmp/library.duckdb.pre-restore-other",
        "unrelated.bak",
    ] {
        let state = serde_json::json!({"rollback": name, "queue_existed": false});
        fs::write(marker_path(&sandbox.db()), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(matches!(
            recover_pending_restore(&sandbox.db()),
            Err(DbError::InvalidRestoreState)
        ));
        assert_eq!(fs::read(sandbox.db()).unwrap(), b"current library");
    }
}

#[test]
fn panicking_writer_rolls_back_before_another_writer_enters() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    initialize(&handle);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), DbError> = handle.write(|conn| {
            conn.execute_batch("BEGIN; UPDATE counter SET n=77")?;
            panic!("injected request panic");
        });
    }));
    assert!(panic.is_err());
    assert_eq!(value(&handle), 0);
    handle
        .write(|conn| {
            conn.execute_batch("BEGIN; UPDATE counter SET n=1; COMMIT")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(value(&handle), 1);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn relative_restore_paths_are_supported() {
    const CHILD: &str = "PLUMDECK_TEST_RELATIVE_RESTORE";
    if std::env::var_os(CHILD).is_some() {
        let db = Path::new("library.duckdb");
        let rollback = Path::new("library.duckdb.pre-restore-test");
        fs::write(rollback, b"original").unwrap();
        begin_restore(db, rollback, false).unwrap();
        fs::write(db, b"replaced").unwrap();
        assert!(recover_pending_restore(db).unwrap());
        assert_eq!(fs::read(db).unwrap(), b"original");
        return;
    }
    let sandbox = Sandbox::new();
    // A separate test process avoids changing cwd under other parallel tests.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "relative_restore_paths_are_supported"])
        .env(CHILD, "1")
        .current_dir(&sandbox.0)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn writer_arriving_during_maintenance_is_rejected_without_waiting_for_writer_lock() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first_handle = handle.clone();
    let first = std::thread::spawn(move || {
        first_handle.write(|_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    entered_rx.recv().unwrap();
    let maintenance_handle = handle.clone();
    let maintenance = std::thread::spawn(move || maintenance_handle.maintenance());
    let started = Instant::now();
    while !matches!(handle.lease_timeout(Duration::ZERO), Err(DbError::Maintenance)) {
        assert!(started.elapsed() < Duration::from_secs(1), "maintenance gate was not reserved");
        std::thread::yield_now();
    }
    let (result_tx, result_rx) = mpsc::channel();
    let second_handle = handle.clone();
    let second = std::thread::spawn(move || {
        result_tx.send(second_handle.write(|_| Ok(()))).unwrap();
    });
    let result = result_rx.recv_timeout(Duration::from_secs(1));
    // Always release the first writer, even if the assertion below fails.
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap();
    drop(maintenance.join().unwrap().unwrap());
    assert!(matches!(result, Ok(Err(DbError::Maintenance))));
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_waits_for_writer_already_queued_on_write_lock() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let (first_entered_tx, first_entered_rx) = mpsc::channel();
    let (first_release_tx, first_release_rx) = mpsc::channel();
    let first_handle = handle.clone();
    let first = std::thread::spawn(move || first_handle.write(|_| {
        first_entered_tx.send(()).unwrap();
        first_release_rx.recv().unwrap();
        Ok(())
    }));
    first_entered_rx.recv().unwrap();
    let held: Vec<_> = (0..POOL_SIZE - 2).map(|_| handle.lease().unwrap()).collect();
    let (second_entered_tx, second_entered_rx) = mpsc::channel();
    let (second_release_tx, second_release_rx) = mpsc::channel();
    let second_handle = handle.clone();
    let second = std::thread::spawn(move || second_handle.write(|_| {
        second_entered_tx.send(()).unwrap();
        second_release_rx.recv().unwrap();
        Ok(())
    }));
    // The first writer, the held leases, and the queued second writer fill
    // the pool. This confirms admission before the maintenance gate is set.
    let started = Instant::now();
    while !matches!(handle.lease_timeout(Duration::ZERO), Err(DbError::PoolTimeout)) {
        assert!(started.elapsed() < Duration::from_secs(1), "second writer was not admitted");
        std::thread::yield_now();
    }
    let (maintenance_tx, maintenance_rx) = mpsc::channel();
    let maintenance_handle = handle.clone();
    let maintenance = std::thread::spawn(move || {
        let guard = maintenance_handle.maintenance();
        maintenance_tx.send(guard.is_ok()).unwrap();
        guard
    });
    let started = Instant::now();
    while !matches!(handle.lease_timeout(Duration::ZERO), Err(DbError::Maintenance)) {
        assert!(started.elapsed() < Duration::from_secs(1), "maintenance gate was not reserved");
        std::thread::yield_now();
    }
    drop(held);
    first_release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    second_entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(maintenance_rx.try_recv().is_err(), "maintenance overtook an accepted writer");
    second_release_tx.send(()).unwrap();
    second.join().unwrap().unwrap();
    assert!(maintenance_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    drop(maintenance.join().unwrap().unwrap());
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn maintenance_wakes_pool_waiters_to_reject_stale_requests() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let held: Vec<_> = (0..POOL_SIZE).map(|_| handle.lease().unwrap()).collect();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let waiting_handle = handle.clone();
    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        result_tx.send(matches!(waiting_handle.lease(), Err(DbError::Maintenance))).unwrap();
    });
    started_rx.recv().unwrap();
    let maintenance_handle = handle.clone();
    let maintenance = std::thread::spawn(move || maintenance_handle.maintenance());
    let result = result_rx.recv_timeout(Duration::from_secs(1));
    // The pool remains full until the waiter has had a chance to observe the gate.
    drop(held);
    waiter.join().unwrap();
    drop(maintenance.join().unwrap().unwrap());
    assert_eq!(result.unwrap(), true);
    handle.checkpoint_and_close().unwrap();
}

// backend/tests/workflow_integrity/test_integrity.py (restore gate and crash recovery)

#[test]
fn restore_gate_rejects_active_connection() {
    let sandbox = Sandbox::new();
    let handle = DbHandle::open(&sandbox.db()).unwrap();
    let session = handle.lease().unwrap();
    session.execute_batch("SELECT 1").unwrap();
    let error = handle.maintenance().map(drop).unwrap_err();
    assert_eq!(error.to_string(), "データベースを使用中です。処理の完了後に再試行してください");
    drop(session);
    drop(handle.maintenance().unwrap());
    let activity = handle.activity();
    let error = handle.maintenance().map(drop).unwrap_err();
    assert_eq!(error.to_string(), "解析・取り込みを停止してからバックアップ／復元してください");
    drop(activity);
    let maintenance = handle.maintenance().unwrap();
    assert_eq!(handle.lease().map(drop).unwrap_err().to_string(), "バックアップ／復元中です。完了後に再試行してください");
    drop(maintenance);
    handle.checkpoint_and_close().unwrap();
}

#[test]
fn crash_between_database_replacements_recovers_original_pair() {
    use plumdeck_core::time::SystemClock;
    use plumdeck_db::jobs_sqlite::{snapshot_sqlite, AnalysisJobQueue};

    let sandbox = Sandbox::new();
    let db = sandbox.db();
    let handle = DbHandle::open(&db).unwrap();
    handle
        .lease()
        .unwrap()
        .execute_batch("CREATE TABLE tracks (id INTEGER, title VARCHAR); INSERT INTO tracks VALUES (1, 'Song')")
        .unwrap();
    handle.checkpoint_and_close().unwrap();
    let rollback = sandbox.0.join("library.duckdb.pre-restore-crash-test");
    fs::copy(&db, &rollback).unwrap();
    let queue = AnalysisJobQueue::for_library(&handle, Arc::new(SystemClock));
    let job = queue.create(&serde_json::json!({"generation": "original"}), &[]).unwrap();
    snapshot_sqlite(queue.path(), &queue_path(&rollback)).unwrap();
    begin_restore(&db, &rollback, true).unwrap();
    fs::write(&db, b"interrupted replacement").unwrap();
    for suffix in ["", "-wal", "-shm"] {
        fs::write(format!("{}{suffix}", queue.path().display()), b"interrupted queue replacement").unwrap();
    }
    assert!(recover_pending_restore(&db).unwrap());
    handle.maintenance().unwrap().reopen().unwrap();
    let title: String = handle.lease().unwrap().query_row("SELECT title FROM tracks", [], |row| row.get(0)).unwrap();
    assert_eq!(title, "Song");
    assert_eq!(queue.get(Some(&job)).unwrap()["config"]["generation"], "original");
    assert!(!marker_path(&db).exists());
    handle.checkpoint_and_close().unwrap();
}
