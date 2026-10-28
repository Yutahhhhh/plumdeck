//! The `lyrics` table behind `GET /api/tracks/{id}/lyrics` and
//! `GET /api/lyrics/search` (backend/api/routers/lyrics.py), and the
//! restored `PUT /api/tracks/{id}/lyrics` (deviation D-02, with
//! `register_lyrics`' write rules).

use chrono::NaiveDateTime;
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection, OptionalExt};

use super::tracks::{BigInt, TrackRow, TRACK_COLUMNS};
use crate::DbError;

/// A `lyrics` row as the `Lyrics` model loads it.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricsRow {
    pub track_id: i64,
    pub content: Option<String>,
    pub source: Option<String>,
    pub language: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

/// `session.get(Lyrics, track_id)`.
pub fn get_lyrics(conn: &Connection, track_id: BigInt) -> Result<Option<LyricsRow>, DbError> {
    let id = match i64::try_from(track_id.0) {
        Ok(id) => Value::BigInt(id),
        Err(_) => Value::HugeInt(track_id.0),
    };
    Ok(conn
        .query_row(
            "SELECT lyrics.track_id, lyrics.content, lyrics.source, lyrics.language, lyrics.created_at, lyrics.updated_at \
             FROM lyrics WHERE lyrics.track_id = ?",
            [id],
            |row| {
                Ok(LyricsRow {
                    track_id: row.get(0)?,
                    content: row.get(1)?,
                    source: row.get(2)?,
                    language: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()?)
}

/// `PUT /api/tracks/{track_id}/lyrics` (D-02): store the user's lyrics the
/// way `register_lyrics(overwrite=True)` does: `source` is `'user'`, the
/// keyword cache is cleared and `updated_at` (and `created_at` for a new
/// row) is `now`; a row that already holds the same content, source and
/// language is left untouched. `None` when the track does not exist.
pub fn put_lyrics(
    conn: &Connection,
    track_id: BigInt,
    content: &str,
    language: Option<&str>,
    now: NaiveDateTime,
) -> Result<Option<LyricsRow>, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let Some(track) = super::tracks::get_track(conn, track_id)? else { return Ok(None) };
        let existing = get_lyrics(conn, track_id)?;
        let unchanged = existing.as_ref().is_some_and(|row| {
            row.content.as_deref() == Some(content) && row.source.as_deref() == Some("user") && row.language.as_deref() == language
        });
        if !unchanged {
            if existing.is_some() {
                conn.execute(
                    "UPDATE lyrics SET content=?, source='user', language=?, keywords_json=NULL, \
                     keywords_content_hash=NULL, updated_at=? WHERE lyrics.track_id = ?",
                    params![content, language, now, track.id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO lyrics (track_id, content, source, language, keywords_json, keywords_content_hash, \
                     created_at, updated_at) VALUES (?, ?, 'user', ?, NULL, NULL, ?, ?)",
                    params![track.id, content, language, now, now],
                )?;
            }
        }
        get_lyrics(conn, track_id)
    })();
    super::settings::finish(conn, result)
}

/// `select(Lyrics, Track).join(Track, ...).where(Lyrics.content.ilike(...))`
/// (and `Track.id != exclude_track_id` when it is truthy): each match's
/// lyrics text with its track, in DuckDB's order (no ORDER BY, as Python).
pub fn search_lyrics(
    conn: &Connection,
    q: &str,
    exclude_track_id: Option<BigInt>,
) -> Result<Vec<(String, TrackRow)>, DbError> {
    let mut sql = format!(
        "SELECT lyrics.content, {TRACK_COLUMNS} FROM lyrics JOIN tracks ON lyrics.track_id = tracks.id \
         WHERE lyrics.content ILIKE ?"
    );
    let mut params = vec![Value::Text(format!("%{q}%"))];
    if let Some(exclude) = exclude_track_id.filter(|id| id.0 != 0) {
        sql.push_str(" AND tracks.id != ?");
        params.push(match i64::try_from(exclude.0) {
            Ok(id) => Value::BigInt(id),
            Err(_) => Value::HugeInt(exclude.0),
        });
    }
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter(params), |row| Ok((row.get(0)?, TrackRow::from_row(row, 1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_searches() {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, title) VALUES ('/a', 'A'), ('/b', 'B');
             INSERT INTO lyrics (track_id, content, created_at, updated_at) VALUES
               (1, 'Hello\nWorld', '2026-01-01 00:00:00', '2026-01-01 00:00:00'), (2, 'hello again', NULL, NULL);",
        )
        .unwrap();
        let row = get_lyrics(&conn, BigInt(1)).unwrap().unwrap();
        assert_eq!((row.content.as_deref(), row.source.as_deref()), (Some("Hello\nWorld"), Some("user")));
        assert_eq!(get_lyrics(&conn, BigInt(3)).unwrap(), None);
        let mut found: Vec<i64> = search_lyrics(&conn, "HELLO", None).unwrap().iter().map(|(_, t)| t.id).collect();
        found.sort();
        assert_eq!(found, [1, 2]);
        assert_eq!(search_lyrics(&conn, "hello", Some(BigInt(1))).unwrap().len(), 1);
        assert_eq!(search_lyrics(&conn, "hello", Some(BigInt(0))).unwrap().len(), 2);
    }

    #[test]
    fn put_creates_updates_and_skips_unchanged() {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, title) VALUES ('/a', 'A'), ('/b', 'B');
             INSERT INTO lyrics (track_id, content, source, keywords_json, keywords_content_hash, created_at, updated_at)
               VALUES (1, 'old', 'lrclib', '[]', 'h', '2026-01-01 00:00:00', '2026-01-01 00:00:00');",
        )
        .unwrap();
        let at = |s: u32| chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap().and_hms_opt(0, 0, s).unwrap();
        let row = put_lyrics(&conn, BigInt(1), "new", None, at(1)).unwrap().unwrap();
        assert_eq!((row.content.as_deref(), row.source.as_deref(), row.updated_at), (Some("new"), Some("user"), Some(at(1))));
        let keywords: Option<String> = conn.query_row("SELECT keywords_json FROM lyrics WHERE track_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(keywords, None);
        // Same content again: untouched.
        assert_eq!(put_lyrics(&conn, BigInt(1), "new", None, at(2)).unwrap().unwrap().updated_at, Some(at(1)));
        let created = put_lyrics(&conn, BigInt(2), "", Some("ja"), at(3)).unwrap().unwrap();
        assert_eq!((created.created_at, created.language.as_deref()), (Some(at(3)), Some("ja")));
        assert_eq!(put_lyrics(&conn, BigInt(9), "x", None, at(4)).unwrap(), None);
    }
}
