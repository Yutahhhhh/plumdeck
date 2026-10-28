//! Developer maintenance commands on the app's platform-specific paths
//! (`pnpm backup:db`, `restore:db`, `local:restore-db`, `prod:restore-db`,
//! `prod:clean-db`, `prod:clean-db-w`, `log:watch`, `db:cli`).
//!
//! `backup` copies the production library (DB_PATH) to `backup/`,
//! `restore` copies `db_data/` over the production library, the two
//! `*-restore` commands migrate the backup's rows into a target library
//! (common columns, then the id sequences), and `clean-db` / `clean-wal`
//! keep a backup before deleting. Every copy holds the source open and uses
//! `COPY FROM DATABASE`, so Windows' exclusive lock is respected.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plumdeck_core::config::Config;
use plumdeck_db::duckdb::Connection;

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn quote(text: &str) -> String {
    text.replace('\'', "''")
}

fn copy_database(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("{} not found", source.display()));
    }
    if source.canonicalize().ok() == destination.canonicalize().ok() {
        return Err("Source and destination must differ".into());
    }
    let connection = Connection::open(source).map_err(|e| e.to_string())?;
    connection.execute_batch("CHECKPOINT").map_err(|e| e.to_string())?;
    if destination.exists() {
        let saved = with_suffix(destination, &format!(".{}.bak", nanos()));
        copy_database(destination, &saved)?;
        println!("Previous database saved: {}", saved.display());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = with_suffix(destination, &format!(".{}.copying", nanos()));
    let result = (|| {
        let database: String = connection.query_row("SELECT current_database()", [], |row| row.get(0)).map_err(|e| e.to_string())?;
        let database = database.replace('"', "\"\"");
        connection
            .execute_batch(&format!(
                "ATTACH '{}' AS plumdeck_snapshot; COPY FROM DATABASE \"{database}\" TO plumdeck_snapshot; CHECKPOINT plumdeck_snapshot; DETACH plumdeck_snapshot;",
                quote(&temporary.to_string_lossy())
            ))
            .map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, destination).map_err(|e| e.to_string())
    })();
    drop(connection);
    let _ = std::fs::remove_file(&temporary);
    let _ = std::fs::remove_file(with_suffix(&temporary, ".wal"));
    result?;
    println!("{}", destination.display());
    Ok(())
}

const TABLES: [&str; 10] =
    ["tracks", "track_analyses", "track_embeddings", "lyrics", "prompts", "presets", "setlists", "setlist_tracks", "wordplay_pairs", "settings"];
const SEQUENCES: [(&str, &str); 6] = [
    ("seq_tracks_id", "tracks"),
    ("seq_prompts_id", "prompts"),
    ("seq_presets_id", "presets"),
    ("seq_setlists_id", "setlists"),
    ("seq_setlist_tracks_id", "setlist_tracks"),
    ("seq_wordplay_pairs_id", "wordplay_pairs"),
];

fn columns(conn: &Connection, alias: &str, table: &str) -> Vec<String> {
    let Ok(mut statement) = conn.prepare(&format!("DESCRIBE {alias}.{table}")) else {
        println!("    Error fetching columns for {alias}.{table}");
        return Vec::new();
    };
    statement.query_map([], |row| row.get::<_, String>(0)).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
}

/// Migrate the rows of `from` into `to` (the old transfer_db.py).
fn migrate(from: &Path, to: &Path) -> Result<(), String> {
    if !from.exists() {
        return Err(format!("{} not found", from.display()));
    }
    println!("Starting migration...\nSource: {}\nTarget: {}", from.display(), to.display());
    let conn = Connection::open(to).map_err(|e| e.to_string())?;
    let run = |sql: &str| conn.execute_batch(sql).map_err(|e| e.to_string());
    run(&format!("ATTACH '{}' AS source_db (READ_ONLY); BEGIN TRANSACTION;", quote(&from.to_string_lossy())))?;
    for table in TABLES {
        println!("  Migrating table: {table}...");
        let source = columns(&conn, "source_db", table);
        let common: Vec<String> = columns(&conn, "main", table).into_iter().filter(|c| source.contains(c)).collect();
        if common.is_empty() {
            println!("    Warning: No common columns found for {table}.");
            continue;
        }
        let list = common.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        run(&format!("DELETE FROM main.{table}; INSERT INTO main.{table} ({list}) SELECT {list} FROM source_db.{table};"))?;
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM main.{table}"), [], |row| row.get(0)).map_err(|e| e.to_string())?;
        println!("    Done. ({count} rows migrated)");
    }
    println!("Synchronizing sequences using nextval pumping...");
    for (sequence, table) in SEQUENCES {
        let max: Result<Option<i64>, _> = conn.query_row(&format!("SELECT MAX(id) FROM {table}"), [], |row| row.get(0));
        match max {
            Ok(Some(max)) if max > 0 => match conn.execute_batch(&format!("SELECT count(nextval('{sequence}')) FROM range({max});")) {
                Ok(()) => println!("    Sequence {sequence} advanced to {max}"),
                Err(error) => println!("    Warning: Could not sync sequence {sequence}: {error}"),
            },
            Ok(_) => println!("    Sequence {sequence} skip (no data)"),
            Err(error) => println!("    Warning: Could not sync sequence {sequence}: {error}"),
        }
    }
    run("COMMIT")?;
    println!("\nMigration completed successfully!\nYou can now swap the database files.");
    Ok(())
}

fn watch(log: &Path) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    println!("Watching {}", log.display());
    let mut position = 0u64;
    loop {
        if let Ok(mut file) = std::fs::File::open(log) {
            let size = file.metadata().map_or(0, |m| m.len());
            if size < position {
                position = 0;
            }
            file.seek(SeekFrom::Start(position)).map_err(|e| e.to_string())?;
            let mut data = Vec::new();
            file.read_to_end(&mut data).map_err(|e| e.to_string())?;
            position += data.len() as u64;
            if !data.is_empty() {
                let mut out = std::io::stdout();
                let _ = out.write_all(&data);
                let _ = out.flush();
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn run(command: &str, root: &Path) -> Result<(), String> {
    let config = Config::from_env().map_err(|e| e.to_string())?;
    let production = config.db_path.clone();
    let development = root.join("db_data/plumdeck.duckdb");
    let backup = root.join("backup/plumdeck.duckdb");
    match command {
        "backup" => copy_database(&production, &backup),
        "restore" => copy_database(&development, &production),
        "local-restore" => migrate(&backup, &development),
        "prod-restore" => migrate(&backup, &production),
        "db-cli" => {
            let status = std::process::Command::new("duckdb").arg(&development).status().map_err(|e| format!("duckdb CLI: {e}"))?;
            if status.success() { Ok(()) } else { Err(format!("duckdb exited with {status}")) }
        }
        "log-watch" => watch(&config.log_dir.join("plumdeck.log")),
        "clean-db" | "clean-wal" => {
            if production.exists() {
                copy_database(&production, &backup)?;
            }
            if command == "clean-db" {
                let _ = std::fs::remove_file(&production);
            }
            let _ = std::fs::remove_file(with_suffix(&production, ".wal"));
            Ok(())
        }
        other => Err(format!("Unknown maintenance command: {other}")),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("usage: plumdeck-maintenance <backup|restore|local-restore|prod-restore|db-cli|log-watch|clean-db|clean-wal> [--root DIR]");
        return ExitCode::from(2);
    };
    let root = match (args.next().as_deref(), args.next()) {
        (Some("--root"), Some(dir)) => PathBuf::from(dir),
        _ => std::env::current_dir().unwrap_or_default(),
    };
    match run(&command, &root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
