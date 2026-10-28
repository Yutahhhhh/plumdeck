//! Synthetic libraries for the startup scenario (S1). Nothing here reads the
//! owner's data: every file is built from contracts/schema/v5.sql and fixed
//! rows, so the cases are deterministic and safe to commit to.
//!
//! None of them may trigger Python's startup compaction (schema below v5,
//! legacy analysis columns or a file over 256 MB), which is not ported yet.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use duckdb::Connection;
use plumdeck_db::rusqlite;
use plumdeck_db::schema::COMPATIBILITY_STATEMENTS;

/// A startup case: how to build the master copy both sides start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// No library yet: both sides create the v5 file.
    Fresh,
    /// A v5 library with rows that startup rewrites (retired LLM settings,
    /// interrupted imports) and a job queue with running work (recovered
    /// lazily, so untouched by startup).
    Populated,
    /// Populated, in the layout of an older v5 build: the compatibility
    /// columns are missing, so startup ALTERs append them.
    UpgradedLayout,
    /// A restore was interrupted between the two file replacements: both
    /// sides must put the rollback pair back before opening.
    PendingRestore,
    /// The last writes are still in the WAL (the app was killed).
    PendingWal,
    /// Not a DuckDB file: both sides fail startup with the same report.
    Unreadable,
    /// A library from a newer app (schema v6). Python opens it; Rust refuses
    /// it untouched (deviation D-14). Run on side B only.
    NewerSchema,
}

impl Case {
    pub const ALL: [Case; 7] = [
        Case::Fresh,
        Case::Populated,
        Case::UpgradedLayout,
        Case::PendingRestore,
        Case::PendingWal,
        Case::Unreadable,
        Case::NewerSchema,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Case::Fresh => "fresh",
            Case::Populated => "populated",
            Case::UpgradedLayout => "upgraded-layout",
            Case::PendingRestore => "pending-restore",
            Case::PendingWal => "pending-wal",
            Case::Unreadable => "unreadable",
            Case::NewerSchema => "newer-schema",
        }
    }
}

pub const DB_NAME: &str = "plumdeck.duckdb";

fn queue_of(db: &Path) -> PathBuf {
    plumdeck_db::restore_recovery::queue_path(db)
}

const ROWS: &str = "
INSERT INTO schema_info VALUES ('version', '5');
INSERT INTO settings VALUES
  ('llm_provider', 'openai'), ('OpenAI_API_Key', 'sk-test'), (' LLM_model_experimental ', 'x'),
  ('codex_timeout_seconds', '30'), ('root_path', '/music'), ('ui.theme', 'dark'), ('mistral_api_key', 'kept');
INSERT INTO tracks (filepath, title, artist, genre, bpm, duration, created_at) VALUES
  ('/fixture/a.mp3', 'Alpha', 'DJ A', 'House', 124.0, 300.5, '2026-01-02 03:04:05'),
  ('/fixture/b.mp3', 'Beta ß', 'DJ B', 'Techno', 131.5, 280.25, '2026-01-02 03:04:06.5');
INSERT INTO import_batches (id, request_id, target_kind, target_id, state, paused, created_at, updated_at) VALUES
  ('batch-queued', 'req-1', 'library', NULL, 'queued', false, '2026-01-01 00:00:00', '2026-01-01 00:00:01'),
  ('batch-processing', 'req-2', 'setlist', 7, 'processing', false, '2026-01-01 00:00:00', '2026-01-01 00:00:01'),
  ('batch-pausing', 'req-3', 'library', NULL, 'pausing', false, '2026-01-01 00:00:00', '2026-01-01 00:00:01'),
  ('batch-completed', 'req-4', 'library', NULL, 'completed', false, '2026-01-01 00:00:00', '2026-01-01 00:00:01'),
  ('batch-failed', 'req-5', 'library', NULL, 'failed', false, '2026-01-01 00:00:00', '2026-01-01 00:00:01');
INSERT INTO import_items (id, batch_id, input_order, source_path, state, attempts) VALUES
  ('item-probing', 'batch-processing', 0, '/drop/1.mp3', 'probing', 1),
  ('item-analyzing', 'batch-processing', 1, '/drop/2.mp3', 'analyzing', 2),
  ('item-queued', 'batch-processing', 2, '/drop/3.mp3', 'queued', 0),
  ('item-done', 'batch-completed', 0, '/drop/4.mp3', 'completed', 1),
  ('item-failed', 'batch-failed', 0, '/drop/5.mp3', 'failed', 3);
INSERT INTO wordplay_pairs (from_track_id, to_track_id, keyword, normalized_keyword, source_phrase, target_phrase, status, created_at, updated_at)
  VALUES (1, 2, 'Night', 'night', 'into the night', 'night falls', 'approved', '2026-01-03 00:00:00', '2026-01-03 00:00:00');
INSERT INTO setlists (name, created_at, updated_at) VALUES ('Fixture set', '2026-01-04 00:00:00', '2026-01-04 00:00:00');
INSERT INTO setlist_tracks (setlist_id, track_id, position, created_at) VALUES (1, 1, 0, '2026-01-04 00:00:00'), (1, 2, 1, '2026-01-04 00:00:00');
INSERT INTO recordings (recording_key, filepath, started_at) VALUES ('rec-1', '/fixture/rec.wav', '2026-01-05 00:00:00');
";

fn build_library(path: &Path, upgraded_layout: bool) -> Result<()> {
    let conn = Connection::open(path)?;
    plumdeck_db::create_schema_v5(&conn)?;
    if upgraded_layout {
        for statement in COMPATIBILITY_STATEMENTS {
            // ALTER TABLE <table> ADD COLUMN IF NOT EXISTS <column> ...
            let words: Vec<&str> = statement.split_whitespace().collect();
            conn.execute_batch(&format!("ALTER TABLE {} DROP COLUMN {}", words[2], words[8]))?;
        }
    }
    conn.execute_batch(ROWS).context("fixture rows")?;
    conn.execute_batch("CHECKPOINT")?;
    Ok(())
}

fn build_queue(path: &Path) -> Result<()> {
    let conn = rusqlite::Connection::open(path)?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    conn.execute_batch(plumdeck_db::jobs_sqlite::CREATE_JOBS)?;
    conn.execute_batch(plumdeck_db::jobs_sqlite::CREATE_ITEMS)?;
    conn.execute_batch(
        r#"INSERT INTO jobs VALUES ('0123456789abcdef0123456789abcdef', 'running',
             '{"features": ["rhythm"], "only_outdated": true, "workers": 2}', 1767225600.5, 1767225601.25, 12.5, NULL);
           INSERT INTO jobs VALUES ('fedcba9876543210fedcba9876543210', 'pausing', '{"features": ["key"]}', 1767225500.0, 1767225500.0, 0, NULL);
           INSERT INTO items VALUES ('0123456789abcdef0123456789abcdef', 1, '/fixture/a.mp3', 'completed', 3.5, NULL);
           INSERT INTO items VALUES ('0123456789abcdef0123456789abcdef', 2, '/fixture/b.mp3', 'running', 0, NULL);
           INSERT INTO items VALUES ('fedcba9876543210fedcba9876543210', 1, '/fixture/a.mp3', 'failed', 1.0, 'boom');"#,
    )?;
    // Fold the WAL so the copy is self-contained.
    let _: (i64, i64, i64) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(())
}

/// Build `case` into the empty directory `dir` (the side's USER_DATA_DIR).
pub fn build(case: Case, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let db = dir.join(DB_NAME);
    match case {
        Case::Fresh => {}
        Case::Populated => {
            build_library(&db, false)?;
            build_queue(&queue_of(&db))?;
        }
        Case::UpgradedLayout => {
            build_library(&db, true)?;
            build_queue(&queue_of(&db))?;
        }
        Case::PendingRestore => {
            let rollback = dir.join(format!("{DB_NAME}.pre-restore-20260101-000000"));
            build_library(&rollback, false)?;
            build_queue(&queue_of(&rollback))?;
            plumdeck_db::restore_recovery::begin_restore(&db, &rollback, true)?;
            // The crash left a half-installed replacement pair behind.
            std::fs::write(&db, b"interrupted replacement")?;
            std::fs::write(format!("{}.wal", db.display()), b"future wal")?;
            let queue = queue_of(&db);
            std::fs::write(&queue, b"interrupted queue")?;
            std::fs::write(format!("{}-wal", queue.display()), b"future queue wal")?;
        }
        Case::PendingWal => {
            build_library(&db, false)?;
            let staged = dir.join("staged");
            std::fs::create_dir_all(&staged)?;
            {
                let conn = Connection::open(&db)?;
                conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown; SET wal_autocheckpoint = '1TB';")?;
                conn.execute_batch(
                    "INSERT INTO settings VALUES ('anthropic_model', 'in-wal'), ('wal.only', 'yes');
                     UPDATE import_items SET state = 'probing' WHERE id = 'item-queued';
                     INSERT INTO tracks (filepath, title, created_at) VALUES ('/fixture/c.mp3', 'Gamma', '2026-01-06 00:00:00');",
                )?;
                // Copy while the writer is still open: what a killed process leaves.
                std::fs::copy(&db, staged.join(DB_NAME))?;
                std::fs::copy(format!("{}.wal", db.display()), staged.join(format!("{DB_NAME}.wal")))?;
            }
            std::fs::rename(staged.join(DB_NAME), &db)?;
            std::fs::rename(staged.join(format!("{DB_NAME}.wal")), format!("{}.wal", db.display()))?;
            std::fs::remove_dir(&staged)?;
            anyhow::ensure!(Path::new(&format!("{}.wal", db.display())).exists(), "fixture lost its WAL");
        }
        Case::Unreadable => std::fs::write(&db, b"not a duckdb file\n")?,
        Case::NewerSchema => {
            build_library(&db, false)?;
            Connection::open(&db)?.execute_batch("UPDATE schema_info SET value = '6'; CHECKPOINT")?;
        }
    }
    Ok(())
}

/// Copy every file of `from` into `to` (flat directories only).
pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_builds_its_library() {
        let root = std::env::temp_dir().join(format!("plumdeck-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for case in Case::ALL {
            let dir = root.join(case.name());
            build(case, &dir).unwrap();
            let db = dir.join(DB_NAME);
            match case {
                Case::Fresh => assert!(!db.exists()),
                Case::PendingRestore => {
                    assert!(plumdeck_db::restore_recovery::marker_path(&db).exists());
                    assert_eq!(std::fs::read(&db).unwrap(), b"interrupted replacement");
                }
                Case::PendingWal => assert!(dir.join(format!("{DB_NAME}.wal")).exists()),
                Case::Unreadable => assert!(db.exists()),
                Case::Populated | Case::UpgradedLayout | Case::NewerSchema => {
                    let conn = plumdeck_db::open_read_only(&db).unwrap();
                    let version = plumdeck_db::schema::read_schema_version(&conn).unwrap();
                    assert_eq!(version, if case == Case::NewerSchema { 6 } else { 5 }, "{}", case.name());
                    assert!(queue_of(&db).exists() || case == Case::NewerSchema);
                }
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
