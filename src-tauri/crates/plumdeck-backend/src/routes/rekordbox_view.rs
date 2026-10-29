//! rekordbox data for the Play screen, and the one 3-band waveform model
//! (additions, not Python routes; deviation D-52).
//!
//! - `GET /api/tracks/{id}/waveform3?level=overview|detail`: the track's
//!   3-band waveform (low, mid, high per column, 0..scale). From rekordbox's
//!   PWV6 (overview, 1200 columns) / PWV7 (detail, 150 columns per second)
//!   when the track is in rekordbox, else from plumdeck's own decoded bands.
//! - `GET /api/tracks/{id}/rekordbox`: phrases (PSSI) and memory cues, on the
//!   playback timeline.
//! - `GET`/`PUT /api/tracks/{id}/memory-cues`, `POST
//!   /api/tracks/{id}/memory-cues/rekordbox`: MEMORY CUE storage.
//!
//! rekordbox is optional and read-only. Its times are converted with the same
//! decoder timing offset the PQTZ grid and hot cues use
//! (`t_playback = t_rekordbox − offset`); PSSI beats are mapped through the
//! uncorrected PQTZ first.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::NaiveDateTime;
use plumdeck_db::duckdb::{params, Connection, OptionalExt};
use plumdeck_db::repos::tracks::{get_track, BigInt};
use plumdeck_db::DbError;
use plumdeck_media::timing::timing_offset_ms;
use plumdeck_rekordbox::anlz::{AnlzFile, Mood, SongStructure, WaveformKind};
use plumdeck_rekordbox::locate::{default_master_db_path, resolve_analysis_path, sibling, AnlzKind};
use plumdeck_rekordbox::{MasterDb, RekordboxError};
use serde_json::{json, Value};

use crate::routes::waveform_detail::{decoded_values, DETAIL_BINS_PER_SECOND};
use crate::{server_error, AppState};

const OVERVIEW_COLUMNS: usize = 1200;
const DETAIL_COLUMNS_PER_SECOND: usize = 150;
const SCALE: u8 = 127;
pub const MAX_MEMORY_CUES: usize = 128;

fn error(status: u16, detail: impl Into<String>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

// ---------------------------------------------------------------- rekordbox

/// What rekordbox knows about one audio file.
#[derive(Clone)]
struct RbAnalysis {
    content_id: String,
    beats: Vec<(u16, u32)>,
    overview: Option<Vec<u8>>,
    detail: Option<Vec<u8>>,
    phrases: Option<SongStructure>,
}

struct Library {
    path: PathBuf,
    modified: Option<SystemTime>,
    db: MasterDb,
    /// filepath → (DAT path, content id); `None` when not in rekordbox.
    lookups: HashMap<String, Option<(PathBuf, String)>>,
}

fn library() -> &'static Mutex<Option<Library>> {
    static LIBRARY: OnceLock<Mutex<Option<Library>>> = OnceLock::new();
    LIBRARY.get_or_init(|| Mutex::new(None))
}

type AnalysisCache = Mutex<Vec<(PathBuf, Option<SystemTime>, Arc<RbAnalysis>)>>;
fn analyses() -> &'static AnalysisCache {
    static CACHE: OnceLock<AnalysisCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn modified(path: &FsPath) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Why rekordbox data is not available (shown in 「詳細」 only).
#[derive(Debug, Clone, PartialEq)]
pub enum Unavailable {
    NoLibrary,
    NotInLibrary,
    Unreadable(String),
}

impl Unavailable {
    fn reason(&self) -> String {
        match self {
            Unavailable::NoLibrary => "rekordbox library not found".into(),
            Unavailable::NotInLibrary => "track is not in rekordbox".into(),
            Unavailable::Unreadable(message) => message.clone(),
        }
    }
}

/// Runs `work` with the (re)opened rekordbox library.
fn with_library<T>(work: impl FnOnce(&mut Library) -> Result<T, Unavailable>) -> Result<T, Unavailable> {
    let path = default_master_db_path().filter(|p| p.is_file()).ok_or(Unavailable::NoLibrary)?;
    let stamp = modified(&path);
    let mut slot = library().lock().unwrap_or_else(|e| e.into_inner());
    let stale = slot.as_ref().is_none_or(|lib| lib.path != path || lib.modified != stamp);
    if stale {
        let db = MasterDb::open(&path).map_err(|e| Unavailable::Unreadable(format!("rekordbox library: {e}")))?;
        *slot = Some(Library { path, modified: stamp, db, lookups: HashMap::new() });
    }
    work(slot.as_mut().expect("opened"))
}

fn locate(filepath: &str) -> Result<(PathBuf, String), Unavailable> {
    with_library(|lib| {
        if let Some(found) = lib.lookups.get(filepath) {
            return found.clone().ok_or(Unavailable::NotInLibrary);
        }
        let rows = match lib.db.contents_at_path(filepath) {
            Ok(rows) => rows,
            Err(RekordboxError::Ambiguous) => return Err(Unavailable::Unreadable("several rekordbox tracks match this path".into())),
            Err(e) => return Err(Unavailable::Unreadable(format!("rekordbox library: {e}"))),
        };
        let found = rows.iter().find_map(|row| {
            let raw = row.analysis_data_path.as_deref().filter(|p| !p.is_empty())?;
            resolve_analysis_path(&lib.path, raw).map(|dat| (dat, row.id.clone()))
        });
        if lib.lookups.len() > 50_000 {
            lib.lookups.clear();
        }
        lib.lookups.insert(filepath.to_owned(), found.clone());
        found.ok_or(Unavailable::NotInLibrary)
    })
}

fn read_analysis(filepath: &str) -> Result<Arc<RbAnalysis>, Unavailable> {
    let (dat, content_id) = locate(filepath)?;
    let ex2 = sibling(&dat, AnlzKind::Ex2);
    let ext = sibling(&dat, AnlzKind::Ext);
    let stamp = [&dat, &ext, &ex2].iter().filter_map(|p| modified(p)).max();
    {
        let cache = analyses().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, hit)) = cache.iter().find(|(path, at, _)| path == &dat && *at == stamp) {
            return Ok(hit.clone());
        }
    }
    let read = |path: &FsPath| if path.is_file() { AnlzFile::read(path).ok() } else { None };
    let files: Vec<AnlzFile> = [&dat, &ext, &ex2].into_iter().filter_map(|p| read(p)).collect();
    if files.is_empty() {
        return Err(Unavailable::Unreadable("rekordbox analysis files are unreadable".into()));
    }
    let beats = files.iter().find_map(|f| f.beat_grid()).map(|g| g.beats.iter().map(|b| (b.beat_number, b.time_ms)).collect()).unwrap_or_default();
    let wave = |kind| files.iter().find_map(|f| f.waveform(kind)).map(|w| w.data.clone()).filter(|d: &Vec<u8>| d.len() >= 3);
    let analysis = Arc::new(RbAnalysis {
        content_id,
        beats,
        overview: wave(WaveformKind::ThreeBandPreview),
        detail: wave(WaveformKind::ThreeBandDetail),
        phrases: files.iter().find_map(|f| f.song_structure()).cloned(),
    });
    let mut cache = analyses().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(path, _, _)| path != &dat);
    cache.push((dat, stamp, analysis.clone()));
    if cache.len() > 24 {
        cache.remove(0);
    }
    Ok(analysis)
}

/// The decoder timing offset for a file (0 when it cannot be judged).
fn offset_ms(filepath: &str) -> f64 {
    timing_offset_ms(filepath).unwrap_or(0.0)
}

// ---------------------------------------------------------------- waveform

/// One 3-band waveform: interleaved low, mid, high bytes per column.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreeBand {
    pub source: &'static str,
    pub columns: usize,
    /// Columns per second for a detail waveform; `None` for an overview,
    /// whose columns span `duration_ms` evenly.
    pub columns_per_second: Option<usize>,
    /// Playback time of column 0.
    pub start_ms: f64,
    pub duration_ms: f64,
    pub data: Vec<u8>,
}

impl ThreeBand {
    fn json(&self) -> Value {
        json!({
            "source": self.source,
            "columns": self.columns,
            "columns_per_second": self.columns_per_second,
            "start_ms": self.start_ms,
            "duration_ms": self.duration_ms,
            "scale": SCALE,
            "band_order": ["low", "mid", "high"],
            "data": base64::engine::general_purpose::STANDARD.encode(&self.data),
        })
    }
}

/// rekordbox stores 0..127 (some files exceed it); clamp to the model's scale.
fn clamp_bands(data: &[u8]) -> Vec<u8> {
    data.iter().map(|v| (*v).min(SCALE)).collect()
}

/// The 98th percentile of a band-max histogram, for the own-data gain.
fn ceiling(levels: impl Iterator<Item = f64>) -> f64 {
    let mut histogram = [0usize; 256];
    let mut total = 0usize;
    for level in levels {
        histogram[level.clamp(0.0, 255.0) as usize] += 1;
        total += 1;
    }
    let mut remaining = total / 50;
    for value in (1..256).rev() {
        if remaining < histogram[value] {
            return value as f64;
        }
        remaining -= histogram[value];
    }
    1.0
}

/// Own 3-band data from the decoded 300/s band peaks: mid and high are
/// tilted down (band-pass peaks overstate them against rekordbox's look), the
/// loudest 2 % reach ~120 of 127, then grouped to `columns`.
pub fn own_three_band(values: &[[u8; 4]], samples: usize, sample_rate: usize, level_overview: bool) -> ThreeBand {
    const TILT: [f64; 3] = [1.0, 0.85, 0.5];
    let duration_ms = samples as f64 / sample_rate as f64 * 1000.0;
    let rows: Vec<[f64; 3]> = values.iter().map(|r| [r[1] as f64 * TILT[0], r[2] as f64 * TILT[1], r[3] as f64 * TILT[2]]).collect();
    let (columns, per_second, group): (usize, Option<usize>, f64) = if level_overview {
        let columns = OVERVIEW_COLUMNS.min(rows.len().max(1));
        (columns, None, rows.len() as f64 / columns as f64)
    } else {
        let group = DETAIL_BINS_PER_SECOND as f64 / DETAIL_COLUMNS_PER_SECOND as f64;
        (((rows.len() as f64) / group).ceil() as usize, Some(DETAIL_COLUMNS_PER_SECOND), group)
    };
    let mut grouped: Vec<[f64; 3]> = Vec::with_capacity(columns);
    for column in 0..columns {
        let from = (column as f64 * group) as usize;
        let to = (((column + 1) as f64 * group) as usize).clamp(from + 1, rows.len().max(from + 1));
        let slice = &rows[from.min(rows.len())..to.min(rows.len())];
        let mut out = [0.0; 3];
        if level_overview {
            // RMS over the column: a song's shape, not the one loudest hit.
            for band in 0..3 {
                let energy: f64 = slice.iter().map(|r| r[band] * r[band]).sum();
                out[band] = (energy / slice.len().max(1) as f64).sqrt();
            }
        } else {
            for row in slice {
                for band in 0..3 {
                    out[band] = out[band].max(row[band]);
                }
            }
        }
        grouped.push(out);
    }
    let top = ceiling(grouped.iter().map(|c| c[0].max(c[1]).max(c[2])));
    let gain = 120.0 / top.max(1.0);
    let data = grouped.iter().flat_map(|c| c.map(|v| (v * gain).round().clamp(0.0, SCALE as f64) as u8)).collect();
    ThreeBand { source: "own", columns, columns_per_second: per_second, start_ms: 0.0, duration_ms, data }
}

fn rekordbox_three_band(analysis: &RbAnalysis, overview: bool, offset: f64, duration_ms: f64) -> Option<ThreeBand> {
    let data = if overview { analysis.overview.as_ref()? } else { analysis.detail.as_ref()? };
    let columns = data.len() / 3;
    let data = clamp_bands(&data[..columns * 3]);
    let duration_ms = if overview { duration_ms } else { columns as f64 * 1000.0 / DETAIL_COLUMNS_PER_SECOND as f64 };
    Some(ThreeBand {
        source: "rekordbox",
        columns,
        columns_per_second: (!overview).then_some(DETAIL_COLUMNS_PER_SECOND),
        start_ms: -offset,
        duration_ms,
        data,
    })
}

#[derive(serde::Deserialize)]
pub struct WaveformQuery {
    level: Option<String>,
}

/// `GET /api/tracks/{track_id}/waveform3`.
pub async fn waveform3(State(state): State<AppState>, Path(track_id): Path<String>, Query(query): Query<WaveformQuery>) -> Response {
    let Ok(id) = track_id.parse::<i64>() else { return error(422, "track_id must be an integer") };
    let overview = match query.level.as_deref() {
        None | Some("overview") => true,
        Some("detail") => false,
        Some(_) => return error(422, "level must be overview or detail"),
    };
    let db = state.db.clone();
    let track = match state.blocking(move || get_track(&*db.lease()?, BigInt(id.into()))).await {
        Ok(Some(track)) => track,
        Ok(None) => return error(404, "Track not found"),
        Err(response) => return response,
    };
    let result = state
        .blocking_result(move || {
            let duration_ms = track.duration.unwrap_or(0.0) * 1000.0;
            if let Ok(analysis) = read_analysis(&track.filepath) {
                let offset = offset_ms(&track.filepath);
                if let Some(wave) = rekordbox_three_band(&analysis, overview, offset, duration_ms.max(1.0)) {
                    return Ok(wave);
                }
            }
            let (values, samples) = decoded_values(&track.filepath)?;
            Ok::<_, (u16, &'static str)>(own_three_band(&values, samples, 24_000, overview))
        })
        .await;
    match result {
        Ok(Ok(wave)) => Json(wave.json()).into_response(),
        Ok(Err((status, detail))) => error(status, detail),
        Err(response) => response,
    }
}

// ---------------------------------------------------------------- phrases

/// The phrase family used for colour: intro, verse, bridge, chorus, up,
/// down, outro.
fn phrase_kind(mood: Mood, kind: u16) -> Option<&'static str> {
    Some(match mood {
        Mood::High => match kind {
            1 => "intro",
            2 => "up",
            3 => "down",
            5 => "chorus",
            6 => "outro",
            _ => return None,
        },
        Mood::Mid | Mood::Low => match (mood, kind) {
            (_, 1) => "intro",
            (_, 8) => "bridge",
            (_, 9) => "chorus",
            (_, 10) => "outro",
            (_, 2..=7) => "verse",
            _ => return None,
        },
        Mood::Unknown(_) => return None,
    })
}

/// PSSI phrases on the playback timeline. Beat numbers are 1-based PQTZ
/// indices; they are read on the uncorrected grid and shifted afterwards.
pub fn phrase_items(structure: &SongStructure, beats: &[(u16, u32)], offset: f64, duration_ms: f64) -> Vec<Value> {
    if beats.len() < 2 {
        return Vec::new();
    }
    let last = beats.len() - 1;
    let period = (beats[last].1 as f64 - beats[0].1 as f64) / last as f64;
    let beat_ms = |beat: u32| -> f64 {
        let index = beat.saturating_sub(1) as usize;
        let raw = if index <= last { beats[index].1 as f64 } else { beats[last].1 as f64 + (index - last) as f64 * period };
        raw - offset
    };
    let mut phrases: Vec<_> = structure.phrases.iter().filter(|p| p.beat >= 1).collect();
    phrases.sort_by_key(|p| p.beat);
    let end_beat = u32::from(structure.end_beat).max(phrases.last().map_or(0, |p| u32::from(p.beat) + 1));
    phrases
        .iter()
        .enumerate()
        .filter_map(|(index, phrase)| {
            let start_beat = u32::from(phrase.beat);
            let next = phrases.get(index + 1).map_or(end_beat, |p| u32::from(p.beat));
            let start_ms = beat_ms(start_beat);
            let end_ms = beat_ms(next).min(duration_ms.max(start_ms));
            if end_ms <= start_ms || start_ms >= duration_ms {
                return None;
            }
            let kind = phrase_kind(structure.mood, phrase.kind)?;
            let label = structure.label(phrase).unwrap_or(kind);
            let fill_ms = (phrase.has_fill() && phrase.beat_fill > phrase.beat).then(|| beat_ms(u32::from(phrase.beat_fill))).filter(|ms| *ms > start_ms && *ms < end_ms);
            Some(json!({
                "start_ms": start_ms.max(0.0),
                "end_ms": end_ms,
                "start_beat": start_beat,
                "end_beat": next,
                "kind": kind,
                "label": label,
                "fill_ms": fill_ms,
            }))
        })
        .collect()
}

fn mood_name(mood: Mood) -> &'static str {
    match mood {
        Mood::High => "high",
        Mood::Mid => "mid",
        Mood::Low => "low",
        Mood::Unknown(_) => "unknown",
    }
}

// ---------------------------------------------------------------- memory cues

/// One MEMORY CUE (or memory loop) on the playback timeline.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryCue {
    pub position_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// "user" or "rekordbox".
    #[serde(default = "user_source")]
    pub source: String,
}

fn user_source() -> String {
    "user".into()
}

/// Sorted, deduplicated (within 10 ms), bounded and inside the track.
pub fn normalize_cues(mut cues: Vec<MemoryCue>, duration_ms: Option<f64>) -> Result<Vec<MemoryCue>, String> {
    let limit = duration_ms.filter(|d| d.is_finite() && *d > 0.0);
    for cue in &mut cues {
        if !cue.position_ms.is_finite() || cue.position_ms < 0.0 || limit.is_some_and(|d| cue.position_ms >= d + 1.0) {
            return Err("memory cue position is outside the track".into());
        }
        if let Some(end) = cue.end_ms {
            if !end.is_finite() || end <= cue.position_ms || limit.is_some_and(|d| end > d + 1.0) {
                return Err("memory loop end must be after its start and inside the track".into());
            }
        }
        cue.label = cue.label.take().map(|l| l.trim().chars().take(100).collect::<String>()).filter(|l| !l.is_empty());
        cue.color = cue.color.take().filter(|c| c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()));
        if cue.source != "rekordbox" {
            cue.source = "user".into();
        }
    }
    cues.sort_by(|a, b| a.position_ms.total_cmp(&b.position_ms));
    cues.dedup_by(|b, a| (a.position_ms - b.position_ms).abs() <= 10.0 && a.end_ms.is_some() == b.end_ms.is_some());
    if cues.len() > MAX_MEMORY_CUES {
        return Err(format!("a track holds at most {MAX_MEMORY_CUES} memory cues"));
    }
    Ok(cues)
}

const TABLE: &str = "CREATE TABLE IF NOT EXISTS track_memory_cues (track_id BIGINT PRIMARY KEY, cues_json VARCHAR NOT NULL, updated_at TIMESTAMP)";

fn table_exists(conn: &Connection) -> Result<bool, DbError> {
    let count: i64 = conn.query_row("SELECT count(*) FROM duckdb_tables() WHERE table_name = 'track_memory_cues' AND schema_name = 'main'", [], |r| r.get(0))?;
    Ok(count > 0)
}

pub fn load_memory_cues(conn: &Connection, track_id: i64) -> Result<Vec<MemoryCue>, DbError> {
    if !table_exists(conn)? {
        return Ok(Vec::new());
    }
    let text: Option<String> = conn.query_row("SELECT cues_json FROM track_memory_cues WHERE track_id = ?", [track_id], |r| r.get(0)).optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// The table is created on the first write (an addition outside the v5
/// schema, so libraries without memory cues stay byte-identical).
pub fn save_memory_cues(conn: &Connection, track_id: i64, cues: &[MemoryCue], now: NaiveDateTime) -> Result<(), DbError> {
    conn.execute_batch(TABLE)?;
    let text = serde_json::to_string(cues).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO track_memory_cues (track_id, cues_json, updated_at) VALUES (?, ?, ?) ON CONFLICT (track_id) DO UPDATE SET cues_json = excluded.cues_json, updated_at = excluded.updated_at",
        params![track_id, text, now],
    )?;
    Ok(())
}

/// rekordbox memory cues (Kind 0) for a content, shifted onto the playback timeline.
pub fn rekordbox_memory_cues(db: &MasterDb, content_id: &str, filepath: &str) -> Vec<MemoryCue> {
    let offset = offset_ms(filepath);
    db.cues(content_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|cue| cue.kind == 0)
        .filter_map(|cue| {
            let position = cue.in_msec? as f64 - offset;
            let end = cue.out_msec.map(|o| o as f64 - offset).filter(|e| *e > position);
            Some(MemoryCue {
                position_ms: position.max(0.0),
                end_ms: end,
                label: cue.comment.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()),
                color: None,
                source: "rekordbox".into(),
            })
        })
        .collect()
}

/// Adds rekordbox memory cues the track does not hold yet (never removes).
pub fn merge_memory_cues(current: Vec<MemoryCue>, incoming: Vec<MemoryCue>) -> (Vec<MemoryCue>, usize) {
    let mut merged = current;
    let mut added = 0;
    for cue in incoming {
        if !merged.iter().any(|c| (c.position_ms - cue.position_ms).abs() <= 10.0 && c.end_ms.is_some() == cue.end_ms.is_some()) {
            merged.push(cue);
            added += 1;
        }
    }
    merged.sort_by(|a, b| a.position_ms.total_cmp(&b.position_ms));
    merged.truncate(MAX_MEMORY_CUES);
    (merged, added)
}

/// Imports one track's rekordbox memory cues into the database; used by the
/// hot-cue import routes too. Returns how many were added.
pub fn import_memory_cues(conn: &Connection, track_id: i64, filepath: &str, now: NaiveDateTime) -> Result<usize, String> {
    let (_, content_id) = locate(filepath).map_err(|e| e.reason())?;
    let incoming = with_library(|lib| Ok(rekordbox_memory_cues(&lib.db, &content_id, filepath))).map_err(|e| e.reason())?;
    if incoming.is_empty() {
        return Ok(0);
    }
    let current = load_memory_cues(conn, track_id).map_err(|e| e.to_string())?;
    let (merged, added) = merge_memory_cues(current, incoming);
    if added > 0 {
        save_memory_cues(conn, track_id, &merged, now).map_err(|e| e.to_string())?;
    }
    Ok(added)
}

/// Memory cues for an already-open library (bulk import).
pub fn import_memory_cues_with(db: &MasterDb, conn: &Connection, track_id: i64, content_id: &str, filepath: &str, now: NaiveDateTime) -> Result<usize, DbError> {
    let incoming = rekordbox_memory_cues(db, content_id, filepath);
    if incoming.is_empty() {
        return Ok(0);
    }
    let (merged, added) = merge_memory_cues(load_memory_cues(conn, track_id)?, incoming);
    if added > 0 {
        save_memory_cues(conn, track_id, &merged, now)?;
    }
    Ok(added)
}

// ---------------------------------------------------------------- routes

fn parse_id(raw: &str) -> Result<i64, Response> {
    raw.parse::<i64>().map_err(|_| error(422, "track_id must be an integer"))
}

/// `GET /api/tracks/{track_id}/rekordbox`.
pub async fn analysis(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let id = match parse_id(&track_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    let track = match state.blocking(move || get_track(&*db.lease()?, BigInt(id.into()))).await {
        Ok(Some(track)) => track,
        Ok(None) => return error(404, "Track not found"),
        Err(response) => return response,
    };
    let value = state
        .blocking_result(move || {
            let duration_ms = track.duration.unwrap_or(0.0) * 1000.0;
            match read_analysis(&track.filepath) {
                Err(why) => json!({ "available": false, "reason": why.reason(), "phrases": null, "memory_cues": [] }),
                Ok(analysis) => {
                    let offset = offset_ms(&track.filepath);
                    let phrases = analysis.phrases.as_ref().map(|s| {
                        json!({ "mood": mood_name(s.mood), "items": phrase_items(s, &analysis.beats, offset, duration_ms.max(1.0)) })
                    });
                    let memory = with_library(|lib| Ok(rekordbox_memory_cues(&lib.db, &analysis.content_id, &track.filepath))).unwrap_or_default();
                    json!({
                        "available": true,
                        "reason": null,
                        "timing_offset_ms": offset,
                        "has_waveform": analysis.detail.is_some() || analysis.overview.is_some(),
                        "phrases": phrases,
                        "memory_cues": memory,
                    })
                }
            }
        })
        .await;
    match value {
        Ok(value) => Json(value).into_response(),
        Err(response) => response,
    }
}

fn cues_body(track_id: i64, cues: &[MemoryCue]) -> Value {
    json!({ "track_id": track_id, "cues": cues })
}

/// `GET /api/tracks/{track_id}/memory-cues`.
pub async fn get_memory_cues(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let id = match parse_id(&track_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let db = state.db.clone();
    match state
        .blocking(move || {
            let conn = db.lease()?;
            if get_track(&conn, BigInt(id.into()))?.is_none() {
                return Ok(None);
            }
            load_memory_cues(&conn, id).map(Some)
        })
        .await
    {
        Ok(Some(cues)) => Json(cues_body(id, &cues)).into_response(),
        Ok(None) => error(404, "Track not found"),
        Err(response) => response,
    }
}

/// `PUT /api/tracks/{track_id}/memory-cues` with `{"cues": [...]}`: replace.
pub async fn put_memory_cues(State(state): State<AppState>, Path(track_id): Path<String>, body: Bytes) -> Response {
    let id = match parse_id(&track_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let cues: Vec<MemoryCue> = match serde_json::from_slice::<Value>(&body).ok().and_then(|v| v.get("cues").cloned()).map(serde_json::from_value) {
        Some(Ok(cues)) => cues,
        _ => return error(422, "body must be {\"cues\": [{\"position_ms\": number, \"end_ms\"?: number}]}"),
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                let Some(track) = get_track(conn, BigInt(id.into()))? else { return Ok(Err((404, "Track not found".to_owned()))) };
                let cues = match normalize_cues(cues, track.duration.map(|d| d * 1000.0)) {
                    Ok(cues) => cues,
                    Err(message) => return Ok(Err((422, message))),
                };
                save_memory_cues(conn, id, &cues, now)?;
                Ok(Ok(cues))
            })
        })
        .await;
    match result {
        Ok(Ok(Ok(cues))) => Json(cues_body(id, &cues)).into_response(),
        Ok(Ok(Err((status, message)))) => error(status, message),
        Ok(Err(e)) => server_error(&e),
        Err(response) => response,
    }
}

/// `POST /api/tracks/{track_id}/memory-cues/rekordbox`: add rekordbox's memory cues.
pub async fn import_memory_cues_route(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let id = match parse_id(&track_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            db.write(|conn| {
                let Some(track) = get_track(conn, BigInt(id.into()))? else { return Ok(Err((404, "Track not found".to_owned()))) };
                match import_memory_cues(conn, id, &track.filepath, now) {
                    Ok(added) => Ok(Ok((added, load_memory_cues(conn, id)?))),
                    Err(message) => Ok(Err((404, message))),
                }
            })
        })
        .await;
    match result {
        Ok(Ok(Ok((added, cues)))) => Json(json!({ "track_id": id, "cues": cues, "imported": added })).into_response(),
        Ok(Ok(Err((status, message)))) => error(status, message),
        Ok(Err(e)) => server_error(&e),
        Err(response) => response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumdeck_rekordbox::anlz::Phrase;

    fn phrase(beat: u16, kind: u16, k1: u8) -> Phrase {
        Phrase { index: 0, beat, kind, k1, k2: 0, k3: 0, b: 0, beat2: 0, beat3: 0, beat4: 0, fill: 0, beat_fill: 0 }
    }

    #[test]
    fn phrases_follow_the_uncorrected_grid_then_shift() {
        // 120 BPM: a beat every 500 ms from 1000 ms.
        let beats: Vec<(u16, u32)> = (0..64).map(|i| ((i % 4 + 1) as u16, 1000 + i * 500)).collect();
        let structure = SongStructure { mood: Mood::High, end_beat: 65, raw_bank: 0, masked: false, phrases: vec![phrase(1, 1, 1), phrase(33, 5, 1)], truncated: false };
        let items = phrase_items(&structure, &beats, 26.0, 60_000.0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["start_ms"], json!(974.0));
        assert_eq!(items[0]["end_ms"], json!(1000.0 + 32.0 * 500.0 - 26.0));
        assert_eq!(items[0]["kind"], json!("intro"));
        assert_eq!(items[1]["label"], json!("Chorus 1"));
        // end_beat past the PQTZ beats extends at the grid's mean period.
        assert_eq!(items[1]["end_ms"], json!(1000.0 + 64.0 * 500.0 - 26.0));
    }

    #[test]
    fn memory_cues_are_validated_sorted_and_deduplicated() {
        let cue = |p: f64, e: Option<f64>| MemoryCue { position_ms: p, end_ms: e, label: None, color: None, source: "x".into() };
        let cues = normalize_cues(vec![cue(5000.0, None), cue(1000.0, Some(2000.0)), cue(5004.0, None)], Some(10_000.0)).unwrap();
        assert_eq!(cues.iter().map(|c| c.position_ms).collect::<Vec<_>>(), vec![1000.0, 5000.0]);
        assert_eq!(cues[0].source, "user");
        assert!(normalize_cues(vec![cue(1000.0, Some(900.0))], None).is_err());
        assert!(normalize_cues(vec![cue(20_000.0, None)], Some(10_000.0)).is_err());
    }

    #[test]
    fn merging_keeps_existing_cues_and_adds_new_ones() {
        let cue = |p: f64, source: &str| MemoryCue { position_ms: p, end_ms: None, label: None, color: None, source: source.into() };
        let (merged, added) = merge_memory_cues(vec![cue(1000.0, "user")], vec![cue(1005.0, "rekordbox"), cue(3000.0, "rekordbox")]);
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].source, "user");
    }

    #[test]
    fn own_waveform_has_the_rekordbox_shape() {
        // Two seconds at 300 rows/s; the loud second half sets the gain.
        let values: Vec<[u8; 4]> = (0..600).map(|i| if i < 300 { [40, 40, 40, 40] } else { [200, 200, 100, 50] }).collect();
        let detail = own_three_band(&values, 48_000, 24_000, false);
        assert_eq!(detail.columns, 300);
        assert_eq!(detail.columns_per_second, Some(150));
        assert_eq!(detail.data.len(), 300 * 3);
        assert!(detail.data.iter().all(|v| *v <= SCALE));
        assert_eq!(detail.data[299 * 3], 120);
        let overview = own_three_band(&values, 48_000, 24_000, true);
        assert_eq!(overview.columns, 600);
        assert_eq!(overview.columns_per_second, None);
        assert_eq!(overview.duration_ms, 2000.0);
    }

    #[test]
    fn rekordbox_waveform_is_shifted_by_the_timing_offset() {
        let analysis = RbAnalysis { content_id: "1".into(), beats: vec![], overview: Some(vec![200; 1200 * 3]), detail: Some(vec![10; 300 * 3]), phrases: None };
        let detail = rekordbox_three_band(&analysis, false, 26.0, 2000.0).unwrap();
        assert_eq!((detail.columns, detail.start_ms, detail.duration_ms), (300, -26.0, 2000.0));
        let overview = rekordbox_three_band(&analysis, true, 26.0, 180_000.0).unwrap();
        assert_eq!(overview.columns, 1200);
        assert!(overview.data.iter().all(|v| *v == SCALE));
    }
}

/// Reads the owner's rekordbox library read-only and prints a summary per file
/// (`PLUMDECK_RB_SAMPLE_PATHS`, newline-separated audio paths). Local only.
#[cfg(test)]
mod real_library {
    use super::*;

    #[test]
    #[ignore = "needs a local rekordbox library"]
    fn summarize_real_tracks() {
        let paths = std::env::var("PLUMDECK_RB_SAMPLE_PATHS").unwrap_or_default();
        for path in paths.lines().filter(|l| !l.trim().is_empty()) {
            match read_analysis(path) {
                Err(why) => println!("- unavailable: {}", why.reason()),
                Ok(a) => {
                    let offset = offset_ms(path);
                    let max = |d: &Option<Vec<u8>>| d.as_ref().map(|d| (d.len() / 3, d.iter().copied().max().unwrap_or(0)));
                    let items = a.phrases.as_ref().map(|s| phrase_items(s, &a.beats, offset, 600_000.0)).unwrap_or_default();
                    let memory = with_library(|lib| Ok(rekordbox_memory_cues(&lib.db, &a.content_id, path))).unwrap_or_default();
                    println!(
                        "- offset {offset} beats {} overview {:?} detail {:?} phrases {} [{}] memory {}",
                        a.beats.len(),
                        max(&a.overview),
                        max(&a.detail),
                        items.len(),
                        items.iter().take(4).map(|i| i["label"].as_str().unwrap_or("?").to_owned()).collect::<Vec<_>>().join(", "),
                        memory.len()
                    );
                }
            }
        }
    }
}
