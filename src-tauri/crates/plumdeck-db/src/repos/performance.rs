//! `track_performance_metadata`, `track_grid_candidates` and the stored
//! analysis a grid can come from (backend/infra/repositories/performance_metadata_repository.py,
//! backend/app/services/grid_candidate_service.py; S5).

use chrono::NaiveDateTime;
use duckdb::{params, params_from_iter, Connection, OptionalExt};

use super::tracks::{BigInt, Sql};
use crate::DbError;

/// A `track_performance_metadata` row.
#[derive(Clone, Debug, PartialEq)]
pub struct MetadataRow {
    pub track_id: i64,
    pub cue_points_json: Option<String>,
    pub loops_json: Option<String>,
    pub beat_grid_json: Option<String>,
    pub revision: i64,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

const COLUMNS: &str = "track_id, cue_points_json, loops_json, beat_grid_json, revision, created_at, updated_at";

fn row(r: &duckdb::Row<'_>) -> duckdb::Result<MetadataRow> {
    Ok(MetadataRow {
        track_id: r.get(0)?,
        cue_points_json: r.get(1)?,
        loops_json: r.get(2)?,
        beat_grid_json: r.get(3)?,
        revision: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

/// `session.get(TrackPerformanceMetadata, track_id)`.
pub fn get_metadata(conn: &Connection, track_id: i64) -> Result<Option<MetadataRow>, DbError> {
    Ok(conn.query_row(&format!("SELECT {COLUMNS} FROM track_performance_metadata WHERE track_id = ?"), [track_id], row).optional()?)
}

/// `get_many`: rows of the given tracks (missing ones absent).
pub fn get_many(conn: &Connection, track_ids: &[i64]) -> Result<Vec<MetadataRow>, DbError> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = Sql::default();
    sql.push(&format!("SELECT {COLUMNS} FROM track_performance_metadata WHERE track_id IN "));
    sql.placeholders(track_ids.iter().map(|id| duckdb::types::Value::BigInt(*id)));
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement.query_map(params_from_iter(sql.params), row)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `replace`'s write: insert revision 1 or update the row, `updated_at` now.
pub fn save_metadata(
    conn: &Connection,
    track_id: i64,
    cues: &str,
    loops: &str,
    grid: Option<&str>,
    revision: i64,
    exists: bool,
    now: NaiveDateTime,
) -> Result<MetadataRow, DbError> {
    if exists {
        conn.execute(
            "UPDATE track_performance_metadata SET cue_points_json=?, loops_json=?, beat_grid_json=?, revision=?, updated_at=? WHERE track_id = ?",
            params![cues, loops, grid, revision, now, track_id],
        )?;
    } else {
        conn.execute(
            "INSERT INTO track_performance_metadata (track_id, cue_points_json, loops_json, beat_grid_json, revision, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![track_id, cues, loops, grid, revision, now, now],
        )?;
    }
    Ok(get_metadata(conn, track_id)?.expect("saved row"))
}

/// Every `(track_id, filepath)` and every stored revision (bulk import).
pub fn tracks_and_revisions(conn: &Connection) -> Result<(Vec<(i64, String)>, Vec<(i64, i64)>), DbError> {
    let mut statement = conn.prepare("SELECT tracks.id, tracks.filepath FROM tracks")?;
    let tracks = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut statement = conn.prepare("SELECT track_id, revision FROM track_performance_metadata")?;
    let revisions = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok((tracks, revisions))
}

/// `_cached`: the grid JSON stored for this source and fingerprint.
pub fn cached_grid(conn: &Connection, track_id: i64, source: &str, fingerprint: &str) -> Result<Option<String>, DbError> {
    Ok(conn
        .query_row(
            "SELECT grid_json FROM track_grid_candidates WHERE track_id=? AND source=? AND fingerprint=?",
            params![track_id, source, fingerprint],
            |r| r.get(0),
        )
        .optional()?)
}

/// `_cache`: upsert one candidate.
pub fn cache_grid(
    conn: &Connection,
    track_id: i64,
    source: &str,
    grid_json: &str,
    fingerprint: &str,
    provenance_json: &str,
    now: NaiveDateTime,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO track_grid_candidates (track_id, source, grid_json, fingerprint, provenance_json, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (track_id, source) DO UPDATE SET grid_json=excluded.grid_json, \
         fingerprint=excluded.fingerprint, provenance_json=excluded.provenance_json, updated_at=excluded.updated_at",
        params![track_id, source, grid_json, fingerprint, provenance_json, now],
    )?;
    Ok(())
}

/// `session.get(TrackAnalysis, id)`: `(features_extra_json, beats_f32)`.
pub fn stored_analysis(conn: &Connection, track_id: BigInt) -> Result<Option<(Option<String>, Option<Vec<u8>>)>, DbError> {
    Ok(conn
        .query_row(
            "SELECT features_extra_json, beats_f32 FROM track_analyses WHERE track_id = ?",
            [track_id.value()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}
