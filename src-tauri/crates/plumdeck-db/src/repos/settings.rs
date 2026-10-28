//! The `settings` table (backend/infra/repositories/setting_repository.py,
//! app/services/setting_app_service.py).

use duckdb::{params, Connection};
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
