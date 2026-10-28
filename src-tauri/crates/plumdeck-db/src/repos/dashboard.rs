//! The library statistics behind `GET /api/dashboard`
//! (backend/api/routers/system.py `get_dashboard_stats`).
//!
//! The queries are the ones SQLAlchemy sends, including their undefined
//! orders: genres tied on count, and setlists tied on `updated_at`, come back
//! in whatever order DuckDB produces (normalized by the parity runner, N6).

use chrono::NaiveDateTime;
use duckdb::Connection;

use super::settings::{finish, setting_value};
use crate::DbError;

/// One `{"name", "count"}` entry of `genre_distribution`.
#[derive(Clone, Debug, PartialEq)]
pub struct GenreCount {
    pub name: String,
    pub count: i64,
}

/// A `setlists` row as the `Setlist` SQLModel loads it. Nullable columns
/// stay `None` even where the model declares a default (no validation on
/// load).
#[derive(Clone, Debug, PartialEq)]
pub struct SetlistRow {
    pub id: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub display_order: Option<i64>,
    pub genre: Option<String>,
    /// FLOAT column: the f32 value, widened exactly as Python's float.
    pub target_duration: Option<f64>,
    pub rating: Option<i64>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DashboardStats {
    pub total_tracks: i64,
    pub analyzed_tracks: i64,
    pub genre_distribution: Vec<GenreCount>,
    pub unverified_genres_count: i64,
    pub lyrics_tracks_count: i64,
    /// Up to five setlists, most recently updated first.
    pub recent_setlists: Vec<SetlistRow>,
    /// `bool(get_setting_value(session, "root_path", ""))`.
    pub has_root_path: bool,
}

impl DashboardStats {
    pub fn unanalyzed_tracks(&self) -> i64 {
        self.total_tracks - self.analyzed_tracks
    }
}

fn count(conn: &Connection, sql: &str) -> Result<i64, DbError> {
    Ok(conn.query_row(sql, [], |row| row.get(0))?)
}

/// Every statistic, read in one transaction (one SQLAlchemy session).
pub fn dashboard_stats(conn: &Connection) -> Result<DashboardStats, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let total_tracks = count(conn, "SELECT count(*) FROM tracks")?;
        let analyzed_tracks = count(conn, "SELECT count(*) FROM tracks WHERE tracks.bpm > 0")?;
        let genre_distribution = conn
            .prepare(
                "SELECT genre, COUNT(*) as count FROM tracks \
                 WHERE genre IS NOT NULL AND genre != 'Unknown' AND genre != '' \
                 GROUP BY genre ORDER BY count DESC",
            )?
            .query_map([], |row| Ok(GenreCount { name: row.get(0)?, count: row.get(1)? }))?
            .collect::<Result<_, _>>()?;
        let unverified_genres_count =
            count(conn, "SELECT count(*) FROM tracks WHERE tracks.is_genre_verified = false")?;
        let lyrics_tracks_count =
            count(conn, "SELECT count(*) FROM lyrics WHERE lyrics.content IS NOT NULL AND lyrics.content != ''")?;
        let recent_setlists = conn
            .prepare(
                "SELECT id, name, description, display_order, genre, target_duration, rating, created_at, updated_at \
                 FROM setlists ORDER BY setlists.updated_at DESC LIMIT 5",
            )?
            .query_map([], |row| {
                Ok(SetlistRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    display_order: row.get(3)?,
                    genre: row.get(4)?,
                    target_duration: row.get::<_, Option<f32>>(5)?.map(f64::from),
                    rating: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let has_root_path = setting_value(conn, "root_path")?.is_some_and(|value| !value.is_empty());
        Ok(DashboardStats {
            total_tracks,
            analyzed_tracks,
            genre_distribution,
            unverified_genres_count,
            lyrics_tracks_count,
            recent_setlists,
            has_root_path,
        })
    })();
    finish(conn, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn counts_follow_the_python_filters() {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, genre, bpm, is_genre_verified) VALUES
               ('/1', 'House', 124, true), ('/2', 'House', 0, false), ('/3', 'Unknown', NULL, NULL),
               ('/4', '', -1, false), ('/5', NULL, 0.5, false), ('/6', 'unknown', 128, true);
             INSERT INTO lyrics (track_id, content) VALUES (1, 'la'), (2, ''), (3, NULL);
             INSERT INTO setlists (name, target_duration, created_at, updated_at) VALUES
               ('old', 3600.1, '2026-01-01 00:00:00', '2026-01-01 00:00:00'),
               ('new', NULL, '2026-01-02 00:00:00', '2026-01-03 00:00:00.5'),
               ('never', NULL, NULL, NULL);
             INSERT INTO settings VALUES ('root_path', '');",
        )
        .unwrap();
        let stats = dashboard_stats(&conn).unwrap();
        assert_eq!((stats.total_tracks, stats.analyzed_tracks, stats.unanalyzed_tracks()), (6, 3, 3));
        assert_eq!(
            stats.genre_distribution,
            [GenreCount { name: "House".into(), count: 2 }, GenreCount { name: "unknown".into(), count: 1 }]
        );
        assert_eq!((stats.unverified_genres_count, stats.lyrics_tracks_count), (3, 1));
        let names: Vec<_> = stats.recent_setlists.iter().map(|s| s.name.clone().unwrap()).collect();
        assert_eq!(names, ["new", "old", "never"]);
        assert_eq!(stats.recent_setlists[1].target_duration, Some(f64::from(3600.1_f32)));
        assert_eq!(
            stats.recent_setlists[0].updated_at,
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap().and_hms_micro_opt(0, 0, 0, 500_000)
        );
        assert_eq!(stats.recent_setlists[2].display_order, Some(0));
        assert!(!stats.has_root_path);
        conn.execute_batch("UPDATE settings SET value = '/music'").unwrap();
        assert!(dashboard_stats(&conn).unwrap().has_root_path);
    }
}
