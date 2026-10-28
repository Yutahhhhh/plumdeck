//! Backend settings resolved from the environment with backend/config.py's
//! rules (pydantic-settings: field names match env vars case-insensitively;
//! DB_PATH and PLUMDECK_LOG_DIR fall back when unset or empty).
//!
//! `.env` files are not read: the shipped (frozen) Python backend ignores them.

use std::fmt;
use std::path::PathBuf;

use crate::paths;

/// Port the desktop app uses in debug builds (and the Python default).
pub const DEV_PORT: u16 = 8001;
/// Port the desktop app uses in release builds (src-tauri/src/lib.rs).
pub const PROD_PORT: u16 = 48123;
/// Vite dev server port allowed by CORS.
pub const FRONTEND_PORT: u16 = 1420;

/// The port the desktop app passes to its backend for this build profile.
pub const fn app_port() -> u16 {
    if cfg!(debug_assertions) { DEV_PORT } else { PROD_PORT }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Settings shared by the HTTP server, the database and logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `ENV` (default "prod").
    pub env: String,
    /// `PLUMDECK_PORT` (server.py reads it again with the exact name).
    pub port: u16,
    /// `FRONTEND_PORT` (CORS).
    pub frontend_port: u16,
    pub user_data_dir: PathBuf,
    pub db_path: PathBuf,
    pub log_dir: PathBuf,
    /// `PLUMDECK_REKORDBOX_DB`: overrides the rekordbox master.db location.
    pub rekordbox_db: Option<PathBuf>,
    /// `PLUMDECK_MANAGED_SIDECAR=1`: stdin carries the desktop's shutdown line.
    pub managed_sidecar: bool,
}

/// Python's `int(text)` for a port number: surrounding whitespace, an
/// optional sign and single underscores between digits are accepted.
pub fn parse_py_int(text: &str) -> Option<i64> {
    let trimmed = crate::compat::text::py_strip(text);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'+') => (false, &trimmed[1..]),
        Some(b'-') => (true, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    if !cleaned.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i64 = cleaned.parse().ok()?;
    Some(if negative { -value } else { value })
}

fn parse_port(name: &str, value: &str) -> Result<u16, ConfigError> {
    parse_py_int(value)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| ConfigError(format!("{name} must be a port number, got {value:?}")))
}

impl Config {
    /// Resolve from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        Self::from_vars(&vars, paths::default_user_data_dir)
    }

    /// Resolve from explicit variables; `default_user_data_dir` is only called
    /// when USER_DATA_DIR is absent.
    pub fn from_vars(
        vars: &[(String, String)],
        default_user_data_dir: impl FnOnce() -> PathBuf,
    ) -> Result<Self, ConfigError> {
        let exact = |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        // pydantic-settings lowercases env names before matching fields.
        let field = |name: &str| {
            exact(name).or_else(|| {
                vars.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
            })
        };
        let non_empty = |name: &str| field(name).filter(|v| !v.is_empty());

        let user_data_dir = field("USER_DATA_DIR").map(PathBuf::from).unwrap_or_else(default_user_data_dir);
        let db_path = non_empty("DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_data_dir.join("plumdeck.duckdb"));
        let log_dir = non_empty("PLUMDECK_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_data_dir.join("logs"));
        let settings_port = match field("PLUMDECK_PORT") {
            Some(value) => parse_port("PLUMDECK_PORT", &value)?,
            None => DEV_PORT,
        };
        // server.py: int(os.environ.get("PLUMDECK_PORT", settings.PLUMDECK_PORT)).
        let port = match exact("PLUMDECK_PORT") {
            Some(value) => parse_port("PLUMDECK_PORT", &value)?,
            None => settings_port,
        };
        let frontend_port = match field("FRONTEND_PORT") {
            Some(value) => parse_port("FRONTEND_PORT", &value)?,
            None => FRONTEND_PORT,
        };
        Ok(Self {
            env: field("ENV").unwrap_or_else(|| "prod".into()),
            port,
            frontend_port,
            user_data_dir,
            db_path,
            log_dir,
            rekordbox_db: exact("PLUMDECK_REKORDBOX_DB").filter(|v| !v.is_empty()).map(PathBuf::from),
            managed_sidecar: exact("PLUMDECK_MANAGED_SIDECAR").as_deref() == Some("1"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn defaults_follow_user_data_dir() {
        let config = Config::from_vars(&vars(&[("USER_DATA_DIR", "/sandbox/ud")]), || unreachable!()).unwrap();
        assert_eq!(config.port, DEV_PORT);
        assert_eq!(config.db_path, PathBuf::from("/sandbox/ud/plumdeck.duckdb"));
        assert_eq!(config.log_dir, PathBuf::from("/sandbox/ud/logs"));
        assert_eq!(config.env, "prod");
        assert!(!config.managed_sidecar);
    }

    #[test]
    fn overrides_and_case_insensitive_fields() {
        let config = Config::from_vars(
            &vars(&[
                ("user_data_dir", "/u"),
                ("DB_PATH", ""),
                ("Plumdeck_Log_Dir", "/logs"),
                ("PLUMDECK_PORT", " 18_002 "),
                ("PLUMDECK_MANAGED_SIDECAR", "1"),
            ]),
            || unreachable!(),
        )
        .unwrap();
        assert_eq!(config.user_data_dir, PathBuf::from("/u"));
        assert_eq!(config.db_path, PathBuf::from("/u/plumdeck.duckdb"));
        assert_eq!(config.log_dir, PathBuf::from("/logs"));
        assert_eq!(config.port, 18002);
        assert!(config.managed_sidecar);
        assert!(Config::from_vars(&vars(&[("PLUMDECK_PORT", "80.5")]), PathBuf::new).is_err());
        assert_eq!(parse_py_int("+12"), Some(12));
        assert_eq!(parse_py_int("1__2"), None);
    }
}
