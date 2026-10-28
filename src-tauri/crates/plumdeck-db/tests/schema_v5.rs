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
use plumdeck_db::{create_schema_v5, engine_version, ensure_engine_version, open, EXPECTED_DUCKDB_VERSION};
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
    let path = dir.join("fresh.duckdb");
    let _ = std::fs::remove_file(&path);
    let conn = open(&path).unwrap();
    create_schema_v5(&conn).unwrap();

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
    drop(stmt);
    drop(conn);
    std::fs::remove_dir_all(&dir).ok();
}
