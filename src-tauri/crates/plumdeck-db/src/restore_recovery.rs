//! Crash recovery for Python's two-file library/analysis-queue restore unit.
//!
//! Invoke only before opening a library, or while a closed `MaintenanceGuard`
//! excludes all library and queue users. Keep the rollback pair until recovery
//! succeeds; retrying after an interrupted copy is safe.

use std::ffi::OsString;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::DbError;

fn appended(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    value.into()
}

pub fn marker_path(db_path: &Path) -> PathBuf {
    appended(db_path, ".restore-state.json")
}

pub fn queue_path(db_path: &Path) -> PathBuf {
    appended(db_path, ".analysis-jobs.sqlite3")
}

fn sync(path: &Path) -> io::Result<()> {
    // Windows FlushFileBuffers requires a writable handle, as Python's helper.
    OpenOptions::new().read(true).write(true).open(path)?.sync_all()
}

// Persist directory entry changes before declaring recovery complete.
#[cfg(not(windows))]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()
}

// Windows file replacement is write-through; directory flushing requires a
// platform-specific handle and remains a separate durability limitation.
#[cfg(windows)]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_with_times(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::metadata(source)?;
    fs::copy(source, destination)?;
    let mut times = FileTimes::new();
    if let Ok(time) = metadata.accessed() {
        times = times.set_accessed(time);
    }
    if let Ok(time) = metadata.modified() {
        times = times.set_modified(time);
    }
    OpenOptions::new().write(true).open(destination)?.set_times(times)
}

#[cfg(not(windows))]
fn durable_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)?;
    File::open(
        destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()
}

#[cfg(windows)]
fn durable_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    // MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH, matching Python.
    let succeeded = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Persist rollback files before installing their recovery marker. The marker
/// stores only a sibling filename, never an arbitrary destination path.
pub fn begin_restore(db_path: &Path, rollback: &Path, queue_existed: bool) -> Result<(), DbError> {
    let name = rollback
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(DbError::InvalidRestoreState)?;
    validate_name(db_path, name)?;
    if rollback.parent() != db_path.parent() {
        return Err(DbError::InvalidRestoreState);
    }
    sync(rollback)?;
    if queue_existed {
        sync(&queue_path(rollback))?;
    }
    let marker = marker_path(db_path);
    let temporary = marker.with_extension("partial");
    let body = json!({"rollback": name, "queue_existed": queue_existed});
    fs::write(&temporary, serde_json::to_vec(&body)?)?;
    sync(&temporary)?;
    durable_replace(&temporary, &marker)?;
    Ok(())
}

pub fn sync_restored_files(db_path: &Path) -> Result<(), DbError> {
    sync(db_path)?;
    let queue = queue_path(db_path);
    if queue.exists() {
        sync(&queue)?;
    }
    Ok(())
}

pub fn finish_restore(db_path: &Path) -> Result<(), DbError> {
    // Persist replacement/removal directory entries before removing the
    // recovery marker, then persist the removal of the marker itself.
    sync_parent(db_path)?;
    remove_if_present(&marker_path(db_path))?;
    sync_parent(db_path)?;
    Ok(())
}

fn validate_name(db_path: &Path, name: &str) -> Result<(), DbError> {
    let filename = db_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(DbError::InvalidRestoreState)?;
    if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
        || !name.starts_with(&format!("{filename}.pre-restore-"))
    {
        return Err(DbError::InvalidRestoreState);
    }
    Ok(())
}

/// Restore both original files after a crash between their replacements.
/// Missing/invalid rollback data fails before touching either destination.
pub fn recover_pending_restore(db_path: &Path) -> Result<bool, DbError> {
    let marker = marker_path(db_path);
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let state: Value = serde_json::from_slice(&bytes)?;
    let name = state["rollback"].as_str().ok_or(DbError::InvalidRestoreState)?;
    validate_name(db_path, name)?;
    let queue_existed = state["queue_existed"]
        .as_bool()
        .ok_or(DbError::InvalidRestoreState)?;
    let rollback = db_path.with_file_name(name);
    let queue_rollback = queue_path(&rollback);
    if !rollback.is_file() || (queue_existed && !queue_rollback.is_file()) {
        return Err(DbError::MissingRollback);
    }
    copy_with_times(&rollback, db_path)?;
    remove_if_present(&appended(db_path, ".wal"))?;
    let queue = queue_path(db_path);
    for suffix in ["", "-wal", "-shm"] {
        remove_if_present(&appended(&queue, suffix))?;
    }
    if queue_existed {
        copy_with_times(&queue_rollback, &queue)?;
    }
    sync_restored_files(db_path)?;
    finish_restore(db_path)?;
    Ok(true)
}
