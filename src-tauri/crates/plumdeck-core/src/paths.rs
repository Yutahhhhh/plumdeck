//! User data locations, resolved exactly like backend/config.py does with
//! `platformdirs.user_data_dir("plumdeck", "plumdeck")` (platformdirs 4.x):
//!
//! - macOS: `$XDG_DATA_HOME/plumdeck` if that is an absolute path, else
//!   `~/Library/Application Support/plumdeck`
//! - Linux: `$XDG_DATA_HOME/plumdeck`, else `~/.local/share/plumdeck`
//! - Windows: `<LocalAppData>\plumdeck\plumdeck`
//!
//! Functions take an env lookup so every platform's rule is testable anywhere.

use std::path::PathBuf;

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
}
