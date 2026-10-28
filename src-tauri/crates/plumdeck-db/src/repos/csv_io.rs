//! The library CSV and metadata CSV queries (backend/app/services/csv_app_service.py, S8).

use chrono::NaiveDateTime;
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection, OptionalExt};

use super::tracks::{TrackRow, TRACK_COLUMNS};
use crate::array_codec::{pack_f32, pack_u8_waveform, unpack_f32, unpack_u8_waveform};
use crate::DbError;

/// One export row: the track and its stored analysis.
#[derive(Clone, Debug)]
pub struct ExportRow {
    pub track: TrackRow,
    pub features_extra_json: Option<Option<String>>,
    pub beats: Vec<f64>,
    pub waveform: Vec<f64>,
}

/// `select(Track, TrackAnalysis).join(..., isouter=True)`.
pub fn export_rows(conn: &Connection) -> Result<Vec<ExportRow>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLUMNS}, track_analyses.track_id, track_analyses.features_extra_json, track_analyses.beats_f32, \
         track_analyses.waveform_u8 FROM tracks LEFT OUTER JOIN track_analyses ON tracks.id = track_analyses.track_id"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows: Vec<(TrackRow, Option<i64>, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>)> = statement
        .query_map([], |row| Ok((TrackRow::from_row(row, 0)?, row.get(24)?, row.get(25)?, row.get(26)?, row.get(27)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(track, analysis, extra, beats, waveform)| ExportRow {
            track,
            features_extra_json: analysis.map(|_| extra),
            beats: unpack_f32(beats.as_deref()).unwrap_or_default(),
            waveform: unpack_u8_waveform(waveform.as_deref()),
        })
        .collect())
}

/// Every track, in table order.
pub fn all_tracks(conn: &Connection) -> Result<Vec<TrackRow>, DbError> {
    let mut statement = conn.prepare(&format!("SELECT {TRACK_COLUMNS} FROM tracks"))?;
    let rows = statement.query_map([], |row| TrackRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// The first track stored under exactly `path`.
pub fn track_at(conn: &Connection, path: &str) -> Result<Option<TrackRow>, DbError> {
    Ok(conn
        .query_row(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE tracks.filepath = ? LIMIT 1"), [path], |row| TrackRow::from_row(row, 0))
        .optional()?)
}

/// Write a track's editable columns.
pub fn write_track(conn: &Connection, track: &TrackRow) -> Result<(), DbError> {
    conn.execute(
        "UPDATE tracks SET filepath=?, title=?, artist=?, album=?, genre=?, subgenre=?, year=?, bpm=?, key=?, energy=?, \
         danceability=?, brightness=?, loudness=?, noisiness=?, contrast=?, is_genre_verified=? WHERE tracks.id = ?",
        params![
            track.filepath, track.title, track.artist, track.album, track.genre, track.subgenre, track.year,
            track.bpm.map(|v| v as f32), track.key, track.energy.map(|v| v as f32), track.danceability.map(|v| v as f32),
            track.brightness.map(|v| v as f32), track.loudness.map(|v| v as f32), track.noisiness.map(|v| v as f32),
            track.contrast.map(|v| v as f32), track.is_genre_verified, track.id
        ],
    )?;
    Ok(())
}

/// `features_extra_json` of a track's analysis row (`None` without a row).
pub fn analysis_extra(conn: &Connection, track_id: i64) -> Result<Option<String>, DbError> {
    Ok(conn
        .query_row("SELECT features_extra_json FROM track_analyses WHERE track_id = ?", [track_id], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .map(|extra| extra.unwrap_or_else(|| "{}".into())))
}

/// Create or update a track's analysis row: `extra`, and the arrays when given.
pub fn write_analysis(conn: &Connection, track_id: i64, extra: &str, beats: Option<&[f64]>, peaks: Option<&[f64]>) -> Result<(), DbError> {
    let exists = analysis_extra(conn, track_id)?.is_some();
    let beats = beats.and_then(|values| pack_f32(Some(values)));
    let peaks = match peaks {
        Some(values) => pack_u8_waveform(Some(values), values.len()).map_err(|error| DbError::Invalid(error.to_string()))?,
        None => None,
    };
    if exists {
        conn.execute("UPDATE track_analyses SET features_extra_json=? WHERE track_id = ?", params![extra, track_id])?;
        if let Some(beats) = beats {
            conn.execute("UPDATE track_analyses SET beats_f32=? WHERE track_id = ?", params![beats, track_id])?;
        }
        if let Some(peaks) = peaks {
            conn.execute("UPDATE track_analyses SET waveform_u8=? WHERE track_id = ?", params![peaks, track_id])?;
        }
    } else {
        conn.execute(
            "INSERT INTO track_analyses (track_id, beats_f32, waveform_u8, features_extra_json) VALUES (?, ?, ?, ?)",
            params![track_id, beats, peaks, extra],
        )?;
    }
    Ok(())
}

/// The new-track fields of a library CSV row.
#[derive(Clone, Debug, Default)]
pub struct NewTrack {
    pub filepath: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub loudness: Option<f64>,
    pub noisiness: Option<f64>,
    pub contrast: Option<f64>,
    pub duration: Option<f64>,
    pub loudness_range: Option<f64>,
    pub spectral_flux: Option<f64>,
    pub spectral_rolloff: Option<f64>,
}

/// Insert a CSV track (model defaults for the rest) and return its id.
pub fn insert_track(conn: &Connection, track: &NewTrack, now: NaiveDateTime) -> Result<i64, DbError> {
    let f = |value: Option<f64>| value.map_or(Value::Null, |v| Value::Float(v as f32));
    let s = |value: &Option<String>| value.clone().map_or(Value::Null, Value::Text);
    let values = vec![
        Value::Text(track.filepath.clone()),
        Value::Text(track.title.clone()),
        Value::Text(track.artist.clone()),
        s(&track.album),
        s(&track.genre),
        s(&track.subgenre),
        track.year.map_or(Value::Null, Value::BigInt),
        f(track.bpm),
        s(&track.key),
        Value::Text(String::new()),
        f(track.duration),
        f(track.energy),
        f(track.danceability),
        f(track.loudness),
        f(track.brightness),
        f(track.noisiness),
        f(track.contrast),
        f(track.loudness_range),
        f(track.spectral_flux),
        f(track.spectral_rolloff),
        Value::Boolean(false),
        Value::Timestamp(duckdb::types::TimeUnit::Microsecond, now.and_utc().timestamp_micros()),
    ];
    Ok(conn.query_row(
        "INSERT INTO tracks (filepath, title, artist, album, genre, subgenre, year, bpm, key, scale, duration, energy, \
         danceability, loudness, brightness, noisiness, contrast, loudness_range, spectral_flux, spectral_rolloff, \
         is_genre_verified, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        params_from_iter(values),
        |row| row.get(0),
    )?)
}
