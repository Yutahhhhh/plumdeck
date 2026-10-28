//! `db-diff A.duckdb B.duckdb`: compares two plumdeck databases table by
//! table in primary-key order (plan §4.4, N13 without normalization yet).
//!
//! The schema is compared too: every column's type, nullability and default
//! in physical order, constraints (PRIMARY KEY, UNIQUE, CHECK, NOT NULL,
//! FOREIGN KEY), indexes, views and sequence settings, so a Rust init_db or
//! compaction rebuild that drops one shows up even when the rows agree.
//!
//! Both files are attached read-only to one in-memory DuckDB, so neither is
//! modified. Values are compared as DuckDB's VARCHAR rendering, which is
//! exact for every type the schema uses (DOUBLE/FLOAT print round-trip digits).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use duckdb::Connection;

type Row = Vec<Option<String>>;

pub struct DbDiff {
    pub lines: Vec<String>,
    pub differences: usize,
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn strings(conn: &Connection, sql: &str, params: &[&str]) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(duckdb::params_from_iter(params.iter()), |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn pairs(conn: &Connection, sql: &str, params: &[&str]) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(duckdb::params_from_iter(params.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn tables(conn: &Connection, db: &str) -> Result<BTreeSet<String>> {
    Ok(strings(
        conn,
        "SELECT table_name FROM duckdb_tables() WHERE database_name = ? AND schema_name = 'main'",
        &[db],
    )?
    .into_iter()
    .collect())
}

fn columns(conn: &Connection, db: &str, table: &str) -> Result<Vec<(String, Option<String>)>> {
    pairs(
        conn,
        "SELECT column_name, data_type FROM duckdb_columns()
         WHERE database_name = ? AND schema_name = 'main' AND table_name = ? ORDER BY column_index",
        &[db, table],
    )
}

fn primary_key(conn: &Connection, db: &str, table: &str) -> Result<Vec<String>> {
    let found = strings(
        conn,
        "SELECT array_to_string(constraint_column_names, chr(31)) FROM duckdb_constraints()
         WHERE database_name = ? AND schema_name = 'main' AND table_name = ?
           AND constraint_type = 'PRIMARY KEY'",
        &[db, table],
    )?;
    Ok(found.first().map(|k| k.split('\u{1f}').map(str::to_owned).collect()).unwrap_or_default())
}

/// `(key, text)` rows grouped by key, in query order.
fn grouped(conn: &Connection, sql: &str, db: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, text) in pairs(conn, sql, &[db])? {
        out.entry(key).or_default().push(text.unwrap_or_default());
    }
    Ok(out)
}

/// The schema of `db` as comparable items: `"<kind> <name>" -> definition`.
fn schema(conn: &Connection, db: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let columns = grouped(
        conn,
        // duckdb_columns() also lists view columns; keep base tables only.
        "SELECT c.table_name, c.column_name || ' ' || c.data_type
                || CASE WHEN c.is_nullable THEN '' ELSE ' NOT NULL' END
                || COALESCE(' DEFAULT ' || c.column_default, '')
         FROM duckdb_columns() c JOIN duckdb_tables() t USING (table_oid)
         WHERE c.database_name = ? AND c.schema_name = 'main'
         ORDER BY c.table_name, c.column_index",
        db,
    )?;
    for (table, defs) in columns {
        out.insert(format!("table {table} columns"), defs.join(", "));
    }
    // constraint_index is a database-wide counter, so it is left out.
    let constraints = grouped(
        conn,
        "SELECT table_name, constraint_type || ' ' || constraint_text
                || ' (' || array_to_string(constraint_column_names, ', ') || ')' AS def
         FROM duckdb_constraints() WHERE database_name = ? AND schema_name = 'main'
         ORDER BY table_name, def",
        db,
    )?;
    for (table, defs) in constraints {
        out.insert(format!("table {table} constraints"), defs.join("; "));
    }
    let named = [
        ("index", "SELECT index_name, sql FROM duckdb_indexes() WHERE database_name = ? AND schema_name = 'main'"),
        (
            "view",
            "SELECT view_name, sql FROM duckdb_views()
             WHERE database_name = ? AND schema_name = 'main' AND NOT internal",
        ),
        (
            "sequence",
            // start_value follows the persisted counter; last_value is compared below.
            "SELECT sequence_name, format('increment {} min {} max {} cycle {}',
                    increment_by, min_value, max_value, cycle)
             FROM duckdb_sequences() WHERE database_name = ? AND schema_name = 'main'",
        ),
    ];
    for (kind, sql) in named {
        for (name, def) in pairs(conn, sql, &[db])? {
            out.insert(format!("{kind} {name}"), def.unwrap_or_default());
        }
    }
    Ok(out)
}

fn rows(conn: &Connection, db: &str, table: &str, cols: &[String], order: &[String]) -> Result<Vec<Row>> {
    let select: Vec<String> =
        cols.iter().map(|c| format!("CAST({} AS VARCHAR)", quote_ident(c))).collect();
    let order: Vec<String> = order.iter().map(|c| format!("{} NULLS FIRST", quote_ident(c))).collect();
    let sql = format!(
        "SELECT {} FROM {}.main.{} ORDER BY {}",
        select.join(", "),
        quote_ident(db),
        quote_ident(table),
        order.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let width = cols.len();
    let mapped = stmt.query_map([], |row| {
        (0..width).map(|i| row.get::<_, Option<String>>(i)).collect::<Result<Row, _>>()
    })?;
    Ok(mapped.collect::<Result<_, _>>()?)
}

fn show(row: &Row) -> String {
    let cells: Vec<String> = row.iter().map(|v| v.clone().unwrap_or_else(|| "NULL".into())).collect();
    format!("({})", cells.join(", "))
}

/// Compare every table, column list, row and sequence of `a` and `b`.
pub fn diff(a: &Path, b: &Path, max_rows: usize) -> Result<DbDiff> {
    diff_ignoring(a, b, max_rows, &[])
}

/// [`diff`] without the row values of `ignored` (`(table, column)`; a
/// primary-key column is still compared unless it is ignored itself, which
/// compares the table's other columns as a multiset).
pub fn diff_ignoring(a: &Path, b: &Path, max_rows: usize, ignored: &[(String, String)]) -> Result<DbDiff> {
    let conn = Connection::open_in_memory()?;
    for (alias, path) in [("a", a), ("b", b)] {
        let path = path.to_str().context("database path is not UTF-8")?;
        conn.execute_batch(&format!("ATTACH {} AS {alias} (READ_ONLY)", quote_literal(path)))
            .with_context(|| format!("attach {path}"))?;
    }
    let mut out = DbDiff { lines: Vec::new(), differences: 0 };
    let (sa, sb) = (schema(&conn, "a")?, schema(&conn, "b")?);
    for item in sa.keys().chain(sb.keys()).collect::<BTreeSet<_>>() {
        match (sa.get(item), sb.get(item)) {
            (Some(da), Some(db)) if da == db => {}
            (da, db) => {
                out.differences += 1;
                out.lines.push(format!("schema {item}:\n    A {da:?}\n    B {db:?}"));
            }
        }
    }
    let (ta, tb) = (tables(&conn, "a")?, tables(&conn, "b")?);
    // A table on one side only is already reported as a schema difference.
    for table in ta.intersection(&tb) {
        // Column definitions were compared with the schema; rows use the
        // columns both sides have.
        let (ca, cb) = (columns(&conn, "a", table)?, columns(&conn, "b", table)?);
        let names_b: BTreeSet<&String> = cb.iter().map(|(n, _)| n).collect();
        let common: Vec<String> =
            ca.iter().map(|(n, _)| n.clone()).filter(|n| names_b.contains(n)).collect();
        if common.is_empty() {
            continue;
        }
        let mut pk = primary_key(&conn, "a", table)?;
        if pk.is_empty() || pk.iter().any(|k| !common.contains(k)) {
            pk = common.clone();
        }
        let is_ignored = |c: &String| ignored.iter().any(|(t, col)| t == table && col == c);
        // A generated primary key (uuid4 ids) cannot pair rows: compare the
        // remaining columns as a multiset instead.
        let generated_key = pk.iter().any(is_ignored);
        let common: Vec<String> = common.into_iter().filter(|c| (!generated_key && pk.contains(c)) || !is_ignored(c)).collect();
        if generated_key {
            pk = common.clone();
        }
        let key_index: Vec<usize> = pk.iter().map(|k| common.iter().position(|c| c == k).unwrap()).collect();
        let keyed = |rows: Vec<Row>| -> BTreeMap<Row, Vec<Row>> {
            let mut map: BTreeMap<Row, Vec<Row>> = BTreeMap::new();
            for row in rows {
                let key: Row = key_index.iter().map(|&i| row[i].clone()).collect();
                map.entry(key).or_default().push(row);
            }
            map
        };
        let ra = keyed(rows(&conn, "a", table, &common, &pk)?);
        let rb = keyed(rows(&conn, "b", table, &common, &pk)?);
        let mut shown = 0;
        let mut table_diffs = 0;
        let keys: BTreeSet<&Row> = ra.keys().chain(rb.keys()).collect();
        for key in keys {
            let (va, vb) = (ra.get(key), rb.get(key));
            if va == vb {
                continue;
            }
            table_diffs += 1;
            if shown >= max_rows {
                continue;
            }
            shown += 1;
            match (va, vb) {
                (Some(va), None) => out.lines.push(format!("{table} {}: only in A {}", show(key), show(&va[0]))),
                (None, Some(vb)) => out.lines.push(format!("{table} {}: only in B {}", show(key), show(&vb[0]))),
                (Some(va), Some(vb)) if va.len() == 1 && vb.len() == 1 => {
                    let changed: Vec<String> = common
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| va[0][*i] != vb[0][*i])
                        .map(|(i, c)| format!("{c}: {:?} -> {:?}", va[0][i], vb[0][i]))
                        .collect();
                    out.lines.push(format!("{table} {}: {}", show(key), changed.join("; ")));
                }
                (va, vb) => out.lines.push(format!(
                    "{table} {}: {} row(s) in A, {} in B",
                    show(key),
                    va.map_or(0, Vec::len),
                    vb.map_or(0, Vec::len)
                )),
            }
        }
        if table_diffs > 0 {
            out.differences += table_diffs;
            out.lines.push(format!("table {table}: {table_diffs} differing key(s)"));
        }
    }
    let sequences = "SELECT sequence_name, CAST(last_value AS VARCHAR) FROM duckdb_sequences()
                     WHERE database_name = ? AND schema_name = 'main' ORDER BY sequence_name";
    let (sa, sb) = (pairs(&conn, sequences, &["a"])?, pairs(&conn, sequences, &["b"])?);
    let (sa, sb): (BTreeMap<_, _>, BTreeMap<_, _>) = (sa.into_iter().collect(), sb.into_iter().collect());
    for name in sa.keys().chain(sb.keys()).collect::<BTreeSet<_>>() {
        if sa.get(name) != sb.get(name) {
            out.differences += 1;
            out.lines.push(format!("sequence {name}: last_value {:?} vs {:?}", sa.get(name), sb.get(name)));
        }
    }
    out.lines.push(format!(
        "db-diff: {} tables compared, {} difference(s)",
        ta.intersection(&tb).count(),
        out.differences
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(path: &Path, extra: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "CREATE SEQUENCE seq_t START 1;
             CREATE TABLE t(id INTEGER DEFAULT(nextval('seq_t')) PRIMARY KEY, name VARCHAR, bpm FLOAT);
             INSERT INTO t(name, bpm) VALUES ('a', 127.9), ('b', NULL);
             {extra}"
        ))
        .unwrap();
    }

    #[test]
    fn reports_row_and_sequence_differences() {
        let dir = std::env::temp_dir().join(format!("plumdeck-dbdiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.duckdb"), dir.join("b.duckdb"), dir.join("c.duckdb"));
        for path in [&a, &b, &c] {
            let _ = std::fs::remove_file(path);
        }
        make(&a, "");
        make(&b, "");
        make(&c, "UPDATE t SET name = 'z' WHERE id = 2; INSERT INTO t(name) VALUES ('c');");
        assert_eq!(diff(&a, &b, 10).unwrap().differences, 0);
        let result = diff(&a, &c, 10).unwrap();
        // Row 2 changed, row 3 added, and the sequence advanced.
        assert_eq!(result.differences, 3, "{:#?}", result.lines);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reports_schema_differences_with_equal_rows() {
        let dir = std::env::temp_dir().join(format!("plumdeck-dbdiff-schema-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("s1.duckdb"), dir.join("s2.duckdb"));
        for path in [&a, &b] {
            let _ = std::fs::remove_file(path);
        }
        Connection::open(&a)
            .unwrap()
            .execute_batch(
                "CREATE TABLE t(id INTEGER PRIMARY KEY, name VARCHAR NOT NULL DEFAULT 'x', u VARCHAR UNIQUE);
                 INSERT INTO t VALUES (1, 'a', 'u');
                 CREATE INDEX idx_t_name ON t(name);
                 CREATE VIEW v AS SELECT id FROM t;",
            )
            .unwrap();
        Connection::open(&b)
            .unwrap()
            .execute_batch(
                "CREATE TABLE t(id INTEGER PRIMARY KEY, name VARCHAR, u VARCHAR);
                 INSERT INTO t VALUES (1, 'a', 'u');",
            )
            .unwrap();
        let result = diff(&a, &b, 10).unwrap();
        // Columns (NOT NULL, DEFAULT), constraints (NOT NULL, UNIQUE), index, view.
        assert_eq!(result.differences, 4, "{:#?}", result.lines);
        assert!(diff(&a, &a, 10).unwrap().differences == 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
