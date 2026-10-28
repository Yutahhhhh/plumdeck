//! Genre queries and bulk updates (backend/infra/repositories/genre_repository.py
//! and the write paths of backend/app/services/genre_app_service.py).
//!
//! `get_unknown_tracks` pages with LIMIT/OFFSET and Python sends no ORDER
//! BY; here the pages are ordered by id (deviation D-01), which is the
//! order DuckDB scans a library in when nothing reorders it.

use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection};

use super::settings::finish;
use super::tracks::{tracks_by_ids, BigInt, TrackRow, TRACK_COLUMNS};
use crate::DbError;

/// `AnalysisMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisMode {
    Genre,
    Subgenre,
    Both,
}

impl AnalysisMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "genre" => Some(Self::Genre),
            "subgenre" => Some(Self::Subgenre),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

const UNKNOWN_GENRE: &str =
    "(tracks.is_genre_verified = false OR tracks.genre IS NULL OR tracks.genre = '' OR lower(tracks.genre) = 'unknown')";
const UNKNOWN_SUBGENRE: &str = "(tracks.subgenre IS NULL OR tracks.subgenre = '')";

fn unknown_condition(mode: AnalysisMode) -> String {
    match mode {
        AnalysisMode::Subgenre => UNKNOWN_SUBGENRE.into(),
        AnalysisMode::Both => format!(
            "(tracks.is_genre_verified = false OR tracks.genre IS NULL OR tracks.genre = '' \
             OR lower(tracks.genre) = 'unknown' OR tracks.subgenre IS NULL OR tracks.subgenre = '')"
        ),
        AnalysisMode::Genre => UNKNOWN_GENRE.into(),
    }
}

fn int_value(value: BigInt) -> Value {
    match i64::try_from(value.0) {
        Ok(small) => Value::BigInt(small),
        Err(_) => Value::HugeInt(value.0),
    }
}

/// `get_unknown_tracks(offset, limit, mode)`, ordered by id (D-01).
pub fn unknown_tracks(conn: &Connection, offset: BigInt, limit: BigInt, mode: AnalysisMode) -> Result<Vec<TrackRow>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS} FROM tracks WHERE {} ORDER BY tracks.id LIMIT ? OFFSET ?",
        unknown_condition(mode)
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter([int_value(limit), int_value(offset)]), |row| TrackRow::from_row(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `get_all_unknown_track_ids(mode)`, ordered by id (D-01).
pub fn unknown_track_ids(conn: &Connection, mode: AnalysisMode) -> Result<Vec<i64>, DbError> {
    let sql = format!("SELECT tracks.id FROM tracks WHERE {} ORDER BY tracks.id", unknown_condition(mode));
    let mut statement = conn.prepare(&sql)?;
    let ids = statement.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
    Ok(ids)
}

fn distinct_sorted(conn: &Connection, sql: &str) -> Result<Vec<String>, DbError> {
    let mut values: Vec<String> = conn.prepare(sql)?.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?;
    // Python's sorted(): code point order, which is UTF-8 byte order.
    values.sort();
    Ok(values)
}

/// `get_all_genres`: distinct genres except NULL, `''` and `'Unknown'`.
pub fn all_genres(conn: &Connection) -> Result<Vec<String>, DbError> {
    distinct_sorted(
        conn,
        "SELECT DISTINCT tracks.genre FROM tracks WHERE tracks.genre IS NOT NULL AND tracks.genre != '' AND tracks.genre != 'Unknown'",
    )
}

/// `get_all_subgenres`.
pub fn all_subgenres(conn: &Connection) -> Result<Vec<String>, DbError> {
    distinct_sorted(
        conn,
        "SELECT DISTINCT tracks.subgenre FROM tracks WHERE tracks.subgenre IS NOT NULL AND tracks.subgenre != ''",
    )
}

/// `get_all_tracks_with_genre`: tracks with a genre other than `'Unknown'`,
/// in table order.
pub fn tracks_with_genre(conn: &Connection) -> Result<Vec<TrackRow>, DbError> {
    let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE tracks.genre IS NOT NULL AND tracks.genre != 'Unknown'");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| TrackRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Why `batch_update_genres` refused (answered as 400).
pub const PARENT_NOT_FOUND: &str = "Parent track not found";
pub const PARENT_HAS_NO_GENRE: &str = "Parent track has no genre";

/// `batch_update_genres`: copy the parent's genre and subgenre to the
/// targets and mark them verified. Returns `(updated_count, genre)`.
pub fn batch_update_genres(conn: &Connection, parent_id: i64, target_ids: &[i64]) -> Result<(i64, String), DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let parent = super::tracks::get_track(conn, BigInt(parent_id.into()))?
            .ok_or_else(|| DbError::Invalid(PARENT_NOT_FOUND.into()))?;
        let genre = parent.genre.clone().filter(|genre| !genre.is_empty());
        let Some(genre) = genre else { return Err(DbError::Invalid(PARENT_HAS_NO_GENRE.into())) };
        let targets = tracks_by_ids(conn, target_ids)?;
        for target in &targets {
            conn.execute(
                "UPDATE tracks SET genre=?, subgenre=?, is_genre_verified=? WHERE tracks.id = ?",
                params![genre, parent.subgenre, true, target.id],
            )?;
        }
        Ok((targets.len() as i64, genre))
    })();
    finish(conn, result)
}

/// `execute_cleanup`: set the genre (or subgenre) of the listed tracks and
/// mark them verified. Returns the number of tracks found.
pub fn execute_cleanup(conn: &Connection, target: &str, track_ids: &[i64], mode: AnalysisMode) -> Result<i64, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let targets = tracks_by_ids(conn, track_ids)?;
        let column = if mode == AnalysisMode::Subgenre { "subgenre" } else { "genre" };
        for track in &targets {
            conn.execute(
                &format!("UPDATE tracks SET {column}=?, is_genre_verified=? WHERE tracks.id = ?"),
                params![target, true, track.id],
            )?;
        }
        Ok(targets.len() as i64)
    })();
    finish(conn, result)
}

/// One track's new genre fields from `apply_genre_analyses`.
#[derive(Clone, Debug, PartialEq)]
pub struct GenreWrite {
    pub track_id: i64,
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub is_genre_verified: bool,
}

/// Commit the final state of every changed track in one transaction (the
/// single `session.commit()` of `apply_genre_analyses`).
pub fn write_genres(conn: &Connection, writes: &[GenreWrite]) -> Result<(), DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        for write in writes {
            conn.execute(
                "UPDATE tracks SET genre=?, subgenre=?, is_genre_verified=? WHERE tracks.id = ?",
                params![write.genre, write.subgenre, write.is_genre_verified, write.track_id],
            )?;
        }
        Ok(())
    })();
    finish(conn, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, genre, subgenre, is_genre_verified) VALUES
               ('/1', 'House', 'Deep', true), ('/2', 'Unknown', '', true), ('/3', NULL, NULL, true),
               ('/4', 'house', 'Deep', false), ('/5', '', 'Tech', true), ('/6', 'Techno', 'Peak', true);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn unknown_lists_and_vocabularies() {
        let conn = library();
        assert_eq!(unknown_track_ids(&conn, AnalysisMode::Genre).unwrap(), [2, 3, 4, 5]);
        assert_eq!(unknown_track_ids(&conn, AnalysisMode::Subgenre).unwrap(), [2, 3]);
        assert_eq!(unknown_track_ids(&conn, AnalysisMode::Both).unwrap(), [2, 3, 4, 5]);
        let page = unknown_tracks(&conn, BigInt(1), BigInt(2), AnalysisMode::Genre).unwrap();
        assert_eq!(page.iter().map(|t| t.id).collect::<Vec<_>>(), [3, 4]);
        assert_eq!(all_genres(&conn).unwrap(), ["House", "Techno", "house"]);
        assert_eq!(all_subgenres(&conn).unwrap(), ["Deep", "Peak", "Tech"]);
        assert_eq!(tracks_with_genre(&conn).unwrap().len(), 4);
        assert_eq!(AnalysisMode::parse("both"), Some(AnalysisMode::Both));
        assert_eq!(AnalysisMode::parse("Both"), None);
    }

    #[test]
    fn bulk_updates() {
        let conn = library();
        assert_eq!(batch_update_genres(&conn, 1, &[4, 5, 5, 99]).unwrap(), (2, "House".into()));
        let row = super::super::tracks::get_track(&conn, BigInt(5)).unwrap().unwrap();
        assert_eq!((row.genre.as_deref(), row.subgenre.as_deref(), row.is_genre_verified), (Some("House"), Some("Deep"), Some(true)));
        assert_eq!(batch_update_genres(&conn, 3, &[1]).unwrap_err().to_string(), PARENT_HAS_NO_GENRE);
        assert_eq!(batch_update_genres(&conn, 99, &[1]).unwrap_err().to_string(), PARENT_NOT_FOUND);
        assert_eq!(execute_cleanup(&conn, "Deep House", &[1, 2], AnalysisMode::Subgenre).unwrap(), 2);
        assert_eq!(execute_cleanup(&conn, "X", &[], AnalysisMode::Genre).unwrap(), 0);
    }
}
