//! Logging in the Python backend's shape (backend/utils/logger.py): every
//! record goes to `<PLUMDECK_LOG_DIR>/plumdeck.log` (default
//! `<USER_DATA_DIR>/logs`) and to stderr as
//! `2026-10-28 12:34:56,789 - <logger> - INFO - <message>`, and the file
//! rotates at 10 MB keeping 5 backups (`plumdeck.log.1` .. `.5`), as
//! `RotatingFileHandler(maxBytes=10*1024*1024, backupCount=5)` does.
//!
//! `<logger>` is the tracing target; events that name a Python logger
//! (`target: "infra.database.schema"`) read exactly like the Python lines.
//! The level filter is INFO unless RUST_LOG says otherwise.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// The log file's name inside the log directory.
pub const LOG_FILE_NAME: &str = "plumdeck.log";
/// `maxBytes`.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// `backupCount`.
pub const BACKUP_COUNT: u32 = 5;

/// Python's level names.
fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        // Python has no TRACE.
        Level::DEBUG | Level::TRACE => "DEBUG",
    }
}

/// `'%(asctime)s - %(name)s - %(levelname)s - %(message)s'`, asctime in
/// local time with truncated milliseconds after a comma.
#[derive(Clone, Copy, Debug, Default)]
pub struct PythonFormat;

impl<S, N> FormatEvent<S, N> for PythonFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let metadata = event.metadata();
        write!(
            writer,
            "{} - {} - {} - ",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f"),
            metadata.target(),
            level_name(metadata.level())
        )?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// A size-rotated log file with `RotatingFileHandler`'s rules.
#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    backups: u32,
    file: Option<File>,
    /// `stream.tell()`: bytes in the current file.
    position: u64,
}

impl RotatingFile {
    /// Open `path` for appending (created if missing), as the handler does
    /// when it is constructed.
    pub fn open(path: &Path, max_bytes: u64, backups: u32) -> io::Result<Self> {
        let mut rotating = Self { path: path.to_owned(), max_bytes, backups, file: None, position: 0 };
        rotating.reopen()?;
        Ok(rotating)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.position = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    /// `shouldRollover`: never for an empty file or a non-regular path; the
    /// record's length is counted in characters, as Python's `len(msg)` does.
    fn should_roll_over(&self, record: &str) -> bool {
        if self.max_bytes == 0 || self.position == 0 {
            return false;
        }
        if self.position + record.chars().count() as u64 >= self.max_bytes {
            return !self.path.exists() || self.path.is_file();
        }
        false
    }

    fn numbered(&self, index: u32) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    /// `doRollover`: `.4` -> `.5`, ..., `.1` -> `.2`, the file -> `.1`, then
    /// start a new file.
    fn roll_over(&mut self) -> io::Result<()> {
        self.file = None;
        if self.backups > 0 {
            for index in (1..self.backups).rev() {
                let (source, target) = (self.numbered(index), self.numbered(index + 1));
                if source.exists() {
                    if target.exists() {
                        std::fs::remove_file(&target)?;
                    }
                    std::fs::rename(&source, &target)?;
                }
            }
            let first = self.numbered(1);
            if first.exists() {
                std::fs::remove_file(&first)?;
            }
            if self.path.exists() {
                std::fs::rename(&self.path, &first)?;
            }
        }
        self.reopen()
    }

    /// Write one formatted record (with its newline).
    pub fn write_record(&mut self, record: &str) -> io::Result<()> {
        if self.should_roll_over(record) {
            self.roll_over()?;
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let file = self.file.as_mut().expect("reopened");
        file.write_all(record.as_bytes())?;
        self.position += record.len() as u64;
        Ok(())
    }
}

/// [`MakeWriter`] over a shared [`RotatingFile`]. tracing-subscriber formats
/// each event into one buffer and writes it in one call, so each `write` is
/// one record.
#[derive(Clone, Debug)]
pub struct SharedLogFile(Arc<Mutex<RotatingFile>>);

impl SharedLogFile {
    pub fn new(file: RotatingFile) -> Self {
        Self(Arc::new(Mutex::new(file)))
    }
}

/// The writer handed out per event.
pub struct LogFileWriter(Arc<Mutex<RotatingFile>>);

impl Write for LogFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let record = String::from_utf8_lossy(buf);
        let mut file = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        file.write_record(&record)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedLogFile {
    type Writer = LogFileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogFileWriter(self.0.clone())
    }
}

/// Open `<log_dir>/plumdeck.log`, creating the directory.
pub fn open_log_file(log_dir: &Path) -> io::Result<RotatingFile> {
    std::fs::create_dir_all(log_dir)?;
    RotatingFile::open(&log_dir.join(LOG_FILE_NAME), MAX_BYTES, BACKUP_COUNT)
}

/// Install the global subscriber: stderr always, the log file when it can
/// be opened (otherwise the Python message on stderr and console only).
pub fn init(log_dir: &Path) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let console = tracing_subscriber::fmt::layer()
        .event_format(PythonFormat)
        .with_ansi(false)
        .with_writer(io::stderr);
    let file = match open_log_file(log_dir) {
        Ok(file) => Some(
            tracing_subscriber::fmt::layer()
                .event_format(PythonFormat)
                .with_ansi(false)
                .with_writer(SharedLogFile::new(file)),
        ),
        Err(error) => {
            eprintln!("Failed to set up file logging: {error}");
            None
        }
    };
    // The embedding app may already own the global subscriber.
    let _ = tracing_subscriber::registry().with(filter).with(console).with(file).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("plumdeck-logging-{name}-{}", std::process::id()));
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

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn rotates_like_rotating_file_handler() {
        let dir = Dir::new("rotate");
        let path = dir.0.join(LOG_FILE_NAME);
        let mut file = RotatingFile::open(&path, 20, 2).unwrap();
        // An empty file never rolls over, even for a record over the limit.
        file.write_record("0123456789012345678901234\n").unwrap();
        // 26 + 5 >= 20: roll over before writing.
        file.write_record("abcd\n").unwrap();
        assert_eq!(read(path.clone()), "abcd\n");
        assert_eq!(read(dir.0.join("plumdeck.log.1")), "0123456789012345678901234\n");
        // 5 + 10 < 20: append.
        file.write_record("123456789\n").unwrap();
        assert_eq!(read(path.clone()), "abcd\n123456789\n");
        // 15 + 5 >= 20: .1 -> .2, file -> .1.
        file.write_record("efgh\n").unwrap();
        assert_eq!(read(path.clone()), "efgh\n");
        assert_eq!(read(dir.0.join("plumdeck.log.1")), "abcd\n123456789\n");
        assert_eq!(read(dir.0.join("plumdeck.log.2")), "0123456789012345678901234\n");
        // Only two backups: the oldest is dropped.
        file.write_record("ijklmnopqrstuvwxyz\n").unwrap();
        file.write_record("tail\n").unwrap();
        assert_eq!(read(path.clone()), "tail\n");
        assert_eq!(read(dir.0.join("plumdeck.log.1")), "ijklmnopqrstuvwxyz\n");
        assert_eq!(read(dir.0.join("plumdeck.log.2")), "efgh\n");
        assert!(!dir.0.join("plumdeck.log.3").exists());
    }

    #[test]
    fn length_counts_characters_and_append_resumes_the_size() {
        let dir = Dir::new("chars");
        let path = dir.0.join(LOG_FILE_NAME);
        std::fs::write(&path, "x\n").unwrap();
        // Reopening resumes at the existing size (2 bytes).
        let mut file = RotatingFile::open(&path, 10, 1).unwrap();
        // "日本語日本\n" is 6 characters but 16 bytes: 2 + 6 < 10, no rollover.
        file.write_record("日本語日本\n").unwrap();
        assert_eq!(read(path.clone()), "x\n日本語日本\n");
        file.write_record("y\n").unwrap();
        assert_eq!(read(path), "y\n");
    }

    #[test]
    fn records_use_the_python_format() {
        let dir = Dir::new("format");
        let shared = SharedLogFile::new(open_log_file(&dir.0.join("logs")).unwrap());
        let subscriber = tracing_subscriber::fmt().with_ansi(false).event_format(PythonFormat).with_writer(shared).finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "infra.database.schema", "Initializing DuckDB schema...");
            tracing::warn!(target: "app", port = 8001, "listening");
        });
        let text = read(dir.0.join("logs").join(LOG_FILE_NAME));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        // 2026-10-28 12:34:56,789
        let stamp = &lines[0][..23];
        assert!(chrono::NaiveDateTime::parse_from_str(&stamp.replace(',', "."), "%Y-%m-%d %H:%M:%S%.3f").is_ok(), "{stamp}");
        assert_eq!(&lines[0][23..], " - infra.database.schema - INFO - Initializing DuckDB schema...");
        assert_eq!(&lines[1][23..], " - app - WARNING - listening port=8001");
    }
}
