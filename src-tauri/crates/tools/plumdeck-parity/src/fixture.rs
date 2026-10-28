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
    /// A v5 library with varied tracks, genres, lyrics and setlists for the
    /// dashboard statistics (S2): excluded and case-variant genres, tied
    /// counts, NULL flags, tiny and f32-inexact durations, microseconds.
    Catalog,
    /// A library from a newer app (schema v6). Python opens it; Rust refuses
    /// it untouched (deviation D-14). Run on side B only.
    NewerSchema,
    /// The S3 library: tracks whose files are the synthetic media
    /// ([`crate::media`]) with analyzed and unanalyzed rows, enharmonic and
    /// Camelot key spellings, genre spelling variants, half/double-time
    /// BPMs, tied timestamps, Unicode titles and NFD paths, lyrics with LRC
    /// tags. Every analyzed track has a valid embedding, so startup queues
    /// no backfill job.
    Library,
    /// The S4 library: [`Case::Library`] with setlists (duplicates, a
    /// missing file, overlaps), wordplay pairs (approved, tested, pending,
    /// approvable or not) and distinct embeddings in three spaces whose
    /// components are small integers, so every dot product and norm is
    /// exact and numpy agrees with the port bit for bit.
    Sets,
}

impl Case {
    pub const ALL: [Case; 10] = [
        Case::Fresh,
        Case::Populated,
        Case::UpgradedLayout,
        Case::PendingRestore,
        Case::PendingWal,
        Case::Unreadable,
        Case::NewerSchema,
        Case::Catalog,
        Case::Library,
        Case::Sets,
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
            Case::Catalog => "catalog",
            Case::Library => "library",
            Case::Sets => "sets",
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

const CATALOG_ROWS: &str = "
INSERT INTO schema_info VALUES ('version', '5');
INSERT INTO settings VALUES ('root_path', '/music/日本'), ('plumdeck.deckCount', '4');
INSERT INTO tracks (filepath, title, genre, bpm, is_genre_verified, created_at) VALUES
  ('/catalog/01.mp3', 'T01', 'House', 124.0, true, '2026-02-01 00:00:00'),
  ('/catalog/02.mp3', 'T02', 'House', 0.0, false, '2026-02-01 00:00:01'),
  ('/catalog/03.mp3', 'T03', 'house', 126.5, NULL, '2026-02-01 00:00:02'),
  ('/catalog/04.mp3', 'T04', 'Techno', NULL, false, '2026-02-01 00:00:03'),
  ('/catalog/05.mp3', 'T05', 'Techno', -1.0, false, '2026-02-01 00:00:04'),
  ('/catalog/06.mp3', 'T06', 'Unknown', 128.0, false, '2026-02-01 00:00:05'),
  ('/catalog/07.mp3', 'T07', '', 128.0, false, '2026-02-01 00:00:06'),
  ('/catalog/08.mp3', 'T08', NULL, 0.001, false, '2026-02-01 00:00:07'),
  ('/catalog/09.mp3', 'T09', 'J-Pop 日本', 99.9, true, '2026-02-01 00:00:08'),
  ('/catalog/10.mp3', 'T10', 'unknown', 100.0, false, '2026-02-01 00:00:09'),
  ('/catalog/11.mp3', 'T11', 'Drum & Bass', 174.0, true, '2026-02-01 00:00:10'),
  ('/catalog/12.mp3', 'T12', 'Drum & Bass', 172.0, true, '2026-02-01 00:00:11'),
  ('/catalog/13.mp3', 'T13', 'Drum & Bass', 0.0, true, '2026-02-01 00:00:12');
INSERT INTO lyrics (track_id, content, source) VALUES
  (1, 'la la', 'user'), (2, '', 'user'), (3, NULL, 'lrclib'), (9, '歌詞', 'user');
INSERT INTO setlists (name, description, display_order, genre, target_duration, rating, created_at, updated_at) VALUES
  ('Oldest', NULL, 0, NULL, 3600.0, NULL, '2026-01-01 00:00:00', '2026-01-01 00:00:00'),
  ('Inexact', 'f32 duration', 1, 'House', 1234.56, 5, '2026-01-02 00:00:00', '2026-03-01 12:00:00.000120'),
  ('Tie A', NULL, 2, NULL, NULL, 1, '2026-01-03 00:00:00', '2026-03-02 00:00:00'),
  ('Tie B', '説明', 3, 'Techno', 0.5, 2, '2026-01-04 00:00:00', '2026-03-02 00:00:00'),
  ('Tiny', NULL, NULL, NULL, 1e-7, NULL, '2026-01-05 00:00:00', '2026-03-03 00:00:00.5'),
  ('Newest', NULL, 4, NULL, 60.25, 3, NULL, '2026-03-04 00:00:00'),
  ('Never updated', NULL, 5, NULL, NULL, NULL, '2026-01-06 00:00:00', NULL);
";

/// The S3 library rows; `{media}` is the media directory.
const LIBRARY_ROWS: &str = r#"
INSERT INTO schema_info VALUES ('version', '5');
INSERT INTO settings VALUES ('root_path', '{media}');
INSERT INTO tracks (filepath, title, artist, album, genre, subgenre, year, bpm, key, scale, duration, analysis_level,
                    energy, danceability, brightness, noisiness, is_genre_verified, created_at) VALUES
  ('{media}/id3v23.mp3', 'Alpha Song', 'DJ Alpha', 'First Album', 'House', 'Deep House', 1999, 124.0, 'F minor', 'minor', 300.5, NULL, 0.5, 0.6, 0.7, 0.1, true, '2026-03-01 10:00:00'),
  ('{media}/日本語 ß.mp3', '日本語のタイトル', 'アーティスト ß', NULL, 'J-Pop', '', NULL, 62.0, '4A', 'minor', 200.25, 'light', 0.3, 0.4, 0.5, 0.2, false, '2026-03-01 10:00:00'),
  ('{media}/vorbis.flac', 'Flac Title', 'Flac Artist', 'Flac Album', 'house', 'deep house', 0, 128.0, 'Eb Minor', 'minor', 250.0, 'full', 0.0, 0.0, 0.0, 0.2, true, '2026-03-02 09:00:00'),
  ('{media}/tagged.m4a', 'M4A Title', 'M4A Artist', 'M4A Album', 'Drum & Bass', 'Liquid', 2010, 174.0, 'C major', 'major', 180.0, NULL, 0.9, 0.8, 0.6, 0.3, true, '2026-03-03 08:00:00.250000'),
  ('{media}/plain.wav', 'Plain Wave', 'Wave Artist', '', 'drum and bass', 'liquid', 2011, 172.0, '', '', 0.1, NULL, 0.2, 0.2, 0.2, 0.2, true, '2026-03-03 08:00:00.250000'),
  ('{media}/id3v1.mp3', 'Old Title', 'Old Artist', 'Old Album', 'DnB', 'Neurofunk', 1985, 170.0, '8A', 'minor', 400.0, NULL, 0.7, 0.7, 0.7, 0.7, false, '2026-03-04 00:00:00'),
  ('{media}/cafe' || chr(769) || ' nfd.mp3', 'Café NFD', 'Zoë', 'Été', 'Unknown', '', NULL, 0.0, '', '', 120.0, NULL, 0.0, 0.0, 0.0, 0.0, false, '2026-03-04 00:00:01'),
  ('{media}/noise.mp3', '100% Pure_Noise', 'DJ_Test', 'Noise LP', 'Hip Hop', 'Trap', 2020, 140.5, 'G# Minor', 'minor', 90.0, NULL, 0.5, 0.5, 0.5, 0.5, true, '2026-03-05 12:00:00'),
  ('{media}/empty.mp3', 'Straße', 'ẞ Artist', 'Ｆｕｌｌ１２３', 'hip-hop', 'trap', 2021, 70.25, 'Ab minor', 'minor', 95.5, NULL, 0.4, 0.4, 0.4, 0.4, true, '2026-03-05 12:00:01'),
  ('{media}/missing.mp3', 'Missing File', 'Ghost', NULL, 'Techno', 'Peak Time', 2015, 130.0, 'A minor', 'minor', 360.0, NULL, 0.8, 0.8, 0.8, 0.8, true, '2026-03-06 00:00:00'),
  ('{media}/untagged.flac', 'rock''n''roll don''t', 'R&B Group', 'Soul', 'R&B', 'Contemporary R&B', 2005, 95.0, '10a', 'minor', 210.0, NULL, 0.5, 0.5, 0.5, 0.5, false, '2026-03-07 00:00:00'),
  ('{media}/UPPER.MP3', 'Upper Case', 'Loud', 'CAPS', 'R and B', 'contemporary r&b', 2006, 96.0, 'B minor', 'minor', 205.0, NULL, 0.5, 0.5, 0.5, 0.5, true, '2026-03-07 00:00:00'),
  ('{media}/folder.mp3', 'Folder', 'Nobody', NULL, '', '', NULL, NULL, '', '', 1.0, NULL, 0.0, 0.0, 0.0, 0.0, false, '2026-03-08 00:00:00');
INSERT INTO track_analyses (track_id, waveform_u8, beats_f32, features_extra_json) VALUES
  (1, '\x00\x7f\xff'::BLOB, NULL, '{"analysis_components": {"timbre": "t1", "rhythm": "r1"}}'),
  (2, '\x10\x20'::BLOB, '\x00\x00\x80\x3f'::BLOB, '{"analysis_components": {"rhythm": "numpy-autocorrelation-light-v3"}}'),
  (3, '\x01'::BLOB, NULL, '{}'),
  (4, '\x05'::BLOB, NULL, '{"analysis_components": {}}'),
  (5, ''::BLOB, NULL, '{}'),
  (6, '\x40'::BLOB, NULL, 'not json'),
  (8, '\x80\x80'::BLOB, NULL, '{}'),
  (9, '\x01'::BLOB, NULL, '{}'),
  (11, '\x02'::BLOB, NULL, '{}'),
  (12, '\x03'::BLOB, NULL, '{}');
INSERT INTO track_embeddings (track_id, model_name, embedding_json)
  SELECT id, 'msd-musicnn-1:musicnn-16khz-v1', (SELECT list_transform(range(200), x -> 0.25)::VARCHAR) FROM tracks;
INSERT INTO lyrics (track_id, content, source, language, created_at, updated_at) VALUES
  (1, '[00:12.50]Into the night' || chr(10) || '[00:15.00]We dance tonight' || chr(10) || 'Outro line', 'user', 'en', '2026-03-01 10:00:00', '2026-03-02 11:00:00.5'),
  (2, '夜に駆ける' || chr(10) || '夜のうた', 'lrclib', 'ja', '2026-03-01 10:00:00', '2026-03-01 10:00:00'),
  (4, '   ', 'user', NULL, '2026-03-03 08:00:00', '2026-03-03 08:00:00'),
  (6, '', 'user', NULL, '2026-03-04 00:00:00', '2026-03-04 00:00:00'),
  (8, 'NIGHT moves [chorus]' || chr(10) || '[1:02]night two' || chr(10) || '[123:45.6] tail', NULL, NULL, '2026-03-05 12:00:00', '2026-03-05 12:00:00'),
  (11, 'a_b 100% line', 'user', NULL, NULL, NULL);
"#;

/// Padding to 200 dimensions for a 4-component vector.
const PAD: &str = "list_transform(range(196), x -> 0.0::DOUBLE)";

/// The S4 rows over [`LIBRARY_ROWS`].
fn sets_rows() -> String {
    let vectors = [
        (1, "[4,1,0,0]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (2, "[3,2,0,1]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (3, "[4,0,1,0]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (4, "[0,1,4,2]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (5, "[1,1,3,2]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (6, "[0,0,0,0]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (7, "[2,2,2,2]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (8, "[2,3,1,0]", "'msd-musicnn-1:musicnn-16khz-v1'"),
        (11, "[1,0,0,1]", "NULL"),
        (12, "[1,1,0,1]", "'musicnn'"),
        (13, "[0,0,1,1]", "''"),
    ];
    let mut sql = String::new();
    for (id, components, model) in vectors {
        sql.push_str(&format!(
            "UPDATE track_embeddings SET model_name = {model}, \
             embedding_json = list_concat({components}::DOUBLE[], {PAD})::VARCHAR WHERE track_id = {id};\n"
        ));
    }
    sql.push_str(SETS_ROWS);
    sql
}

const SETS_ROWS: &str = r#"
-- Tracks whose vector startup would re-queue (S11) are left unanalyzed.
DELETE FROM track_analyses WHERE track_id IN (6, 9, 11, 12);
UPDATE track_embeddings SET embedding_json = 'not json' WHERE track_id = 9;
UPDATE track_embeddings SET model_name = 'short-model', embedding_json = '[1, 2, 3]' WHERE track_id = 10;
UPDATE tracks SET genre = 'R&B', subgenre = 'Contemporary R&B' WHERE id = 12;
INSERT INTO setlists (name, description, display_order, genre, target_duration, rating, created_at, updated_at) VALUES
  ('Warm-up', 'early', 0, 'House', 3600.5, 4, '2026-04-01 10:00:00', '2026-04-02 10:00:00'),
  ('Peak/Time: "set"?', NULL, 1, NULL, 1234.56, NULL, '2026-04-01 11:00:00', '2026-04-03 10:00:00.5'),
  ('空のセット', NULL, 2, NULL, NULL, NULL, '2026-04-01 12:00:00', '2026-04-01 12:00:00');
INSERT INTO setlist_tracks (setlist_id, track_id, position, transition_note, wordplay_json, in_ms, out_ms, playback_rate,
                            extra_duration_ms, overlap_next_ms, revision, created_at) VALUES
  (1, 1, 0, NULL, NULL, 0, NULL, 1, 0, 5000, 1, '2026-04-02 10:00:00'),
  (1, 3, 1, 'blend', '{"pair_id": 1}', 1000, 60000, 1.0, 0, 0, 2, '2026-04-02 10:00:00'),
  (1, 10, 2, NULL, NULL, 0, NULL, 1.05, 2000, 0, 1, '2026-04-02 10:00:00'),
  (1, 1, 3, NULL, NULL, 0, 90000, 1, 0, 0, 3, '2026-04-02 10:00:00'),
  (2, 2, 0, NULL, NULL, 0, NULL, 1, 0, 0, 1, '2026-04-03 10:00:00'),
  (2, 4, 1, NULL, NULL, 0, NULL, 1, 0, 0, 1, '2026-04-03 10:00:00');
INSERT INTO wordplay_pairs (from_track_id, to_track_id, keyword, normalized_keyword, source_phrase, target_phrase,
                            source_section_position, target_section_position, source_cue_mode, from_timestamp,
                            source_cue_end_timestamp, to_timestamp, target_intro_timestamp, target_landing_timestamp,
                            transition_notes, source_url, evidence_type, verification_status, status, created_at, updated_at) VALUES
  (1, 3, 'Night', 'night', 'into the night', 'night moves', 'end', 'start', 'section_end', 10.0, NULL, 20.0, 5.0, NULL,
   'tested at home', 'https://example.invalid/a', 'performance', 'tested', 'approved', '2026-04-05 00:00:00', '2026-04-05 00:00:00'),
  (1, 3, 'Dance', 'dance', 'we dance', 'dance on', 'end', 'start', 'section_end', 12.0, NULL, 22.0, 6.0, 22.0,
   '', '', 'hypothesis', 'unverified', 'approved', '2026-04-05 00:00:01', '2026-04-05 00:00:01'),
  (11, 12, 'Soul', 'soul', 'soul line', 'soul again', 'end', 'start', 'section_end', 100.0, NULL, 30.0, 5.0, NULL,
   '', '', 'hypothesis', 'unverified', 'pending', '2026-04-05 00:00:02', '2026-04-05 00:00:02'),
  (3, 1, 'Moves', 'moves', 'night moves', 'into the night', 'end', 'start', 'section_end', 10.0, NULL, 20.0, 5.0, 20.0,
   'tempo too far', 'http://example.invalid/b', 'edit_listing', 'unverified', 'pending', '2026-04-05 00:00:03', '2026-04-05 00:00:03'),
  (4, 5, 'Bass', 'bass', 'bass drop', 'bass line', 'middle', 'middle', 'cue_drumming', 30.0, 40.0, 50.0, 45.0, 50.0,
   '', '', 'hypothesis', 'unverified', 'pending', '2026-04-05 00:00:04', '2026-04-05 00:00:04'),
  (2, 8, 'Yoru', 'yoru', '夜に駆ける', '夜のうた', 'unknown', 'unknown', 'section_end', NULL, NULL, NULL, NULL, NULL,
   '', '', 'hypothesis', 'unverified', 'approved', '2026-04-05 00:00:05', '2026-04-05 00:00:05');
"#;

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
/// `media` is where the scenario's synthetic audio lives (used by
/// [`Case::Library`], whose paths point into it).
pub fn build(case: Case, dir: &Path, media: &Path) -> Result<()> {
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
        Case::Catalog => {
            let conn = Connection::open(&db)?;
            plumdeck_db::create_schema_v5(&conn)?;
            conn.execute_batch(CATALOG_ROWS).context("catalog rows")?;
            conn.execute_batch("CHECKPOINT")?;
        }
        Case::NewerSchema => {
            build_library(&db, false)?;
            Connection::open(&db)?.execute_batch("UPDATE schema_info SET value = '6'; CHECKPOINT")?;
        }
        Case::Library | Case::Sets => {
            crate::media::write(media)?;
            let conn = Connection::open(&db)?;
            plumdeck_db::create_schema_v5(&conn)?;
            conn.execute_batch(&with_media(LIBRARY_ROWS, media)).context("library rows")?;
            if case == Case::Sets {
                conn.execute_batch(&sets_rows()).context("sets rows")?;
            }
            conn.execute_batch("CHECKPOINT")?;
        }
    }
    Ok(())
}

/// `text` with `{media}` replaced by the media directory (SQL-quoted).
pub fn with_media(text: &str, media: &Path) -> String {
    text.replace("{media}", &media.to_string_lossy().replace('\'', "''"))
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
            build(case, &dir, &root.join("media")).unwrap();
            let db = dir.join(DB_NAME);
            match case {
                Case::Fresh => assert!(!db.exists()),
                Case::PendingRestore => {
                    assert!(plumdeck_db::restore_recovery::marker_path(&db).exists());
                    assert_eq!(std::fs::read(&db).unwrap(), b"interrupted replacement");
                }
                Case::PendingWal => assert!(dir.join(format!("{DB_NAME}.wal")).exists()),
                Case::Unreadable => assert!(db.exists()),
                Case::Catalog | Case::Library | Case::Sets => assert!(db.exists() && !queue_of(&db).exists()),
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
