//! User data locations, resolved exactly like backend/config.py does with
//! `platformdirs.user_data_dir("plumdeck", "plumdeck")` (platformdirs 4.x):
//!
//! - macOS: `$XDG_DATA_HOME/plumdeck` if that is an absolute path, else
//!   `~/Library/Application Support/plumdeck`
//! - Linux: `$XDG_DATA_HOME/plumdeck`, else `~/.local/share/plumdeck`
//! - Windows: `<LocalAppData>\plumdeck\plumdeck`
//!
//! Functions take an env lookup so every platform's rule is testable anywhere.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

pub const APP_NAME: &str = "plumdeck";
pub const APP_AUTHOR: &str = "plumdeck";

/// The platformdirs flavour to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// `posixpath.join(base, name)` for a relative `name`.
fn posix_join(base: &str, name: &str) -> String {
    if base.is_empty() || base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// `ntpath.join(base, name)` for a relative, drive-less `name`.
fn nt_join(base: &str, name: &str) -> String {
    let drive_only = base.len() == 2 && base.as_bytes()[1] == b':';
    if base.is_empty() || base.ends_with(['\\', '/']) || drive_only {
        format!("{base}{name}")
    } else {
        format!("{base}\\{name}")
    }
}

/// `posixpath.expanduser("~" + suffix)` given `$HOME`.
fn expand_home(home: &str, suffix: &str) -> String {
    let joined = format!("{}{}", home.trim_end_matches('/'), suffix);
    if joined.is_empty() { "/".into() } else { joined }
}

/// platformdirs' `user_data_dir(APP_NAME, APP_AUTHOR)` for `platform`.
///
/// `env` looks up environment variables; `home` is `~` on Unix. On Windows
/// the local AppData folder comes from `WIN_PD_OVERRIDE_LOCAL_APPDATA`, then
/// `LOCALAPPDATA` (Python asks SHGetKnownFolderPath, which agrees unless the
/// variable was changed by hand).
pub fn user_data_dir_for(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    home: &str,
) -> Option<String> {
    match platform {
        Platform::MacOs | Platform::Linux => {
            let xdg = env("XDG_DATA_HOME").unwrap_or_default();
            let xdg = xdg.trim();
            if xdg.starts_with('/') {
                return Some(posix_join(xdg, APP_NAME));
            }
            let base = if platform == Platform::MacOs {
                expand_home(home, "/Library/Application Support")
            } else {
                expand_home(home, "/.local/share")
            };
            Some(posix_join(&base, APP_NAME))
        }
        Platform::Windows => {
            let override_dir = env("WIN_PD_OVERRIDE_LOCAL_APPDATA").unwrap_or_default();
            let base = if !override_dir.trim().is_empty() {
                override_dir.trim().to_owned()
            } else {
                env("LOCALAPPDATA").filter(|value| !value.is_empty())?
            };
            let base = crate::compat::pathnorm::nt_normpath(&base);
            Some(nt_join(&nt_join(&base, APP_AUTHOR), APP_NAME))
        }
    }
}

/// The platform default user data dir for this process.
pub fn default_user_data_dir() -> PathBuf {
    let env = |name: &str| std::env::var(name).ok();
    let home = std::env::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let platform = Platform::current();
    let fallback = || {
        // No LocalAppData at all: mirror the user-profile layout.
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        nt_join(&nt_join(&nt_join(&nt_join(&profile, "AppData"), "Local"), APP_AUTHOR), APP_NAME)
    };
    PathBuf::from(user_data_dir_for(platform, &env, &home).unwrap_or_else(fallback))
}

/// Every place the live library can be, for the parity sandbox guard:
/// platformdirs' answer with and without `XDG_DATA_HOME`, plus each OS's
/// fixed default (whatever OS this runs on).
pub fn live_data_dirs(env: &dyn Fn(&str) -> Option<String>, home: &str) -> Vec<PathBuf> {
    let platform = Platform::current();
    let no_xdg = |name: &str| if name == "XDG_DATA_HOME" { None } else { env(name) };
    let mut dirs: Vec<PathBuf> = [user_data_dir_for(platform, env, home), user_data_dir_for(platform, &no_xdg, home)]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect();
    let home = PathBuf::from(home);
    dirs.push(home.join("Library").join("Application Support").join(APP_NAME));
    dirs.push(home.join(".local").join("share").join(APP_NAME));
    dirs.push(home.join("AppData").join("Local").join(APP_AUTHOR).join(APP_NAME));
    if let Some(local) = env("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(local).join(APP_AUTHOR).join(APP_NAME));
    }
    dedup(dirs)
}

/// rekordbox's own library folders (infra/rekordbox_grid.py master_db_path).
pub fn live_rekordbox_dirs(env: &dyn Fn(&str) -> Option<String>, home: &str) -> Vec<PathBuf> {
    let home = PathBuf::from(home);
    let mut dirs = vec![home.join("Library").join("Pioneer"), home.join("AppData").join("Roaming").join("Pioneer")];
    if let Some(appdata) = env("APPDATA").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(appdata).join("Pioneer"));
    }
    dedup(dirs)
}

fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// `path` made absolute with symlinks resolved as far as it exists, like
/// Python's non-strict `Path.resolve()`.
pub fn resolve_lenient(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.as_path();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for part in rest.iter().rev() {
                match Path::new(part).components().next() {
                    Some(Component::ParentDir) => {
                        out.pop();
                    }
                    Some(Component::CurDir) => {}
                    _ => out.push(part),
                }
            }
            return out;
        }
        match (existing.parent(), existing.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last.as_os_str().to_owned());
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

/// Whether `path` is `root` or inside it, after resolving both. APFS and NTFS
/// are case-insensitive by default, so macOS and Windows compare NFC-casefolded.
pub fn is_within(path: &Path, root: &Path) -> bool {
    let fold = |p: &Path| {
        let text = resolve_lenient(p).to_string_lossy().into_owned();
        if cfg!(any(target_os = "macos", windows)) {
            PathBuf::from(crate::compat::text::casefold(&crate::compat::text::nfc(&text)))
        } else {
            PathBuf::from(text)
        }
    };
    fold(path).starts_with(fold(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn platformdirs_layouts() {
        let none = lookup(&[]);
        assert_eq!(
            user_data_dir_for(Platform::MacOs, &none, "/Users/dj/").unwrap(),
            "/Users/dj/Library/Application Support/plumdeck"
        );
        assert_eq!(user_data_dir_for(Platform::Linux, &none, "/home/dj").unwrap(), "/home/dj/.local/share/plumdeck");
        let xdg = lookup(&[("XDG_DATA_HOME", " /data/ ")]);
        assert_eq!(user_data_dir_for(Platform::MacOs, &xdg, "/Users/dj").unwrap(), "/data/plumdeck");
        let relative = lookup(&[("XDG_DATA_HOME", "data")]);
        assert_eq!(user_data_dir_for(Platform::Linux, &relative, "/h").unwrap(), "/h/.local/share/plumdeck");
        let win = lookup(&[("LOCALAPPDATA", "C:/Users/dj/AppData/Local/")]);
        assert_eq!(
            user_data_dir_for(Platform::Windows, &win, "").unwrap(),
            "C:\\Users\\dj\\AppData\\Local\\plumdeck\\plumdeck"
        );
        assert_eq!(user_data_dir_for(Platform::Windows, &none, ""), None);
    }

    #[test]
    fn live_dirs_include_fixed_defaults_despite_xdg() {
        let xdg = lookup(&[("XDG_DATA_HOME", "/sandbox/xdg"), ("LOCALAPPDATA", "/la")]);
        let dirs = live_data_dirs(&xdg, "/Users/dj");
        let home = PathBuf::from("/Users/dj");
        for want in [
            home.join("Library").join("Application Support").join("plumdeck"),
            home.join(".local").join("share").join("plumdeck"),
            PathBuf::from("/la").join("plumdeck").join("plumdeck"),
        ] {
            assert!(dirs.contains(&want), "{want:?} missing from {dirs:?}");
        }
        if !cfg!(windows) {
            assert!(dirs.contains(&PathBuf::from("/sandbox/xdg/plumdeck")), "{dirs:?}");
        }
    }

    #[test]
    fn within_resolves_symlinks_and_dot_dot() {
        let base = std::env::temp_dir().join(format!("plumdeck-within-{}", std::process::id()));
        let live = base.join("live");
        let sandbox = base.join("sandbox");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::create_dir_all(&sandbox).unwrap();
        assert!(is_within(&live.join("plumdeck.duckdb"), &live));
        assert!(is_within(&live, &live));
        assert!(!is_within(&sandbox.join("plumdeck.duckdb"), &live));
        assert!(is_within(&sandbox.join("..").join("live").join("x"), &live));
        assert!(!is_within(&base.join("live-other"), &live));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&live, sandbox.join("link")).unwrap();
            assert!(is_within(&sandbox.join("link").join("new").join("db"), &live));
        }
        if cfg!(target_os = "macos") {
            let upper = PathBuf::from(live.to_string_lossy().to_uppercase());
            assert!(is_within(&upper.join("x"), &live));
        }
        std::fs::remove_dir_all(&base).ok();
    }
}
