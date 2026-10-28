//! The pinned engine is DuckDB 1.5.5, and contracts/schema/v5.sql recreates
//! exactly the catalog recorded in v5.columns.json (both dumped from a DB the
//! Python backend initialized).
//!
//! v5.columns.json ordinals describe a fresh file only. Libraries upgraded by
//! init_db have ALTER-added columns appended (v5.columns.migrated.json), so
//! code reading real files must address columns by name.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plumdeck_db::duckdb::Connection;
use plumdeck_db::schema::{check_file_version, init_schema, read_schema_version};
use plumdeck_db::{create_schema_v5, engine_version, ensure_engine_version, open, DbError, EXPECTED_DUCKDB_VERSION};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/schema").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn contract() -> Value {
    load("v5.columns.json")
}

fn column_names(conn: &Connection) -> BTreeMap<String, Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT table_name, column_name FROM duckdb_columns()
             WHERE database_name = current_database() AND schema_name = 'main'
             ORDER BY table_name, column_index",
        )
        .unwrap();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap() {
        let (table, column) = row.unwrap();
        out.entry(table).or_default().push(column);
    }
    out
}

#[test]
fn upgraded_libraries_append_alter_added_columns() {
    // Rebuild the oldest upgraded layout the way extract_contracts.py did:
    // drop every ALTER-added column from a fresh file, then replay the ALTERs.
    let migrated = load("v5.columns.migrated.json");
    let conn = Connection::open_in_memory().unwrap();
    create_schema_v5(&conn).unwrap();
    let fresh = column_names(&conn);
    let statements: Vec<&str> =
        migrated["alter_statements"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    for statement in &statements {
        let words: Vec<&str> = statement.split_whitespace().collect();
        // ALTER TABLE <table> ADD COLUMN IF NOT EXISTS <column> ...
        conn.execute_batch(&format!("ALTER TABLE {} DROP COLUMN {}", words[2], words[8])).unwrap();
    }
    for statement in &statements {
        conn.execute_batch(statement).unwrap();
    }
    let upgraded = column_names(&conn);
    let listed = migrated["tables"].as_object().unwrap();
    for (table, columns) in &upgraded {
        let want: Vec<String> = match listed.get(table) {
            Some(order) => order.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect(),
            None => fresh[table].clone(),
        };
        assert_eq!(columns, &want, "upgraded column order of {table}");
    }
    assert!(listed.keys().all(|table| fresh[table] != upgraded[table]), "listed tables differ from fresh");
}

#[test]
fn linked_engine_is_duckdb_1_5_5() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(engine_version(&conn).unwrap(), EXPECTED_DUCKDB_VERSION);
    let pragma: String = conn.query_row("PRAGMA version", [], |row| row.get(0)).unwrap();
    assert_eq!(pragma, "v1.5.5");
    ensure_engine_version(&conn).unwrap();
    assert_eq!(contract()["duckdb_version"], EXPECTED_DUCKDB_VERSION);
}

type ColumnRow = (String, i64, String, bool, Option<String>);

#[test]
fn v5_sql_recreates_every_table_and_column() {
    let dir = std::env::temp_dir().join(format!("plumdeck-db-schema-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, build) in [
        ("fresh.duckdb", create_schema_v5 as fn(&Connection) -> Result<(), plumdeck_db::DbError>),
        ("init.duckdb", init_schema),
    ] {
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let conn = open(&path).unwrap();
        build(&conn).unwrap();
        assert_matches_contract(&conn);
    }
    std::fs::remove_dir_all(&dir).ok();
}

fn assert_matches_contract(conn: &Connection) {

    let mut stmt = conn
        .prepare(
            "SELECT table_name, column_name, column_index, data_type, is_nullable, column_default
             FROM duckdb_columns()
             WHERE database_name = current_database() AND schema_name = 'main'
             ORDER BY table_name, column_index",
        )
        .unwrap();
    let mut actual: BTreeMap<String, Vec<ColumnRow>> = BTreeMap::new();
    for row in stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
            ))
        })
        .unwrap()
    {
        let (table, column) = row.unwrap();
        actual.entry(table).or_default().push(column);
    }

    let contract = contract();
    let tables = contract["tables"].as_object().unwrap();
    assert_eq!(tables.len(), 31);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        {
            let mut names: Vec<&String> = tables.keys().collect();
            names.sort();
            names
        },
        "table set"
    );
    for (table, spec) in tables {
        let expected: Vec<ColumnRow> = spec["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().to_owned(),
                    c["ordinal"].as_i64().unwrap(),
                    c["type"].as_str().unwrap().to_owned(),
                    c["nullable"].as_bool().unwrap(),
                    c["default"].as_str().map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(actual[table], expected, "columns of {table}");

        let pk: Vec<String> = conn
            .query_row(
                "SELECT COALESCE(array_to_string(constraint_column_names, ','), '') FROM duckdb_constraints()
                 WHERE database_name = current_database() AND table_name = ?
                   AND constraint_type = 'PRIMARY KEY'",
                [table],
                |row| row.get::<_, String>(0),
            )
            .map(|joined| joined.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let want: Vec<String> = spec["primary_key"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(pk, want, "primary key of {table}");
    }

    for sequence in contract["sequences"].as_array().unwrap() {
        let name = sequence["name"].as_str().unwrap();
        let (start, increment): (i64, i64) = conn
            .query_row(
                "SELECT start_value, increment_by FROM duckdb_sequences()
                 WHERE database_name = current_database() AND sequence_name = ?",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(start, sequence["start_value"].as_i64().unwrap(), "{name}");
        assert_eq!(increment, sequence["increment_by"].as_i64().unwrap(), "{name}");
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("plumdeck-schema-{name}-{}", std::process::id()));
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

// backend/tests/test_wordplay.py::test_existing_v4_wordplay_table_gets_unknown_boundary_defaults
#[test]
fn existing_v4_wordplay_table_gets_unknown_boundary_defaults() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE SEQUENCE seq_wordplay_pairs_id START 2;
         CREATE TABLE wordplay_pairs (
            id INTEGER PRIMARY KEY DEFAULT nextval('seq_wordplay_pairs_id'),
            from_track_id INTEGER NOT NULL, to_track_id INTEGER NOT NULL,
            keyword VARCHAR NOT NULL, normalized_keyword VARCHAR NOT NULL,
            source_phrase VARCHAR NOT NULL, target_phrase VARCHAR NOT NULL,
            from_timestamp DOUBLE, to_timestamp DOUBLE,
            transition_notes VARCHAR NOT NULL DEFAULT '', source_url VARCHAR NOT NULL DEFAULT '',
            evidence_type VARCHAR NOT NULL DEFAULT 'hypothesis',
            verification_status VARCHAR NOT NULL DEFAULT 'unverified',
            status VARCHAR NOT NULL DEFAULT 'pending',
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, -- allow-engine-clock: Python's v4 fixture
            UNIQUE (from_track_id, to_track_id, normalized_keyword));
         INSERT INTO wordplay_pairs
            (id, from_track_id, to_track_id, keyword, normalized_keyword, source_phrase, target_phrase, status)
            VALUES (1, 10, 20, 'legacy', 'legacy', 'out', 'in', 'approved');
         CREATE TABLE schema_info (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
         INSERT INTO schema_info VALUES ('version', '4');",
    )
    .unwrap();
    init_schema(&conn).unwrap();
    type Row = (String, String, String, Option<f64>, Option<f64>, Option<f64>, String);
    let row: Row = conn
        .query_row(
            "SELECT source_section_position, target_section_position, source_cue_mode,
                    source_cue_end_timestamp, target_intro_timestamp, target_landing_timestamp, status
             FROM wordplay_pairs WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    assert_eq!(row, ("unknown".into(), "unknown".into(), "section_end".into(), None, None, None, "approved".into()));
    assert_eq!(read_schema_version(&conn).unwrap(), 5);
    // Every other v5 table was created; the existing one kept its sequence.
    let tables: i64 = conn
        .query_row("SELECT count(*) FROM duckdb_tables() WHERE schema_name = 'main'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tables, 31);
    let next: i64 = conn.query_row("SELECT nextval('seq_wordplay_pairs_id')", [], |row| row.get(0)).unwrap();
    assert_eq!(next, 2);
}

#[test]
fn migrations_run_from_the_stored_version_and_drop_retired_tables() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE presets (id INTEGER); CREATE TABLE prompts (id INTEGER);
         CREATE TABLE schema_info (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
         INSERT INTO schema_info VALUES ('version', 'garbage');",
    )
    .unwrap();
    init_schema(&conn).unwrap();
    let retired: i64 = conn
        .query_row("SELECT count(*) FROM duckdb_tables() WHERE table_name IN ('presets', 'prompts')", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(retired, 0, "unparseable version counts as 0, so v3 drops them");
    let value: String = conn.query_row("SELECT value FROM schema_info", [], |row| row.get(0)).unwrap();
    assert_eq!(value, "5");
    // A v5 file is not migrated again: re-created retired tables survive.
    conn.execute_batch("CREATE TABLE presets (id INTEGER)").unwrap();
    init_schema(&conn).unwrap();
    let kept: i64 =
        conn.query_row("SELECT count(*) FROM duckdb_tables() WHERE table_name = 'presets'", [], |row| row.get(0)).unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn a_library_from_a_newer_app_is_refused_untouched() {
    let dir = Dir::new("newer");
    let path = dir.0.join("library.duckdb");
    {
        let conn = open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_info (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
             INSERT INTO schema_info VALUES ('version', '6');
             CREATE TABLE future_table (id INTEGER);",
        )
        .unwrap();
        let error = init_schema(&conn).unwrap_err();
        assert!(matches!(error, DbError::SchemaTooNew { found: 6, supported: 5 }), "{error}");
        assert!(error.to_string().contains("スキーマ v6"), "{error}");
        let tables: i64 =
            conn.query_row("SELECT count(*) FROM duckdb_tables()", [], |row| row.get(0)).unwrap();
        assert_eq!(tables, 2, "nothing was created");
    }
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(check_file_version(&path), Err(DbError::SchemaTooNew { found: 6, .. })));
    assert_eq!(std::fs::read(&path).unwrap(), before, "the guard opens read-only");
    assert_eq!(check_file_version(&dir.0.join("absent.duckdb")).unwrap(), None);
    assert!(!dir.0.join("absent.duckdb").exists(), "the guard never creates a file");
}

#[test]
fn version_guard_reads_a_file_with_a_pending_wal() {
    let dir = Dir::new("wal");
    let path = dir.0.join("library.duckdb");
    let wal = dir.0.join("library.duckdb.wal");
    // Leave a WAL behind the way a killed process does: copy the files while
    // the writer is still open.
    let staged = dir.0.join("staged");
    std::fs::create_dir_all(&staged).unwrap();
    {
        let conn = open(&path).unwrap();
        conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown; SET wal_autocheckpoint = '1TB';").unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("UPDATE schema_info SET value = '7'").unwrap();
        assert!(wal.exists());
        std::fs::copy(&path, staged.join("library.duckdb")).unwrap();
        std::fs::copy(&wal, staged.join("library.duckdb.wal")).unwrap();
    }
    let copy = staged.join("library.duckdb");
    assert!(matches!(check_file_version(&copy), Err(DbError::SchemaTooNew { found: 7, .. })));
    assert!(staged.join("library.duckdb.wal").exists(), "a read-only open leaves the WAL in place");
}

/// Whether `code` calls the engine's clock: `now()` not preceded by an
/// identifier, path or method (`Instant::now()`), or one of the SQL clock
/// keywords as a whole word.
fn asks_engine_clock(code: &str) -> Option<&'static str> {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let words = ["now()", "current_timestamp", "current_date", "current_time", "localtimestamp", "get_current_time"]; // allow-engine-clock
    for word in words {
        let mut from = 0;
        while let Some(found) = code[from..].find(word) {
            let start = from + found;
            let end = start + word.len();
            let before = code[..start].chars().next_back();
            let after = code[end..].chars().next();
            let attached = ident(before) || matches!(before, Some(':' | '.'));
            let whole_word = word.ends_with(')') || !ident(after);
            if !attached && whole_word {
                return Some(word);
            }
            from = end;
        }
    }
    None
}

/// Plan R3: Rust SQL binds Clock values instead of calling the engine's clock
/// (the bundled DuckDB has no ICU, so it would answer UTC).
#[test]
fn rust_sources_never_ask_the_engine_for_the_time() {
    assert_eq!(asks_engine_clock("conn.execute(\"UPDATE t SET at=now()\")"), Some("now()")); // allow-engine-clock
    assert_eq!(asks_engine_clock("select current_timestamp"), Some("current_timestamp")); // allow-engine-clock
    assert_eq!(asks_engine_clock("let t = Instant::now(); yield_now(); x.now(); current_timeout"), None);
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut stack = vec![crates];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            checked += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let lower = line.to_ascii_lowercase();
                // Fixtures recreating old layouts carry historical DDL and are marked.
                if lower.contains("allow-engine-clock") {
                    continue;
                }
                // Comments may explain the rule.
                let code = lower.split("//").next().unwrap_or_default();
                if let Some(word) = asks_engine_clock(code) {
                    panic!("{}:{}: SQL must bind Clock values, not {word}", path.display(), number + 1);
                }
            }
        }
    }
    assert!(checked > 10);
}
