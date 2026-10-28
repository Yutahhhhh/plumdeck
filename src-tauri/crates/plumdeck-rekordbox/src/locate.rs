//! Where rekordbox keeps its library (`infra/rekordbox_grid.py`).

use std::path::{Path, PathBuf};

/// Environment variable that points at a specific `master.db`.
pub const DB_ENV: &str = "PLUMDECK_REKORDBOX_DB";

/// `master_db_path`: `PLUMDECK_REKORDBOX_DB` when set, else the per-OS
/// default (`~/Library/Pioneer/rekordbox/master.db` on macOS,
/// `%APPDATA%\Pioneer\rekordbox\master.db` on Windows). `None` elsewhere.
pub fn master_db_path(env: &dyn Fn(&str) -> Option<String>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(explicit) = env(DB_ENV).filter(|v| !v.is_empty()) {
        return Some(expand_user(&explicit, home));
    }
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Pioneer/rekordbox/master.db"));
    }
    if cfg!(windows) {
        return env("APPDATA")
            .filter(|v| !v.is_empty())
            .map(|appdata| PathBuf::from(appdata).join("Pioneer").join("rekordbox").join("master.db"));
    }
    None
}

/// [`master_db_path`] with the process environment and home directory.
pub fn default_master_db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    master_db_path(&|name| std::env::var(name).ok(), home.as_deref())
}

fn expand_user(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// The analysis file for a `djmdContent.AnalysisDataPath` value, resolved like
/// `find_analysis`: the stored path itself when it is a file, else relative to
/// `<library>/share`, else relative to the library folder. `None` when no
/// candidate is a file.
pub fn resolve_analysis_path(database: &Path, raw: &str) -> Option<PathBuf> {
    resolve_analysis_path_in(database.parent()?, raw)
}

/// [`resolve_analysis_path`] given the library folder (the parent of
/// `master.db`) directly; useful when reading a copy of `master.db`.
pub fn resolve_analysis_path_in(library_dir: &Path, raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let raw = raw.replace('\\', "/");
    let direct = PathBuf::from(&raw);
    if direct.is_file() {
        return Some(direct);
    }
    let relative = raw.trim_start_matches('/');
    [library_dir.join("share").join(relative), library_dir.join(relative)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The analysis files of one track: `ANLZ0000.DAT` and its siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnlzKind {
    Dat,
    Ext,
    Ex2,
    Ex3,
}

impl AnlzKind {
    pub const fn extension(self) -> &'static str {
        match self {
            AnlzKind::Dat => "DAT",
            AnlzKind::Ext => "EXT",
            AnlzKind::Ex2 => "2EX",
            AnlzKind::Ex3 => "3EX",
        }
    }
}

/// `path` with its extension replaced by the sibling `kind` (the file may not
/// exist: older analyses have no `.2EX`, streaming tracks often none at all).
pub fn sibling(path: &Path, kind: AnlzKind) -> PathBuf {
    path.with_extension(kind.extension())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_db_wins_and_expands_tilde() {
        let env = |name: &str| (name == DB_ENV).then(|| "~/rb/master.db".to_owned());
        assert_eq!(
            master_db_path(&env, Some(Path::new("/home/u"))).unwrap(),
            PathBuf::from("/home/u/rb/master.db")
        );
    }

    #[test]
    fn missing_analysis_resolves_to_none() {
        assert_eq!(resolve_analysis_path(Path::new("/nonexistent/master.db"), "/PIONEER/x.DAT"), None);
        assert_eq!(resolve_analysis_path(Path::new("/nonexistent/master.db"), ""), None);
    }

    #[test]
    fn sibling_swaps_extension() {
        let dat = Path::new("/s/PIONEER/USBANLZ/abc/def/ANLZ0000.DAT");
        assert_eq!(sibling(dat, AnlzKind::Ex2), PathBuf::from("/s/PIONEER/USBANLZ/abc/def/ANLZ0000.2EX"));
    }
}
