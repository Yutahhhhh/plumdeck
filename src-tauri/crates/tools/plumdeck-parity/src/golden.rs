//! Golden recordings of the Python oracle (plan §4.4: "S13 までに … Python
//! 応答をゴールデンとして記録し、S14 以降は Rust とゴールデンを比較する").
//!
//! `suite --record-golden DIR` runs a scenario as usual and also stores side
//! A's answer to every step plus the library A leaves; `suite --golden DIR`
//! then runs side B alone and compares it with that recording, so the
//! scenarios keep guarding the port after the Python backend is gone.
//!
//! A recording is `<DIR>/<scenario>.json.z` (zlib JSON). The scenario's work
//! directory is stored as `{work}` and A's port as `{port}`, so a replay on
//! another machine or port substitutes its own. The library is the schema
//! DuckDB's `EXPORT DATABASE` writes, every row as the VARCHAR rendering the
//! db-diff compares (cast back on restore), and the sequences' last values.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use duckdb::Connection;
use serde::{Deserialize, Serialize};

use crate::compare::Captured;

/// One recorded answer of side A.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub status: u16,
    pub content_type: String,
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Table {
    pub columns: Vec<(String, String)>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// The library side A left.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Library {
    pub schema: String,
    /// Tables in `load.sql` order (dependencies first).
    pub tables: Vec<(String, Table)>,
    pub sequences: BTreeMap<String, Option<i64>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Recording {
    pub scenario: String,
    pub answers: Vec<Answer>,
    pub library: Library,
}

enum Mode {
    Record { work: String, port: u16, answers: Vec<Answer> },
    Replay { work: String, port: u16, answers: VecDeque<Answer> },
}

static MODE: Mutex<Option<Mode>> = Mutex::new(None);

fn mode() -> std::sync::MutexGuard<'static, Option<Mode>> {
    MODE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record side A's answers (work directory `work`, A on `port`).
pub fn start_recording(work: &str, port: u16) {
    *mode() = Some(Mode::Record { work: work.to_owned(), port, answers: Vec::new() });
}

/// Answer side A from a recording.
pub fn start_replay(work: &str, port: u16, answers: Vec<Answer>) {
    *mode() = Some(Mode::Replay { work: work.to_owned(), port, answers: answers.into() });
}

/// End the current mode; the recorded answers, if recording.
pub fn finish() -> Vec<Answer> {
    match mode().take() {
        Some(Mode::Record { answers, .. }) => answers,
        _ => Vec::new(),
    }
}

pub fn replaying() -> bool {
    matches!(*mode(), Some(Mode::Replay { .. }))
}

fn generalize(text: &str, work: &str, port: u16) -> String {
    text.replace(work, "{work}").replace(&format!(":{port}"), ":{port}")
}

fn specialize(text: &str, work: &str, port: u16) -> String {
    text.replace("{work}", work).replace(":{port}", &format!(":{port}"))
}

/// While recording, keep A's answer.
pub fn record(captured: &Captured) {
    if let Some(Mode::Record { work, port, answers }) = mode().as_mut() {
        let (text, base64) = match std::str::from_utf8(&captured.body) {
            Ok(text) => (Some(generalize(text, work, *port)), None),
            Err(_) => (None, Some(base64::engine::general_purpose::STANDARD.encode(&captured.body))),
        };
        answers.push(Answer {
            status: captured.status,
            content_type: captured.content_type.clone(),
            headers: captured.headers.iter().map(|(n, v)| (n.clone(), generalize(v, work, *port))).collect(),
            text,
            base64,
        });
    }
}

/// While replaying, A's next answer.
pub fn next() -> Result<Captured> {
    let mut guard = mode();
    let Some(Mode::Replay { work, port, answers }) = guard.as_mut() else { bail!("not replaying a recording") };
    let answer = answers.pop_front().context("the recording has fewer answers than the scenario has steps; record it again")?;
    let body = match (&answer.text, &answer.base64) {
        (Some(text), _) => specialize(text, work, *port).into_bytes(),
        (None, Some(data)) => base64::engine::general_purpose::STANDARD.decode(data)?,
        (None, None) => Vec::new(),
    };
    Ok(Captured {
        status: answer.status,
        content_type: answer.content_type,
        headers: answer.headers.iter().map(|(n, v)| (n.clone(), specialize(v, work, *port))).collect(),
        body,
    })
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The library at `path` (read-only), paths under `work` generalized.
pub fn dump(path: &Path, work: &str, scratch: &Path) -> Result<Library> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(&format!("ATTACH {} AS lib (READ_ONLY); USE lib;", quote_literal(&path.to_string_lossy())))?;
    if scratch.exists() {
        std::fs::remove_dir_all(scratch)?;
    }
    conn.execute_batch(&format!("EXPORT DATABASE {} (FORMAT csv)", quote_literal(&scratch.to_string_lossy())))?;
    let schema = std::fs::read_to_string(scratch.join("schema.sql"))?;
    let load = std::fs::read_to_string(scratch.join("load.sql"))?;
    let _ = std::fs::remove_dir_all(scratch);
    let mut tables = Vec::new();
    for line in load.lines() {
        let Some(rest) = line.strip_prefix("COPY ") else { continue };
        let name = rest.split(" FROM ").next().unwrap_or_default().trim().trim_matches('"').to_owned();
        let mut statement = conn.prepare(
            "SELECT column_name, data_type FROM duckdb_columns() WHERE database_name = 'lib' AND schema_name = 'main' AND table_name = ? ORDER BY column_index",
        )?;
        let columns: Vec<(String, String)> = statement.query_map([&name], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let select: Vec<String> = columns.iter().map(|(c, _)| format!("CAST({} AS VARCHAR)", quote_ident(c))).collect();
        let sql = format!("SELECT {} FROM lib.main.{} ORDER BY ALL", select.join(", "), quote_ident(&name));
        let width = columns.len();
        let mut statement = conn.prepare(&sql)?;
        let rows: Vec<Vec<Option<String>>> = statement
            .query_map([], |row| (0..width).map(|i| row.get::<_, Option<String>>(i)).collect::<Result<Vec<_>, _>>())?
            .collect::<Result<_, _>>()?;
        let rows = rows.into_iter().map(|row| row.into_iter().map(|v| v.map(|t| t.replace(work, "{work}"))).collect()).collect();
        tables.push((name, Table { columns, rows }));
    }
    let mut statement = conn.prepare("SELECT sequence_name, last_value FROM duckdb_sequences() WHERE database_name = 'lib' AND schema_name = 'main'")?;
    let sequences = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))?.collect::<Result<_, _>>()?;
    Ok(Library { schema, tables, sequences })
}

/// Rebuild a recorded library at `path` with paths under `work`.
pub fn restore(library: &Library, path: &Path, work: &str) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    let conn = Connection::open(path)?;
    // `last_value` of a reopened file is the persisted counter (one past the
    // last id), so a used sequence starts one below the recorded value and
    // is advanced once: reopened, it reads the same and continues the same.
    let schema: String = library
        .schema
        .lines()
        .map(|line| {
            let name = line.strip_prefix("CREATE SEQUENCE ").and_then(|rest| rest.split_whitespace().next()).map(|n| n.trim_matches('"'));
            match (name.and_then(|n| library.sequences.get(n)).copied().flatten(), line.find(" START ")) {
                (Some(last), Some(at)) => {
                    let rest = &line[at + 7..];
                    let end = rest.find(' ').unwrap_or(rest.len());
                    format!("{} START {}{}", &line[..at], (last - 1).max(1), &rest[end..])
                }
                _ => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    conn.execute_batch(&schema).context("recorded schema")?;
    for (name, value) in &library.sequences {
        if value.is_some() {
            conn.execute_batch(&format!("SELECT nextval({})", quote_literal(name)))?;
        }
    }
    conn.execute_batch("BEGIN")?;
    for (name, table) in &library.tables {
        if table.rows.is_empty() {
            continue;
        }
        let columns: Vec<String> = table.columns.iter().map(|(c, _)| quote_ident(c)).collect();
        let values: Vec<String> = table.columns.iter().map(|(_, t)| format!("CAST(? AS {t})")).collect();
        let sql = format!("INSERT INTO main.{} ({}) VALUES ({})", quote_ident(name), columns.join(", "), values.join(", "));
        let mut statement = conn.prepare(&sql)?;
        for row in &table.rows {
            let cells: Vec<Option<String>> = row.iter().map(|v| v.as_ref().map(|t| t.replace("{work}", work))).collect();
            statement.execute(duckdb::params_from_iter(cells)).with_context(|| format!("restore a row of {name}"))?;
        }
    }
    conn.execute_batch("COMMIT; CHECKPOINT;")?;
    Ok(())
}

pub fn file(dir: &Path, scenario: &str) -> std::path::PathBuf {
    dir.join(format!("{scenario}.json.z"))
}

pub fn save(dir: &Path, recording: &Recording) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec(recording)?;
    std::fs::write(file(dir, &recording.scenario), miniz_oxide::deflate::compress_to_vec_zlib(&json, 9))?;
    Ok(())
}

pub fn load(dir: &Path, scenario: &str) -> Result<Recording> {
    let path = file(dir, scenario);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let json = miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).map_err(|e| anyhow::anyhow!("inflate {}: {e:?}", path.display()))?;
    Ok(serde_json::from_slice(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_survives_dump_and_restore() {
        let dir = std::env::temp_dir().join(format!("plumdeck-golden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.duckdb");
        let work = dir.to_string_lossy().into_owned();
        {
            let conn = Connection::open(&source).unwrap();
            conn.execute_batch(&format!(
                "CREATE SEQUENCE s START 1; CREATE TABLE t(id INTEGER DEFAULT nextval('s') PRIMARY KEY, path VARCHAR, blob BLOB, f FLOAT, e VARCHAR);
                 INSERT INTO t (path, blob, f, e) VALUES ('{work}/x.mp3', '\\x00\\xff'::BLOB, 0.1, ''), (NULL, NULL, NULL, NULL);"
            ))
            .unwrap();
        }
        let library = dump(&source, &work, &dir.join("export")).unwrap();
        assert_eq!(library.tables[0].1.rows[0][1].as_deref(), Some("{work}/x.mp3"));
        let back = dir.join("b.duckdb");
        restore(&library, &back, &work).unwrap();
        let result = crate::dbdiff::diff(&source, &back, 5).unwrap();
        assert_eq!(result.differences, 0, "{:?}", result.lines);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
