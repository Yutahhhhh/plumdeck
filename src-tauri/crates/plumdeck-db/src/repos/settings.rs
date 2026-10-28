//! The `settings` table (backend/infra/repositories/setting_repository.py,
//! app/services/setting_app_service.py).

use duckdb::{params, Connection, OptionalExt};
use plumdeck_core::compat::text::{py_lower, py_strip};

use crate::DbError;

/// `RETIRED_LLM_SETTING_KEYS`.
pub const RETIRED_LLM_SETTING_KEYS: &[&str] = &[
    "llm_provider",
    "llm_model",
    "api_key",
    "openai_api_key",
    "anthropic_api_key",
    "google_api_key",
    "ollama_host",
    "codex_cli_path",
    "codex_timeout_seconds",
    "openai_model",
    "codex_model",
    "anthropic_model",
    "google_model",
    "ollama_model",
];

/// `RETIRED_LLM_PROVIDERS`.
pub const RETIRED_LLM_PROVIDERS: &[&str] = &["openai", "anthropic", "google", "ollama", "codex"];

/// `is_retired_llm_setting_key`: model/API credentials plumdeck stored before
/// v0.4, including variants such as `LLM_*` or ` OpenAI_Model `.
pub fn is_retired_llm_setting_key(key: &str) -> bool {
    let normalized = py_lower(py_strip(key));
    if RETIRED_LLM_SETTING_KEYS.contains(&normalized.as_str()) || normalized.starts_with("llm_") {
        return true;
    }
    RETIRED_LLM_PROVIDERS.iter().any(|provider| {
        normalized == format!("{provider}_model") || normalized == format!("{provider}_api_key")
    })
}

/// The startup cleanup in `init_db`: delete every retired LLM setting in one
/// transaction. Returns the number of deleted rows.
pub fn delete_retired_llm_settings(conn: &Connection) -> Result<usize, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let keys: Vec<String> = conn
            .prepare("SELECT key FROM settings")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let mut deleted = 0;
        for key in keys.iter().filter(|key| is_retired_llm_setting_key(key)) {
            deleted += conn.execute("DELETE FROM settings WHERE key = ?", params![key])?;
        }
        Ok(deleted)
    })();
    finish(conn, result)
}

/// `SettingRepository.find_all` filtered by `SettingAppService.get_settings`:
/// every `(key, value)` that is not a retired LLM setting, in table order
/// (no ORDER BY, as in Python).
pub fn visible_settings(conn: &Connection) -> Result<Vec<(String, String)>, DbError> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT key, value FROM settings")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows.into_iter().filter(|(key, _)| !is_retired_llm_setting_key(key)).collect())
}

/// `get_setting_value(session, key, default)` without the default: the
/// stored value of `key` (exact match), if any.
pub fn setting_value(conn: &Connection, key: &str) -> Result<Option<String>, DbError> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?", params![key], |row| row.get(0)).optional()?)
}

/// The error `SettingAppService.update_setting` raises for a retired key
/// (answered as 400).
pub const RETIRED_SETTING_MESSAGE: &str = "plumdeck no longer stores LLM credentials or model settings; \
     AI reasoning is provided by the connected MCP client.";

/// `SettingAppService.update_setting`: refuse retired LLM keys, otherwise
/// insert the row or replace its value, in one transaction. Returns the
/// stored `(key, value)`.
///
/// Call under `DbHandle::write` so the read-then-write cannot race (Python
/// answered 500 when two requests inserted the same new key at once).
pub fn save_setting(conn: &Connection, key: &str, value: &str) -> Result<(String, String), DbError> {
    if is_retired_llm_setting_key(key) {
        return Err(DbError::Invalid(RETIRED_SETTING_MESSAGE.into()));
    }
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        if setting_value(conn, key)?.is_some() {
            conn.execute("UPDATE settings SET value = ? WHERE key = ?", params![value, key])?;
        } else {
            conn.execute("INSERT INTO settings (key, value) VALUES (?, ?)", params![key, value])?;
        }
        // session.refresh: answer what the row now holds.
        Ok(conn.query_row("SELECT key, value FROM settings WHERE key = ?", params![key], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?)
    })();
    finish(conn, result)
}

/// COMMIT on success, ROLLBACK on error.
pub(crate) fn finish<T>(conn: &Connection, result: Result<T, DbError>) -> Result<T, DbError> {
    match result {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_keys_match_python_rules() {
        for key in [
            "llm_provider",
            "LLM_MODEL_EXPERIMENTAL",
            " openai_api_key\n",
            "Anthropic_Model",
            "codex_cli_path",
            "ollama_api_key",
            "llm_",
            "\u{1f}api_key\u{85}",
        ] {
            assert!(is_retired_llm_setting_key(key), "{key:?}");
        }
        for key in ["root_path", "plumdeck.deckCount", "llm", "mistral_api_key", "openai", "", "api_keys"] {
            assert!(!is_retired_llm_setting_key(key), "{key:?}");
        }
    }

    #[test]
    fn save_inserts_updates_and_refuses_retired_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
             INSERT INTO settings VALUES ('root_path', '/music'), ('llm_provider', 'openai');",
        )
        .unwrap();
        assert_eq!(save_setting(&conn, "ui.theme", "dark").unwrap(), ("ui.theme".into(), "dark".into()));
        assert_eq!(save_setting(&conn, "root_path", "").unwrap(), ("root_path".into(), String::new()));
        assert_eq!(save_setting(&conn, "Root_Path", "x").unwrap().0, "Root_Path");
        let error = save_setting(&conn, " OpenAI_Model ", "gpt").unwrap_err();
        assert_eq!(error.to_string(), RETIRED_SETTING_MESSAGE);
        assert!(RETIRED_SETTING_MESSAGE.contains("settings; AI reasoning"));
        assert_eq!(setting_value(&conn, "root_path").unwrap().as_deref(), Some(""));
        assert_eq!(setting_value(&conn, "missing").unwrap(), None);
        let mut visible = visible_settings(&conn).unwrap();
        visible.sort();
        let keys: Vec<&str> = visible.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["Root_Path", "root_path", "ui.theme"]);
    }

    #[test]
    fn cleanup_deletes_only_retired_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);
             INSERT INTO settings VALUES ('llm_model_experimental', 'x'), ('root_path', '/music'),
                                         ('OpenAI_API_Key', 'secret'), ('ui.theme', 'dark');",
        )
        .unwrap();
        assert_eq!(delete_retired_llm_settings(&conn).unwrap(), 2);
        let left: Vec<String> = conn
            .prepare("SELECT key FROM settings ORDER BY key")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(left, ["root_path", "ui.theme"]);
    }
}
