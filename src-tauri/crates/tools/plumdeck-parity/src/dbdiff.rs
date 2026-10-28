//! `db-diff A.duckdb B.duckdb`: compares two plumdeck databases table by
//! table in primary-key order (plan §4.4, N13 without normalization yet).
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
    let conn = Connection::open_in_memory()?;
    for (alias, path) in [("a", a), ("b", b)] {
        let path = path.to_str().context("database path is not UTF-8")?;
        conn.execute_batch(&format!("ATTACH {} AS {alias} (READ_ONLY)", quote_literal(path)))
            .with_context(|| format!("attach {path}"))?;
    }
    let mut out = DbDiff { lines: Vec::new(), differences: 0 };
    let (ta, tb) = (tables(&conn, "a")?, tables(&conn, "b")?);
    for table in ta.symmetric_difference(&tb) {
        out.differences += 1;
        let side = if ta.contains(table) { "A" } else { "B" };
        out.lines.push(format!("table {table}: only in {side}"));
    }
    for table in ta.intersection(&tb) {
        let (ca, cb) = (columns(&conn, "a", table)?, columns(&conn, "b", table)?);
        if ca != cb {
            out.differences += 1;
            out.lines.push(format!("table {table}: columns differ\n    A {ca:?}\n    B {cb:?}"));
        }
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
}
