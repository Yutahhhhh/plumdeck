//! `plumdeck-parity db-stats FILE`: aggregates of a library opened
//! read-only, for checking that this crate's DuckDB reads a file (and its
//! WAL) exactly as the Python backend's does (plan R2). The output is
//! compared with tools/parity/db_stats.py, which runs the same queries
//! through Python's duckdb.
//!
//! Only aggregates are printed, never row values:
//! - per table: row count and, per column, the non-NULL count and
//!   `bit_xor(hash(column))` computed by the engine;
//! - per selected column ([`CLIENT_COLUMNS`]): a SHA-256 over the values as
//!   the client library decodes them, in primary-key order, which checks the
//!   bindings (FLOAT widening, TIMESTAMP, BLOB, text) as well.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::DateTime;
use duckdb::types::{TimeUnit, ValueRef};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// `(table, order by, column)` decoded on the client.
pub const CLIENT_COLUMNS: &[(&str, &str, &str)] = &[
    ("tracks", "id", "id"),
    ("tracks", "id", "filepath"),
    ("tracks", "id", "bpm"),
    ("tracks", "id", "duration"),
    ("tracks", "id", "is_genre_verified"),
    ("tracks", "id", "created_at"),
    ("track_analyses", "track_id", "beats_f32"),
    ("track_analyses", "track_id", "waveform_u8"),
    ("track_analyses", "track_id", "features_extra_json"),
    ("lyrics", "track_id", "content"),
    ("lyrics", "track_id", "updated_at"),
    ("setlists", "id", "target_duration"),
    ("setlists", "id", "updated_at"),
    ("setlist_tracks", "id", "in_ms"),
    ("settings", "key", "value"),
];

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The canonical text of one decoded value (same rules as db_stats.py).
fn canonical(value: ValueRef<'_>) -> String {
    let micros_to_iso = |micros: i64| {
        DateTime::from_timestamp_micros(micros)
            .map(|dt| plumdeck_core::time::py_isoformat(&dt.naive_utc()))
            .unwrap_or_else(|| format!("<timestamp {micros}>"))
    };
    match value {
        ValueRef::Null => "\\N".into(),
        ValueRef::Boolean(b) => if b { "1" } else { "0" }.into(),
        ValueRef::TinyInt(v) => v.to_string(),
        ValueRef::SmallInt(v) => v.to_string(),
        ValueRef::Int(v) => v.to_string(),
        ValueRef::BigInt(v) => v.to_string(),
        ValueRef::HugeInt(v) => v.to_string(),
        ValueRef::UTinyInt(v) => v.to_string(),
        ValueRef::USmallInt(v) => v.to_string(),
        ValueRef::UInt(v) => v.to_string(),
        ValueRef::UBigInt(v) => v.to_string(),
        ValueRef::Float(v) => format!("{:016x}", f64::from(v).to_bits()),
        ValueRef::Double(v) => format!("{:016x}", v.to_bits()),
        ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        ValueRef::Blob(bytes) => hex::encode(bytes),
        ValueRef::Timestamp(unit, raw) => {
            let micros = match unit {
                TimeUnit::Second => raw * 1_000_000,
                TimeUnit::Millisecond => raw * 1_000,
                TimeUnit::Microsecond => raw,
                TimeUnit::Nanosecond => raw / 1_000,
            };
            micros_to_iso(micros)
        }
        other => format!("<{:?}>", other.data_type()),
    }
}

fn tables(conn: &duckdb::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT table_name FROM duckdb_tables() WHERE schema_name = 'main' AND NOT temporary ORDER BY table_name",
    )?;
    let names = stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
    Ok(names)
}

fn columns(conn: &duckdb::Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT column_name FROM duckdb_columns() WHERE schema_name = 'main' AND table_name = ? ORDER BY column_index",
    )?;
    let names = stmt.query_map([table], |row| row.get(0))?.collect::<Result<_, _>>()?;
    Ok(names)
}

/// The aggregates of `path`, as JSON.
pub fn stats(path: &Path) -> Result<Value> {
    let conn = plumdeck_db::ClaimedConnection::open_read_only(path)
        .with_context(|| format!("open {} read-only", path.display()))?;
    let version = plumdeck_db::engine_version(&conn)?;
    let mut out_tables = Map::new();
    let all_tables = tables(&conn)?;
    for table in &all_tables {
        let cols = columns(&conn, table)?;
        let mut select = vec!["CAST(count(*) AS VARCHAR)".to_owned()];
        for col in &cols {
            let q = quote(col);
            select.push(format!("CAST(count({q}) AS VARCHAR)"));
            select.push(format!("CAST(bit_xor(hash({q})) AS VARCHAR)"));
        }
        let sql = format!("SELECT {} FROM {}", select.join(", "), quote(table));
        let values: Vec<Option<String>> = conn.query_row(&sql, [], |row| {
            (0..select.len()).map(|i| row.get::<_, Option<String>>(i)).collect()
        })?;
        let mut per_column = Map::new();
        for (index, col) in cols.iter().enumerate() {
            per_column.insert(col.clone(), json!([values[1 + 2 * index], values[2 + 2 * index]]));
        }
        out_tables.insert(table.clone(), json!({ "rows": values[0], "columns": per_column }));
    }
    let mut client = Map::new();
    for (table, order, column) in CLIENT_COLUMNS {
        if !all_tables.iter().any(|t| t == table) || !columns(&conn, table)?.iter().any(|c| c == column) {
            continue;
        }
        let sql = format!("SELECT {} FROM {} ORDER BY {}", quote(column), quote(table), quote(order));
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut hasher = Sha256::new();
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            hasher.update(canonical(row.get_ref(0)?).as_bytes());
            hasher.update(b"\n");
            count += 1;
        }
        client.insert(format!("{table}.{column}"), json!([count, hex::encode(hasher.finalize())]));
    }
    Ok(json!({ "engine": version, "tables": out_tables, "client": client }))
}
