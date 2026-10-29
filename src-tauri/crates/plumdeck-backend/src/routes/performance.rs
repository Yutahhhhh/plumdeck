//! Performance metadata and rekordbox imports (backend/api/routers/performance_metadata.py
//! over performance_metadata_app_service.py, grid_candidate_service.py,
//! infra/rekordbox_grid.py and infra/rekordbox_cues.py; S5).
//!
//! The metadata GET resolves a missing grid from rekordbox's PQTZ, then the
//! stored analysis. Python then started a background whole-track analysis
//! (`grid_pending: true`); that needs the analyzer and comes with S11, so
//! the port answers `grid_pending: false` until then (deviation D-19).
//! `POST /api/tracks/{id}/grid-analysis` and the grid-jobs routes are S11.

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use plumdeck_core::compat::pyfmt::median;
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::compat::pyjson_loads::loads;
use plumdeck_core::compat::text::py_strip;
use plumdeck_core::error::ApiError;
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::performance::{
    cache_grid, cached_grid, get_many, get_metadata, save_metadata, stored_analysis, tracks_and_revisions, MetadataRow,
};
use plumdeck_db::repos::tracks::{get_track, BigInt, TrackRow};
use plumdeck_db::DbError;
use plumdeck_domain::performance::{adjust_grid, playback_grid, BeatGrid, CuePoint, MetadataWrite, Num, SavedLoop, GRID_VERSION};
use plumdeck_media::timing::timing_offset_ms;
use plumdeck_rekordbox::anlz::AnlzFile;
use plumdeck_rekordbox::locate::{default_master_db_path, resolve_analysis_path};
use plumdeck_rekordbox::{MasterDb, RekordboxError};
use sha2::{Digest, Sha256};

use crate::body::{read_body, Loc, ValidationError};
use crate::query::Params;
use crate::response::{boolean, datetime, dict, float, int, opt, pydantic, text};
use crate::{server_error, AppState, HttpError};

/// `rekordbox_timing.VERSION`.
const TIMING_VERSION: &str = "rekordbox-decoder-timing-v1";
/// rekordbox Kind → pad slot (Kind 4 is reserved; D–H are 5–9).
const HOT_CUE_SLOTS: [(i64, i128); 8] = [(1, 0), (2, 1), (3, 2), (5, 3), (6, 4), (7, 5), (8, 6), (9, 7)];
const MAX_BULK_IMPORT_ERRORS: usize = 100;

/// A service failure and its HTTP answer (`_raise_service_error`).
enum Failure {
    Status(u16, String),
    Conflict(i64),
    Server(String),
}

impl Failure {
    fn response(self) -> Response {
        match self {
            Failure::Status(status, detail) => HttpError(ApiError::new(status, detail)).into_response(),
            Failure::Conflict(current) => HttpError(ApiError::json(
                409,
                serde_json::json!({
                    "message": format!("Performance metadata revision conflict; current revision is {current}"),
                    "current_revision": current,
                }),
            ))
            .into_response(),
            Failure::Server(reason) => server_error(&reason),
        }
    }
}

impl From<DbError> for Failure {
    fn from(error: DbError) -> Self {
        Failure::Server(error.to_string())
    }
}

fn python_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_nanos() as i128)
}

/// `audio_fingerprint`: path, size and mtime (or "missing"), hashed.
pub fn audio_fingerprint(filepath: &str) -> String {
    let value = match std::fs::metadata(filepath) {
        Ok(meta) => PyValue::List(vec![PyValue::Str(filepath.into()), PyValue::Int(meta.len().into()), PyValue::Int(mtime_ns(&meta))]),
        Err(_) => PyValue::List(vec![PyValue::Str(filepath.into()), PyValue::Str("missing".into())]),
    };
    sha256_hex(&dumps(&value, &DumpOptions::default()).expect("plain JSON"))
}

fn json_text(value: &PyValue, compact: bool) -> String {
    let options = if compact { DumpOptions { separators: Some((",".into(), ":".into())), ..DumpOptions::default() } } else { DumpOptions::default() };
    dumps(value, &options).expect("finite JSON")
}

/// pydantic `model_dump_json` of a grid (compact, non-ASCII kept).
fn grid_json(grid: &BeatGrid) -> String {
    dumps(&grid.json(), &DumpOptions { ensure_ascii: false, separators: Some((",".into(), ":".into())), ..DumpOptions::default() })
        .expect("finite JSON")
}

fn not_found_track(track_id: i128) -> Failure {
    Failure::Status(404, format!("Track {track_id} not found"))
}

fn within_duration(track: &TrackRow, grid: BeatGrid) -> Result<BeatGrid, Failure> {
    let duration_ms = track.duration.filter(|d| *d != 0.0).unwrap_or(0.0) * 1000.0;
    if duration_ms > 0.0 && grid.last_ms() >= duration_ms {
        return Err(Failure::Status(
            422,
            "The source grid extends beyond the catalog track duration; verify the audio duration before saving".into(),
        ));
    }
    Ok(grid)
}

fn rekordbox_path() -> Option<PathBuf> {
    default_master_db_path()
}

const LIBRARY_UNREADABLE: &str = "Could not read the local rekordbox library";

/// `find_analysis`: the ANLZ file and content id for an exact audio path.
fn find_analysis(filepath: &str) -> Result<Option<(PathBuf, String)>, Failure> {
    let Some(database) = rekordbox_path().filter(|path| path.is_file()) else { return Ok(None) };
    let db = MasterDb::open(&database).map_err(|_| Failure::Status(503, LIBRARY_UNREADABLE.into()))?;
    let rows = db.contents_at_path(filepath).map_err(|_| Failure::Status(503, LIBRARY_UNREADABLE.into()))?;
    if rows.is_empty() {
        return Ok(None);
    }
    let paths: std::collections::BTreeSet<String> =
        rows.iter().filter_map(|row| row.analysis_data_path.clone()).filter(|path| !path.is_empty()).collect();
    if paths.len() > 1 {
        return Err(Failure::Status(503, "Multiple rekordbox grids match this audio path".into()));
    }
    let Some(raw) = paths.into_iter().next() else { return Ok(None) };
    Ok(resolve_analysis_path(&database, &raw).map(|path| (path, rows[0].id.clone())))
}

/// `read_grid` / `parse_pqtz`.
fn read_grid(path: &FsPath) -> Result<Option<BeatGrid>, Failure> {
    let file = AnlzFile::read(path).map_err(|_| Failure::Status(503, "Invalid ANLZ header".into()))?;
    let Some(pqtz) = file.beat_grid() else { return Ok(None) };
    if pqtz.beats.len() < 2 {
        return Ok(None);
    }
    let tempos: Vec<f64> = pqtz.beats.iter().map(|b| f64::from(b.tempo_x100)).collect();
    let bpm = median(&tempos).map_err(|_| Failure::Status(503, "Invalid PQTZ beat values".into()))? / 100.0;
    let grid = BeatGrid {
        bpm: Num::Float(bpm),
        first_beat_ms: Num::Int(pqtz.beats[0].time_ms.into()),
        beats_per_bar: pqtz.beats.iter().map(|b| i128::from(b.beat_number)).max().unwrap_or(4),
        beat_times_ms: Some(pqtz.beats.iter().map(|b| Num::Int(b.time_ms.into())).collect()),
        beat_numbers: Some(pqtz.beats.iter().map(|b| i128::from(b.beat_number)).collect()),
        source: "rekordbox".into(),
        confidence: None,
    };
    grid.check().map_err(|_| Failure::Status(503, "Invalid PQTZ beat values".into()))?;
    Ok(Some(grid))
}

fn parse_grid_json(text: &str) -> Option<BeatGrid> {
    loads(text).ok().and_then(|value| BeatGrid::parse(&value).ok())
}

/// `GridCandidateService.rekordbox`.
fn rekordbox_grid(conn: &Connection, track: &TrackRow, refresh: bool, now: NaiveDateTime) -> Result<Option<BeatGrid>, Failure> {
    let Some((path, external_id)) = find_analysis(&track.filepath)? else { return Ok(None) };
    let meta = std::fs::metadata(&path).map_err(|_| Failure::Status(503, "The rekordbox analysis file is unavailable".into()))?;
    let fingerprint = sha256_hex(&json_text(
        &PyValue::List(vec![
            PyValue::Str(path.to_string_lossy().into_owned()),
            PyValue::Int(meta.len().into()),
            PyValue::Int(mtime_ns(&meta)),
            PyValue::Str(audio_fingerprint(&track.filepath)),
            PyValue::Str(TIMING_VERSION.into()),
            PyValue::Str(python_platform().into()),
        ]),
        false,
    ));
    if !refresh {
        if let Some(grid) = cached_grid(conn, track.id, "rekordbox", &fingerprint)?.and_then(|text| parse_grid_json(&text)) {
            if grid.source == "rekordbox" {
                return within_duration(track, grid).map(Some);
            }
        }
    }
    let Some(grid) = read_grid(&path)? else { return Ok(None) };
    let offset = timing_offset_ms(&track.filepath).map_err(|message| Failure::Status(503, message.into()))?;
    let grid = adjust_grid(&grid, offset).map_err(|error| Failure::Status(503, error.0))?;
    let grid = within_duration(track, grid)?;
    let provenance = PyValue::Dict(vec![
        (PyValue::Str("database".into()), PyValue::Str(rekordbox_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "None".into()))),
        (PyValue::Str("analysis_path".into()), PyValue::Str(path.to_string_lossy().into_owned())),
        (PyValue::Str("external_track_id".into()), PyValue::Str(external_id)),
        (PyValue::Str("format".into()), PyValue::Str("PQTZ".into())),
        (PyValue::Str("read_only".into()), PyValue::Bool(true)),
        (PyValue::Str("timing_offset_ms".into()), PyValue::Float(offset)),
        (PyValue::Str("timing_version".into()), PyValue::Str(TIMING_VERSION.into())),
    ]);
    cache_grid(conn, track.id, "rekordbox", &grid_json(&grid), &fingerprint, &json_text(&provenance, false), now)?;
    Ok(Some(grid))
}

/// `existing_analysis`: the cached analysis grid, the stored playback grid
/// or one fitted from the stored beats.
fn existing_analysis(conn: &Connection, track: &TrackRow) -> Result<Option<BeatGrid>, Failure> {
    let key = format!("{}:grid-v{GRID_VERSION}", audio_fingerprint(&track.filepath));
    if let Some(grid) = cached_grid(conn, track.id, "analysis", &key)?.and_then(|text| parse_grid_json(&text)) {
        if grid.source == "analysis" {
            return within_duration(track, grid).map(Some);
        }
    }
    let Some((extra, beats)) = stored_analysis(conn, BigInt(track.id.into()))? else { return Ok(None) };
    let extra = extra.and_then(|text| loads(&text).ok()).unwrap_or(PyValue::Dict(Vec::new()));
    let field = |name: &str| match &extra {
        PyValue::Dict(pairs) => pairs.iter().find(|(k, _)| matches!(k, PyValue::Str(n) if n == name)).map(|(_, v)| v.clone()),
        _ => None,
    };
    let analysis_error = |message: &str| Failure::Status(422, message.into());
    if field("playback_grid_version") == Some(PyValue::Int(GRID_VERSION.into())) {
        if let Some(prepared) = field("playback_grid").filter(plumdeck_core::compat::pyobj::truthy) {
            let grid = BeatGrid::parse(&prepared).map_err(|_| analysis_error("Stored analysis has invalid playback grid"))?;
            return within_duration(track, grid).map(Some);
        }
    }
    let ticks = plumdeck_db::array_codec::unpack_f32(beats.as_deref()).unwrap_or_default();
    if ticks.len() < 2 {
        return Ok(None);
    }
    let mut bpm = track.bpm.unwrap_or(f64::NAN);
    if !bpm.is_finite() || !(20.0..=300.0).contains(&bpm) {
        let intervals: Vec<f64> = ticks.windows(2).map(|p| p[1] - p[0]).collect();
        if !intervals.iter().all(|d| d.is_finite() && *d > 0.0) {
            return Err(analysis_error("Stored analysis has invalid beat timestamps"));
        }
        bpm = 60.0 / median(&intervals).map_err(|_| analysis_error("Stored analysis has invalid beat timestamps"))?;
    }
    let confidence = field("bpm_confidence").and_then(|value| match value {
        PyValue::Int(n) => Some(Num::Int(n)),
        PyValue::Float(x) => Some(Num::Float(x)),
        _ => None,
    });
    match playback_grid(&ticks, bpm, confidence) {
        Ok(Some(grid)) => within_duration(track, grid).map(Some),
        Ok(None) => Ok(None),
        Err(_) => Err(analysis_error("Analysis returned invalid beat values")),
    }
}

/// `_decode`: stored JSON, or the fallback for empty or invalid text.
fn decode(raw: &Option<String>, fallback: PyValue) -> PyValue {
    raw.as_deref().filter(|text| !text.is_empty()).and_then(|text| loads(text).ok()).unwrap_or(fallback)
}

/// `_item` validated as `PerformanceMetadataRead`; `Err` (a 500) when stored
/// JSON does not fit the model.
fn read_model(track_id: i64, row: Option<&MetadataRow>, grid_override: Option<BeatGrid>, warning: Option<String>) -> Result<PyValue, Failure> {
    let bad = || Failure::Server("response validation failed (PerformanceMetadataRead)".into());
    let (revision, cues, loops, grid, created, updated) = match row {
        None => (0, PyValue::List(Vec::new()), PyValue::List(Vec::new()), PyValue::None, None, None),
        Some(row) => (
            row.revision,
            decode(&row.cue_points_json, PyValue::List(Vec::new())),
            decode(&row.loops_json, PyValue::List(Vec::new())),
            decode(&row.beat_grid_json, PyValue::None),
            row.created_at,
            row.updated_at,
        ),
    };
    let list = |value: &PyValue| match value {
        PyValue::List(items) => Ok(items.clone()),
        _ => Err(bad()),
    };
    let cues = list(&cues)?.iter().map(|item| CuePoint::parse(item).map(|cue| cue.json())).collect::<Result<Vec<_>, _>>().map_err(|_| bad())?;
    let loops = list(&loops)?.iter().map(|item| SavedLoop::parse(item).map(|l| l.json())).collect::<Result<Vec<_>, _>>().map_err(|_| bad())?;
    let grid = match (grid_override, &grid) {
        (Some(grid), _) => grid.json(),
        (None, PyValue::None) => PyValue::None,
        (None, value) => BeatGrid::parse(value).map_err(|_| bad())?.json(),
    };
    Ok(dict([
        ("track_id", int(track_id)),
        ("revision", int(revision)),
        ("cue_points", PyValue::List(cues)),
        ("loops", PyValue::List(loops)),
        ("beat_grid", grid),
        ("created_at", opt(created.as_ref(), datetime)),
        ("updated_at", opt(updated.as_ref(), datetime)),
        ("grid_warning", opt(warning, text)),
        ("grid_pending", boolean(false)),
    ]))
}

fn track_or_404(conn: &Connection, track_id: i128) -> Result<TrackRow, Failure> {
    get_track(conn, BigInt(track_id))?.ok_or_else(|| not_found_track(track_id))
}

/// `get`: the stored metadata, with an automatic grid when none is saved.
fn get_item(conn: &Connection, track_id: i128, now: NaiveDateTime) -> Result<PyValue, Failure> {
    let track = track_or_404(conn, track_id)?;
    let row = get_metadata(conn, track.id)?;
    let stored_grid = row.as_ref().map(|row| decode(&row.beat_grid_json, PyValue::None)).unwrap_or(PyValue::None);
    if stored_grid != PyValue::None {
        return read_model(track.id, row.as_ref(), None, None);
    }
    let mut warnings = Vec::new();
    match rekordbox_grid(conn, &track, false, now) {
        Ok(Some(grid)) => return read_model(track.id, row.as_ref(), Some(grid), None),
        Ok(None) => {}
        Err(Failure::Status(_, message)) => warnings.push(format!("rekordbox: {message}")),
        Err(other) => return Err(other),
    }
    let grid = match existing_analysis(conn, &track) {
        Ok(grid) => grid,
        Err(Failure::Status(_, message)) => {
            warnings.push(format!("analysis: {message}"));
            return read_model(track.id, row.as_ref(), None, Some(warnings.join("; ")));
        }
        Err(other) => return Err(other),
    };
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    read_model(track.id, row.as_ref(), grid, warning)
}

/// `replace`: optimistic write of the whole payload.
fn replace(conn: &Connection, track_id: i128, request: &MetadataWrite, now: NaiveDateTime) -> Result<PyValue, Failure> {
    let track = track_or_404(conn, track_id)?;
    request.check_duration(track.duration).map_err(|error| Failure::Status(422, error.0))?;
    let row = get_metadata(conn, track.id)?;
    let current = row.as_ref().map_or(0, |row| row.revision);
    if request.revision != i128::from(current) {
        return Err(Failure::Conflict(current));
    }
    let cues = json_text(&PyValue::List(request.cue_points.iter().map(CuePoint::json).collect()), true);
    let loops = json_text(&PyValue::List(request.loops.iter().map(SavedLoop::json).collect()), true);
    let grid = request.beat_grid.as_ref().map(|grid| json_text(&grid.json(), true));
    let saved = save_metadata(conn, track.id, &cues, &loops, grid.as_deref(), current + 1, row.is_some(), now)?;
    read_model(track.id, Some(&saved), None, None)
}

/// Stored cues/loops/grid of a row as the model, for an import that keeps them.
fn current_parts(row: Option<&MetadataRow>) -> Result<(Vec<SavedLoop>, Option<BeatGrid>), Failure> {
    let bad = || Failure::Status(422, "stored metadata is invalid".into());
    let loops = match row.map(|row| decode(&row.loops_json, PyValue::List(Vec::new()))) {
        Some(PyValue::List(items)) => items.iter().map(SavedLoop::parse).collect::<Result<Vec<_>, _>>().map_err(|_| bad())?,
        _ => Vec::new(),
    };
    let grid = match row.map(|row| decode(&row.beat_grid_json, PyValue::None)) {
        None | Some(PyValue::None) => None,
        Some(value) => Some(BeatGrid::parse(&value).map_err(|_| bad())?),
    };
    Ok((loops, grid))
}

/// rekordbox cue rows (`Kind`, `InMsec`, `Comment`) as pads A–H, shifted.
fn cue_points(rows: &[(i64, Option<i64>, Option<String>)], filepath: &str) -> Result<Vec<CuePoint>, String> {
    let mut cues: Vec<CuePoint> = Vec::new();
    for (kind, position, comment) in rows {
        let Some((_, slot)) = HOT_CUE_SLOTS.iter().find(|(k, _)| k == kind) else { continue };
        if cues.iter().any(|cue| cue.slot == *slot) {
            return Err(format!("Multiple rekordbox cues use hot cue slot {}", slot + 1));
        }
        let Some(position) = position.filter(|p| *p >= 0) else {
            return Err(format!("Invalid rekordbox hot cue position in slot {}", slot + 1));
        };
        let label = py_strip(comment.as_deref().unwrap_or("")).to_owned();
        let label = if label.is_empty() { char::from(b'A' + *slot as u8).to_string() } else { label.chars().take(100).collect() };
        cues.push(CuePoint { slot: *slot, position_ms: Num::Int(position.into()), label, color: None });
    }
    if cues.is_empty() {
        return Ok(cues);
    }
    let offset = timing_offset_ms(filepath).map_err(str::to_owned)?;
    Ok(cues
        .into_iter()
        .map(|cue| CuePoint { position_ms: if offset == 0.0 { cue.position_ms } else { Num::Float((cue.position_ms.value() - offset).max(0.0)) }, ..cue })
        .collect())
}

fn open_library() -> Result<MasterDb, Failure> {
    let Some(database) = rekordbox_path().filter(|path| path.is_file()) else {
        return Err(Failure::Status(404, "The local rekordbox library is unavailable".into()));
    };
    MasterDb::open(&database).map_err(|_| Failure::Status(503, "Could not read cues from the local rekordbox library".into()))
}

fn cue_rows(db: &MasterDb, content_id: &str) -> Result<Vec<(i64, Option<i64>, Option<String>)>, Failure> {
    let cues = db.cues(content_id).map_err(|_| Failure::Status(503, "Could not read cues from the local rekordbox library".into()))?;
    let mut rows: Vec<_> = cues
        .into_iter()
        .filter(|cue| HOT_CUE_SLOTS.iter().any(|(kind, _)| *kind == cue.kind))
        .map(|cue| (cue.kind, cue.in_msec, cue.comment))
        .collect();
    rows.sort_by_key(|row| row.0);
    Ok(rows)
}

/// `read_hot_cues`.
fn read_hot_cues(filepath: &str) -> Result<Vec<CuePoint>, Failure> {
    let db = open_library()?;
    let content = match db.content_at_path(filepath) {
        Ok(Some(content)) => content,
        Ok(None) => return Err(Failure::Status(404, "No rekordbox track matches this audio file path".into())),
        Err(RekordboxError::Ambiguous) => return Err(Failure::Status(503, "Multiple rekordbox tracks match this audio path".into())),
        Err(_) => return Err(Failure::Status(503, "Could not read cues from the local rekordbox library".into())),
    };
    let rows = cue_rows(&db, &content.id)?;
    cue_points(&rows, filepath).map_err(|message| Failure::Status(503, message))
}

fn path_track_id(params: &mut Params, value: &str) -> Option<i128> {
    params.path_int("track_id", value)
}

async fn run(state: &AppState, work: impl FnOnce(&Connection, NaiveDateTime) -> Result<PyValue, Failure> + Send + 'static, write: bool) -> Response {
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let result = state
        .blocking_result(move || {
            if write {
                db.write(|conn| Ok(work(conn, now)))
            } else {
                db.lease().map(|conn| work(&conn, now))
            }
        })
        .await;
    match result {
        Ok(Ok(Ok(value))) => pydantic(value).into_response(),
        Ok(Ok(Err(failure))) => failure.response(),
        Ok(Err(error)) => server_error(&error),
        Err(response) => response,
    }
}

/// `GET /api/tracks/{track_id}/performance-metadata`.
pub async fn get_route(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_track_id(&mut params, &track_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    // The grid cache is written on a first rekordbox read.
    run(&state, move |conn, now| get_item(conn, id, now), true).await
}

fn body_error(body: Option<PyValue>) -> ValidationError {
    ValidationError::new("model_type", vec![Loc::from("body")], "Input should be valid", body.unwrap_or(PyValue::None))
}

/// `PUT /api/tracks/{track_id}/performance-metadata`.
pub async fn put_route(State(state): State<AppState>, Path(track_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_track_id(&mut params, &track_id);
    let request = match body.as_ref().map(MetadataWrite::parse) {
        Some(Ok(request)) => Some(request),
        _ => {
            params.extend(vec![body_error(body.clone())]);
            None
        }
    };
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, request) = (id.expect("validated"), request.expect("validated"));
    run(&state, move |conn, now| replace(conn, id, &request, now), true).await
}

/// `POST /api/tracks/performance-metadata/cue-points`: hot cue positions of
/// up to 500 tracks, 8 slots each (stored data only).
pub async fn cue_points_route(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let ids: Option<Vec<i64>> = match &body {
        Some(PyValue::Dict(pairs)) => match pairs.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(n) if n == "track_ids")) {
            None => Some(Vec::new()),
            Some((_, PyValue::List(items))) if items.len() <= 500 => {
                items.iter().map(|item| crate::lax::to_int(item).and_then(|n| i64::try_from(n).ok())).collect()
            }
            _ => None,
        },
        _ => None,
    };
    let Some(ids) = ids else { return crate::body::validation_response(&[body_error(body)]) };
    let db = state.db.clone();
    let result = state.blocking(move || db.lease().and_then(|conn| get_many(&conn, &ids))).await;
    let rows = match result {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut summary = Vec::new();
    for row in rows {
        let Ok(PyValue::List(cues)) = loads(row.cue_points_json.as_deref().unwrap_or("[]")) else { continue };
        let mut slots: Vec<Option<f64>> = vec![None; 8];
        for cue in &cues {
            let PyValue::Dict(pairs) = cue else { continue };
            let get = |key: &str| pairs.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(n) if n == key)).map(|(_, v)| v);
            let slot = match get("slot") {
                Some(PyValue::Int(slot)) if (0..=7).contains(slot) => *slot as usize,
                Some(PyValue::Bool(flag)) => usize::from(*flag),
                _ => continue,
            };
            let position = match get("position_ms") {
                Some(PyValue::Int(n)) => *n as f64,
                Some(PyValue::Float(x)) => *x,
                Some(PyValue::Bool(flag)) => f64::from(u8::from(*flag)),
                _ => continue,
            };
            slots[slot] = Some(position);
        }
        if slots.iter().any(Option::is_some) {
            summary.push((row.track_id.to_string(), PyValue::List(slots.into_iter().map(|s| opt(s, float)).collect())));
        }
    }
    pydantic(dict(summary)).into_response()
}

/// `POST /api/tracks/{track_id}/grid-rekordbox`: re-read the PQTZ grid.
pub async fn grid_rekordbox_route(State(state): State<AppState>, Path(track_id): Path<String>) -> Response {
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_track_id(&mut params, &track_id);
    if let Err(response) = params.finish() {
        return response;
    }
    let id = id.expect("validated");
    run(
        &state,
        move |conn, now| {
            let track = track_or_404(conn, id)?;
            match rekordbox_grid(conn, &track, true, now)? {
                Some(grid) => Ok(grid.json()),
                None => Err(Failure::Status(404, "No rekordbox PQTZ grid is available for this audio file".into())),
            }
        },
        true,
    )
    .await
}

fn revision_body(body: &Option<PyValue>) -> Option<i128> {
    let Some(PyValue::Dict(pairs)) = body else { return None };
    if pairs.iter().any(|(k, _)| !matches!(k, PyValue::Str(n) if n == "revision")) {
        return None;
    }
    match pairs.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(n) if n == "revision")).map(|(_, v)| v) {
        Some(PyValue::Int(n)) if *n >= 0 => Some(*n),
        _ => None,
    }
}

/// `POST /api/tracks/{track_id}/cues-rekordbox`: replace the cues with
/// rekordbox's pads A–H, keeping loops and grid.
pub async fn import_cues_route(State(state): State<AppState>, Path(track_id): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let mut params = Params::from_uri(&Uri::from_static("/"));
    let id = path_track_id(&mut params, &track_id);
    let revision = revision_body(&body);
    if revision.is_none() {
        params.extend(vec![body_error(body.clone())]);
    }
    if let Err(response) = params.finish() {
        return response;
    }
    let (id, revision) = (id.expect("validated"), revision.expect("validated"));
    run(
        &state,
        move |conn, now| {
            let track = track_or_404(conn, id)?;
            let cues = read_hot_cues(&track.filepath)?;
            let row = get_metadata(conn, track.id)?;
            let (loops, beat_grid) = current_parts(row.as_ref())?;
            let saved = replace(conn, id, &MetadataWrite { revision, cue_points: cues, loops, beat_grid }, now)?;
            // MEMORY CUEs come along (added, never removed); a failure there
            // does not undo the hot-cue import.
            let _ = crate::routes::rekordbox_view::import_memory_cues(conn, track.id, &track.filepath, now);
            Ok(saved)
        },
        true,
    )
    .await
}

/// `POST /api/tracks/cues-rekordbox/import`: every exact-path match.
pub async fn import_all_route(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let body = match read_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if !matches!(&body, Some(PyValue::Dict(pairs)) if pairs.is_empty()) {
        return crate::body::validation_response(&[body_error(body)]);
    }
    run(
        &state,
        |conn, now| {
            let (tracks, revisions) = tracks_and_revisions(conn)?;
            let revisions: BTreeMap<i64, i64> = revisions.into_iter().collect();
            let db = open_library()?;
            let mut source: BTreeMap<String, Result<Vec<CuePoint>, String>> = BTreeMap::new();
            let mut contents: BTreeMap<String, String> = BTreeMap::new();
            for (_, filepath) in &tracks {
                if source.contains_key(filepath) {
                    continue;
                }
                let found = match db.content_at_path(filepath) {
                    Ok(Some(content)) => Some({ contents.insert(filepath.clone(), content.id.clone()); cue_rows(&db, &content.id) }.map_err(|_| "Could not read cues from the local rekordbox library".to_owned())
                        .and_then(|rows| cue_points(&rows, filepath))),
                    Ok(None) => None,
                    Err(RekordboxError::Ambiguous) => Some(Err("Multiple rekordbox tracks match this audio path".into())),
                    Err(_) => return Err(Failure::Status(503, "Could not read cues from the local rekordbox library".into())),
                };
                if let Some(found) = found {
                    source.insert(filepath.clone(), found);
                }
            }
            let (mut imported, mut skipped, mut failed, mut conflicts) = (0i64, 0i64, 0i64, 0i64);
            let mut errors: Vec<PyValue> = Vec::new();
            let error = |track_id: i64, message: String| dict([("track_id", int(track_id)), ("message", text(message))]);
            for (track_id, filepath) in &tracks {
                let cues = match source.get(filepath) {
                    None => {
                        skipped += 1;
                        continue;
                    }
                    Some(Err(message)) => {
                        failed += 1;
                        errors.push(error(*track_id, message.clone()));
                        continue;
                    }
                    Some(Ok(cues)) => cues.clone(),
                };
                let row = get_metadata(conn, *track_id)?;
                let current = row.as_ref().map_or(0, |row| row.revision);
                let expected = revisions.get(track_id).copied().unwrap_or(0);
                if current != expected {
                    conflicts += 1;
                    errors.push(error(*track_id, format!("Performance metadata revision conflict; current revision is {current}")));
                    continue;
                }
                let outcome = current_parts(row.as_ref()).and_then(|(loops, beat_grid)| {
                    replace(conn, i128::from(*track_id), &MetadataWrite { revision: expected.into(), cue_points: cues, loops, beat_grid }, now)
                });
                match outcome {
                    Ok(_) => {
                        imported += 1;
                        if let Some(content_id) = contents.get(filepath) {
                            let _ = crate::routes::rekordbox_view::import_memory_cues_with(&db, conn, *track_id, content_id, filepath, now);
                        }
                    }
                    Err(Failure::Conflict(current)) => {
                        conflicts += 1;
                        errors.push(error(*track_id, format!("Performance metadata revision conflict; current revision is {current}")));
                    }
                    Err(Failure::Status(_, message)) | Err(Failure::Server(message)) => {
                        failed += 1;
                        errors.push(error(*track_id, message));
                    }
                }
            }
            let truncated = errors.len().saturating_sub(MAX_BULK_IMPORT_ERRORS);
            errors.truncate(MAX_BULK_IMPORT_ERRORS);
            Ok(dict([
                ("imported", int(imported)),
                ("skipped", int(skipped)),
                ("failed", int(failed)),
                ("conflicts", int(conflicts)),
                ("errors", PyValue::List(errors)),
                ("errors_truncated", int(truncated as i64)),
            ]))
        },
        true,
    )
    .await
}
