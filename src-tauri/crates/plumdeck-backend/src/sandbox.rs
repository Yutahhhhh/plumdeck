//! `plumdeck-backend --parity`: side B of a parity run must only ever touch
//! sandbox copies, the same rule tools/parity/parity_env.py enforces for the
//! Python oracle.

use std::path::{Path, PathBuf};

use plumdeck_core::config::Config;
use plumdeck_core::paths::is_within;

/// Variables a parity run must set explicitly: no platform default may be
/// used, and rekordbox must be this side's copy (or a missing file).
pub const REQUIRED_VARS: [&str; 3] = ["DB_PATH", "USER_DATA_DIR", "PLUMDECK_REKORDBOX_DB"];

/// backend/config.py Settings fields. pydantic-settings matches them in any
/// case, so `db_path` would silently override DB_PATH.
const SETTINGS_FIELDS: [&str; 13] = [
    "APP_NAME",
    "APP_AUTHOR",
    "ENV",
    "USER_DATA_DIR",
    "DB_PATH",
    "MUSIC_DIR",
    "PLUMDECK_PORT",
    "FRONTEND_PORT",
    "TF_CPP_MIN_LOG_LEVEL",
    "NUM_WORKERS",
    "PLUMDECK_LOG_DIR",
    "NUMBA_CACHE_DIR",
    "MPLCONFIGDIR",
];

fn refuse_inside(label: &str, path: &Path, roots: &[PathBuf]) -> Result<(), String> {
    match roots.iter().find(|root| is_within(path, root)) {
        Some(root) => Err(format!("{label}={} is inside the live dir {}", path.display(), root.display())),
        None => Ok(()),
    }
}

/// Refuse unless every location `config` resolved is outside `live_data`
/// (and rekordbox outside `live_rekordbox` too).
pub fn check_parity_sandbox(
    vars: &[(String, String)],
    config: &Config,
    live_data: &[PathBuf],
    live_rekordbox: &[PathBuf],
) -> Result<(), String> {
    // Windows env names are case-insensitive already; elsewhere a lowercase
    // twin is a second, conflicting value.
    if !cfg!(windows) {
        for (name, _) in vars {
            if SETTINGS_FIELDS.iter().any(|field| field.eq_ignore_ascii_case(name) && field != name) {
                return Err(format!("{name} would override {} (matched in any case); unset it", name.to_uppercase()));
            }
        }
    }
    for name in REQUIRED_VARS {
        if !vars.iter().any(|(key, value)| key == name && !value.is_empty()) {
            return Err(format!("{name} must be set to a sandbox path"));
        }
    }
    refuse_inside("USER_DATA_DIR", &config.user_data_dir, live_data)?;
    refuse_inside("DB_PATH", &config.db_path, live_data)?;
    refuse_inside("PLUMDECK_LOG_DIR", &config.log_dir, live_data)?;
    if let Some(rekordbox) = &config.rekordbox_db {
        let roots: Vec<PathBuf> = live_data.iter().chain(live_rekordbox).cloned().collect();
        refuse_inside("PLUMDECK_REKORDBOX_DB", rekordbox, &roots)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, String)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    fn check(pairs: &[(&str, String)], live: &Path) -> Result<(), String> {
        let vars = vars(pairs);
        let config = Config::from_vars(&vars, || live.to_path_buf()).unwrap();
        check_parity_sandbox(&vars, &config, &[live.to_path_buf()], &[live.join("Pioneer")])
    }

    #[test]
    fn parity_mode_refuses_the_live_library() {
        let base = std::env::temp_dir().join(format!("plumdeck-sandbox-{}", std::process::id()));
        let (live, sandbox) = (base.join("live"), base.join("sandbox"));
        let at = |root: &Path, name: &str| root.join(name).to_string_lossy().into_owned();
        let good = [
            ("DB_PATH", at(&sandbox, "ud/plumdeck.duckdb")),
            ("USER_DATA_DIR", at(&sandbox, "ud")),
            ("PLUMDECK_REKORDBOX_DB", at(&sandbox, "rb/master.db")),
        ];
        assert_eq!(check(&good, &live), Ok(()));
        // Every required variable must be explicit.
        assert!(check(&good[..2], &live).unwrap_err().contains("PLUMDECK_REKORDBOX_DB"));
        // The live library, its logs and rekordbox's own folder are refused.
        let mut bad = good.clone();
        bad[0].1 = at(&live, "plumdeck.duckdb");
        assert!(check(&bad, &live).unwrap_err().starts_with("DB_PATH="));
        let mut logs = good.to_vec();
        logs.push(("PLUMDECK_LOG_DIR", at(&live, "logs")));
        assert!(check(&logs, &live).unwrap_err().starts_with("PLUMDECK_LOG_DIR="));
        let mut rekordbox = good.clone();
        rekordbox[2].1 = at(&live, "Pioneer/rekordbox/master.db");
        assert!(check(&rekordbox, &live).unwrap_err().starts_with("PLUMDECK_REKORDBOX_DB="));
        if !cfg!(windows) {
            let mut twin = good.to_vec();
            twin.push(("db_path", at(&live, "plumdeck.duckdb")));
            assert!(check(&twin, &live).unwrap_err().contains("db_path would override DB_PATH"));
        }
    }
}
