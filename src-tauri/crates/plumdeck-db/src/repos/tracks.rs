//! The `tracks` table behind the library routes (S3):
//! backend/infra/repositories/track_repository.py (`search_tracks`,
//! `search_tracks_page`, `search_track_ids`, `update_genre`) and the
//! analyzed rule of backend/utils/ingestion.py.
//!
//! The SQL is what SQLAlchemy sends for those queries (compiled with the
//! duckdb-engine dialect), with bound parameters in the same places, so
//! DuckDB evaluates the same expressions: ILIKE patterns are not escaped
//! (`%` and `_` in a search act as wildcards, as in Python), and the BPM
//! window is computed in f64 before binding. Orders without a tie-breaker
//! (`search_track_ids`) stay undefined, as in Python.
//!
//! One intended difference in shape, not in result: Python asks for the
//! analyzed ids of a page in a second query (`analyzed_track_ids`); here the
//! same expression is a column of the page query.

use chrono::NaiveDateTime;
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection, OptionalExt, Row};
use plumdeck_core::compat::pyjson_loads::loads;
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::text::{py_strip, search_tokens};
use plumdeck_domain::audio_math::{camelot_order, key_spellings};

use super::analyzed::COMPLETED_ANALYSIS_SQL;
use super::settings::finish;
use crate::DbError;

/// `EXCERPT_LIGHT_RHYTHM`: light rows without the whole-track grid.
pub const EXCERPT_LIGHT_RHYTHM: [&str; 2] = ["librosa-rhythm-light-v1", "numpy-autocorrelation-light-v2"];

/// The `Track` model's columns, in model order.
pub const TRACK_COLUMNS: &str = "tracks.id, tracks.filepath, tracks.title, tracks.artist, tracks.album, tracks.genre, \
tracks.subgenre, tracks.year, tracks.bpm, tracks.key, tracks.scale, tracks.duration, tracks.analysis_level, \
tracks.energy, tracks.danceability, tracks.loudness, tracks.brightness, tracks.noisiness, tracks.contrast, \
tracks.loudness_range, tracks.spectral_flux, tracks.spectral_rolloff, tracks.is_genre_verified, tracks.created_at";

const TRACK_COLUMN_COUNT: usize = 24;

/// A `tracks` row as SQLModel loads it: no validation, so nullable columns
/// stay `None` where the model declares a default. FLOAT columns are the
/// f32 values widened exactly, as Python's float.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub id: i64,
    pub filepath: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub year: Option<i64>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub scale: Option<String>,
    pub duration: Option<f64>,
    pub analysis_level: Option<String>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub loudness: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
    pub contrast: Option<f64>,
    pub loudness_range: Option<f64>,
    pub spectral_flux: Option<f64>,
    pub spectral_rolloff: Option<f64>,
    pub is_genre_verified: Option<bool>,
    pub created_at: Option<NaiveDateTime>,
}

fn float(row: &Row<'_>, index: usize) -> duckdb::Result<Option<f64>> {
    Ok(row.get::<_, Option<f32>>(index)?.map(f64::from))
}

impl TrackRow {
    /// Read [`TRACK_COLUMNS`] starting at column `at`.
    pub fn from_row(row: &Row<'_>, at: usize) -> duckdb::Result<Self> {
        Ok(Self {
            id: row.get(at)?,
            filepath: row.get(at + 1)?,
            title: row.get(at + 2)?,
            artist: row.get(at + 3)?,
            album: row.get(at + 4)?,
            genre: row.get(at + 5)?,
            subgenre: row.get(at + 6)?,
            year: row.get(at + 7)?,
            bpm: float(row, at + 8)?,
            key: row.get(at + 9)?,
            scale: row.get(at + 10)?,
            duration: float(row, at + 11)?,
            analysis_level: row.get(at + 12)?,
            energy: float(row, at + 13)?,
            danceability: float(row, at + 14)?,
            loudness: float(row, at + 15)?,
            brightness: float(row, at + 16)?,
            noisiness: float(row, at + 17)?,
            contrast: float(row, at + 18)?,
            loudness_range: float(row, at + 19)?,
            spectral_flux: float(row, at + 20)?,
            spectral_rolloff: float(row, at + 21)?,
            is_genre_verified: row.get(at + 22)?,
            created_at: row.get(at + 23)?,
        })
    }
}

/// One `search_tracks` result: the track, its lyrics text (LEFT JOIN) and
/// whether it meets the analyzed rule.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackHit {
    pub track: TrackRow,
    pub lyrics: Option<String>,
    pub is_analyzed: bool,
}

/// An integer query value. FastAPI accepts any size; DuckDB gets a BIGINT,
/// or a HUGEINT beyond it, as duckdb's Python binding sends it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BigInt(pub i128);

impl BigInt {
    pub(crate) fn value(self) -> Value {
        match i64::try_from(self.0) {
            Ok(small) => Value::BigInt(small),
            Err(_) => Value::HugeInt(self.0),
        }
    }
}

/// The filters of `_apply_search_conditions` that HTTP can send
/// (`target_params` is MCP-only and always `None` here).
#[derive(Clone, Debug, PartialEq)]
pub struct TrackFilters {
    pub status: String,
    pub q: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genres: Option<Vec<String>>,
    pub subgenres: Option<Vec<String>>,
    pub key: Option<String>,
    pub bpm: Option<f64>,
    pub bpm_range: f64,
    pub min_duration: Option<f64>,
    pub max_duration: Option<f64>,
    pub min_energy: Option<f64>,
    pub max_energy: Option<f64>,
    pub min_danceability: Option<f64>,
    pub max_danceability: Option<f64>,
    pub min_brightness: Option<f64>,
    pub max_brightness: Option<f64>,
    pub min_year: Option<BigInt>,
    pub max_year: Option<BigInt>,
    pub year_status: String,
    pub lyrics_status: String,
    pub lyrics: Option<String>,
}

impl Default for TrackFilters {
    fn default() -> Self {
        Self {
            status: "all".into(),
            q: None,
            title: None,
            artist: None,
            album: None,
            genres: None,
            subgenres: None,
            key: None,
            bpm: None,
            bpm_range: 5.0,
            min_duration: None,
            max_duration: None,
            min_energy: None,
            max_energy: None,
            min_danceability: None,
            max_danceability: None,
            min_brightness: None,
            max_brightness: None,
            min_year: None,
            max_year: None,
            year_status: "all".into(),
            lyrics_status: "all".into(),
            lyrics: None,
        }
    }
}

const SEARCH_FIELDS: [&str; 6] =
    ["tracks.title", "tracks.artist", "tracks.album", "tracks.genre", "tracks.subgenre", "tracks.filepath"];

/// SQL text plus its bound parameters, in order.
#[derive(Default)]
pub(crate) struct Sql {
    pub(crate) text: String,
    pub(crate) params: Vec<Value>,
}

impl Sql {
    pub(crate) fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    pub(crate) fn bind(&mut self, value: Value) {
        self.text.push('?');
        self.params.push(value);
    }

    pub(crate) fn placeholders(&mut self, values: impl IntoIterator<Item = Value>) {
        self.text.push('(');
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                self.text.push_str(", ");
            }
            self.bind(value);
        }
        self.text.push(')');
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn contains(value: &str) -> Value {
    Value::Text(format!("%{value}%"))
}

/// `or_(*[field.ilike(f"%{value}%") for field in SEARCH_FIELDS])`.
fn any_field_ilike(sql: &mut Sql, value: &str) {
    sql.push("(");
    for (index, field) in SEARCH_FIELDS.iter().enumerate() {
        if index > 0 {
            sql.push(" OR ");
        }
        sql.push(field);
        sql.push(" ILIKE ");
        sql.bind(contains(value));
    }
    sql.push(")");
}

/// `_build_global_search_condition(q)`.
fn global_search(sql: &mut Sql, q: &str) {
    let raw = py_strip(q);
    let tokens = search_tokens(q);
    if tokens.len() <= 1 {
        any_field_ilike(sql, raw);
        return;
    }
    sql.push("(");
    any_field_ilike(sql, raw);
    sql.push(" OR ");
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            sql.push(" AND ");
        }
        any_field_ilike(sql, token);
    }
    sql.push(")");
}

const VALID_LYRICS_IDS: &str = "(SELECT lyrics.track_id FROM lyrics WHERE lyrics.track_id IS NOT NULL \
AND lyrics.content IS NOT NULL AND trim(lyrics.content) != '')";

/// The WHERE clause of `_apply_search_conditions`, conditions in Python's
/// order. Empty when no filter applies.
fn where_clause(filters: &TrackFilters) -> Sql {
    let mut conditions: Vec<Sql> = Vec::new();
    let mut condition = |build: &dyn Fn(&mut Sql)| {
        let mut sql = Sql::default();
        build(&mut sql);
        conditions.push(sql);
    };
    match filters.status.as_str() {
        "analyzed" => condition(&|sql| sql.push(COMPLETED_ANALYSIS_SQL)),
        "unanalyzed" => condition(&|sql| {
            sql.push("NOT ");
            sql.push(COMPLETED_ANALYSIS_SQL);
        }),
        _ => {}
    }
    if let Some(q) = filters.q.as_deref().filter(|q| !q.is_empty()) {
        condition(&|sql| global_search(sql, q));
    }
    for (column, value) in
        [("tracks.title", &filters.title), ("tracks.artist", &filters.artist), ("tracks.album", &filters.album)]
    {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            condition(&|sql| {
                sql.push(column);
                sql.push(" ILIKE ");
                sql.bind(contains(value));
            });
        }
    }
    match filters.year_status.as_str() {
        "set" => condition(&|sql| sql.push("tracks.year IS NOT NULL AND tracks.year > 0")),
        "unset" => condition(&|sql| sql.push("(tracks.year IS NULL OR tracks.year = 0)")),
        _ => {}
    }
    if let Some(min_year) = filters.min_year {
        condition(&|sql| {
            sql.push("tracks.year >= ");
            sql.bind(min_year.value());
        });
    }
    if let Some(max_year) = filters.max_year {
        condition(&|sql| {
            sql.push("tracks.year <= ");
            sql.bind(max_year.value());
        });
    }
    match filters.lyrics_status.as_str() {
        "set" => condition(&|sql| {
            sql.push("tracks.id IN ");
            sql.push(VALID_LYRICS_IDS);
        }),
        "unset" => condition(&|sql| {
            sql.push("(tracks.id NOT IN ");
            sql.push(VALID_LYRICS_IDS);
            sql.push(")");
        }),
        _ => {}
    }
    if let Some(lyrics) = filters.lyrics.as_deref().filter(|lyrics| !lyrics.is_empty()) {
        condition(&|sql| {
            sql.push("lyrics.content ILIKE ");
            sql.bind(contains(lyrics));
        });
    }
    for (column, values) in [("tracks.genre", &filters.genres), ("tracks.subgenre", &filters.subgenres)] {
        if let Some(values) = values.as_ref().filter(|values| !values.is_empty()) {
            condition(&|sql| {
                sql.push(column);
                sql.push(" IN ");
                sql.placeholders(values.iter().map(|value| text(value)));
            });
        }
    }
    if let Some(key) = filters.key.as_deref().filter(|key| !key.is_empty()) {
        if key == "Major" || key == "Minor" {
            condition(&|sql| {
                sql.push("lower(tracks.key) LIKE ");
                sql.bind(Value::Text(format!("%{}", key.to_ascii_lowercase())));
            });
        } else {
            let spellings = key_spellings(key);
            condition(&|sql| {
                sql.push("lower(trim(tracks.key)) IN ");
                sql.placeholders(spellings.iter().map(|spelling| text(spelling)));
            });
        }
    }
    // `if bpm and bpm > 0`: NaN is truthy but not > 0.
    if let Some(bpm) = filters.bpm.filter(|bpm| *bpm > 0.0) {
        let range = filters.bpm_range;
        condition(&|sql| {
            sql.push("(");
            for (index, target) in [bpm, bpm * 0.5, bpm * 2.0].into_iter().enumerate() {
                if index > 0 {
                    sql.push(" OR ");
                }
                sql.push("tracks.bpm >= ");
                sql.bind(Value::Double(target - range));
                sql.push(" AND tracks.bpm <= ");
                sql.bind(Value::Double(target + range));
            }
            sql.push(")");
        });
    }
    for (column, op, value) in [
        ("tracks.energy", ">=", filters.min_energy),
        ("tracks.energy", "<=", filters.max_energy),
        ("tracks.danceability", ">=", filters.min_danceability),
        ("tracks.danceability", "<=", filters.max_danceability),
        ("tracks.brightness", ">=", filters.min_brightness),
        ("tracks.brightness", "<=", filters.max_brightness),
        ("tracks.duration", ">=", filters.min_duration),
        ("tracks.duration", "<=", filters.max_duration),
    ] {
        if let Some(value) = value {
            condition(&|sql| {
                sql.push(column);
                sql.push(" ");
                sql.push(op);
                sql.push(" ");
                sql.bind(Value::Double(value));
            });
        }
    }
    let mut out = Sql::default();
    for (index, part) in conditions.into_iter().enumerate() {
        out.push(if index == 0 { " WHERE " } else { " AND " });
        out.push(&part.text);
        out.params.extend(part.params);
    }
    out
}

/// `SORTABLE_FIELDS`.
pub const SORTABLE_FIELDS: [&str; 12] =
    ["title", "artist", "album", "genre", "subgenre", "bpm", "key", "duration", "year", "energy", "danceability", "created_at"];
const TEXT_SORT_FIELDS: [&str; 5] = ["title", "artist", "album", "genre", "subgenre"];

/// `apply_sort` after the default `ORDER BY tracks.created_at DESC`.
fn order_clause(sort: Option<&str>, order: &str) -> String {
    let Some(sort) = sort.filter(|sort| SORTABLE_FIELDS.contains(sort)) else {
        return " ORDER BY tracks.created_at DESC, tracks.id ASC".into();
    };
    let descending = plumdeck_core::compat::text::py_lower(order) == "desc";
    let value = if sort == "key" {
        let whens: String = camelot_order()
            .iter()
            .map(|(spelling, position)| format!(" WHEN '{spelling}' THEN {position}"))
            .collect();
        format!("CASE lower(trim(tracks.key)){whens} END")
    } else if TEXT_SORT_FIELDS.contains(&sort) {
        format!("nullif(lower(coalesce(tracks.{sort}, '')), '')")
    } else {
        format!("tracks.{sort}")
    };
    let direction = if descending { "DESC" } else { "ASC" };
    format!(" ORDER BY CASE WHEN ({value} IS NULL) THEN 1 ELSE 0 END ASC, {value} {direction}, tracks.id ASC")
}

fn lyrics_join(filters: &TrackFilters) -> &'static str {
    if filters.lyrics.as_deref().is_some_and(|lyrics| !lyrics.is_empty()) {
        " LEFT OUTER JOIN lyrics ON tracks.id = lyrics.track_id"
    } else {
        ""
    }
}

/// `search_tracks`: one page of tracks with their lyrics and analyzed flag.
pub fn search_tracks(
    conn: &Connection,
    filters: &TrackFilters,
    sort: Option<&str>,
    order: &str,
    limit: BigInt,
    offset: BigInt,
) -> Result<Vec<TrackHit>, DbError> {
    let mut sql = Sql::default();
    sql.push(&format!(
        "SELECT {TRACK_COLUMNS}, lyrics.content, {COMPLETED_ANALYSIS_SQL} \
         FROM tracks LEFT OUTER JOIN lyrics ON tracks.id = lyrics.track_id"
    ));
    let conditions = where_clause(filters);
    sql.push(&conditions.text);
    sql.params.extend(conditions.params);
    sql.push(&order_clause(sort, order));
    sql.push(" LIMIT ");
    sql.bind(limit.value());
    sql.push(" OFFSET ");
    sql.bind(offset.value());
    let mut statement = conn.prepare(&sql.text)?;
    let hits = statement
        .query_map(params_from_iter(sql.params), |row| {
            Ok(TrackHit {
                track: TrackRow::from_row(row, 0)?,
                lyrics: row.get(TRACK_COLUMN_COUNT)?,
                is_analyzed: row.get(TRACK_COLUMN_COUNT + 1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(hits)
}

/// The exact total of `search_tracks_page` (`count()` over the id query).
pub fn count_tracks(conn: &Connection, filters: &TrackFilters) -> Result<i64, DbError> {
    let mut sql = Sql::default();
    sql.push("SELECT count(*) AS count_1 FROM (SELECT tracks.id AS id FROM tracks");
    sql.push(lyrics_join(filters));
    let conditions = where_clause(filters);
    sql.push(&conditions.text);
    sql.params.extend(conditions.params);
    sql.push(") AS anon_1");
    Ok(conn.query_row(&sql.text, params_from_iter(sql.params), |row| row.get(0))?)
}

/// `search_track_ids`: matching ids, newest first (ties undefined).
pub fn search_track_ids(conn: &Connection, filters: &TrackFilters) -> Result<Vec<i64>, DbError> {
    let mut sql = Sql::default();
    sql.push("SELECT tracks.id FROM tracks");
    sql.push(lyrics_join(filters));
    let conditions = where_clause(filters);
    sql.push(&conditions.text);
    sql.params.extend(conditions.params);
    sql.push(" ORDER BY tracks.created_at DESC");
    let mut statement = conn.prepare(&sql.text)?;
    let ids = statement.query_map(params_from_iter(sql.params), |row| row.get(0))?.collect::<Result<_, _>>()?;
    Ok(ids)
}

/// `session.get(Track, track_id)`.
pub fn get_track(conn: &Connection, track_id: BigInt) -> Result<Option<TrackRow>, DbError> {
    let sql = format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE tracks.id = ?");
    Ok(conn.query_row(&sql, [track_id.value()], |row| TrackRow::from_row(row, 0)).optional()?)
}

/// `select(Track).where(Track.id.in_(track_ids))`, in table order.
pub fn tracks_by_ids(conn: &Connection, track_ids: &[i64]) -> Result<Vec<TrackRow>, DbError> {
    if track_ids.is_empty() {
        // SQLAlchemy's empty IN: no rows.
        return Ok(Vec::new());
    }
    let mut sql = Sql::default();
    sql.push(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE tracks.id IN "));
    sql.placeholders(track_ids.iter().map(|id| Value::BigInt(*id)));
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement.query_map(params_from_iter(sql.params), |row| TrackRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Why `similar_tracks` found nothing to rank.
#[derive(Debug)]
pub enum SimilarError {
    /// No embedding row for the track (Python's ValueError, a 404).
    NoEmbedding,
    Db(DbError),
}

impl From<DbError> for SimilarError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl From<duckdb::Error> for SimilarError {
    fn from(error: duckdb::Error) -> Self {
        Self::Db(error.into())
    }
}

/// `TrackRepository.get_similar_tracks`: the tracks in the same embedding
/// space, by DuckDB's `array_cosine_similarity` to the track's vector.
pub fn similar_tracks(conn: &Connection, track_id: BigInt, limit: BigInt) -> Result<Vec<TrackHit>, SimilarError> {
    use super::recommendations::EMBEDDING_DIM;
    use plumdeck_domain::mixability::{embedding_space, LEGACY_MODELS};
    let target: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT track_embeddings.model_name, track_embeddings.embedding_json FROM track_embeddings WHERE track_embeddings.track_id = ? LIMIT 1",
            [track_id.value()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((model, vector)) = target else { return Err(SimilarError::NoEmbedding) };
    let mut sql = Sql::default();
    sql.push(&format!(
        "SELECT {TRACK_COLUMNS}, lyrics.content, {COMPLETED_ANALYSIS_SQL} \
         FROM tracks JOIN track_embeddings ON tracks.id = track_embeddings.track_id \
         LEFT OUTER JOIN lyrics ON tracks.id = lyrics.track_id WHERE tracks.id != "
    ));
    sql.bind(track_id.value());
    if embedding_space(model.as_deref()) == "musicnn" {
        sql.push(" AND (track_embeddings.model_name IN ");
        sql.placeholders(LEGACY_MODELS.iter().map(|name| Value::Text((*name).into())));
        sql.push(" OR track_embeddings.model_name IS NULL)");
    } else {
        sql.push(" AND track_embeddings.model_name = ");
        sql.bind(model.map_or(Value::Null, Value::Text));
    }
    sql.push(&format!(
        " ORDER BY array_cosine_similarity(CAST(track_embeddings.embedding_json AS FLOAT[{EMBEDDING_DIM}]), CAST("
    ));
    sql.bind(vector.map_or(Value::Null, Value::Text));
    sql.push(&format!(" AS FLOAT[{EMBEDDING_DIM}])) DESC LIMIT "));
    sql.bind(limit.value());
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement
        .query_map(params_from_iter(sql.params), |row| {
            Ok(TrackHit { track: TrackRow::from_row(row, 0)?, lyrics: row.get(TRACK_COLUMN_COUNT)?, is_analyzed: row.get(TRACK_COLUMN_COUNT + 1)? })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// The metadata fields `PATCH /api/tracks/{id}/info` may set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
}

/// `update_track_info`: set the given fields and commit; the refreshed
/// row, or `None` when the track does not exist.
pub fn update_track_info(conn: &Connection, track_id: BigInt, info: &TrackInfo) -> Result<Option<TrackRow>, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        if get_track(conn, track_id)?.is_none() {
            return Ok(None);
        }
        let mut sets = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        for (column, value) in [("title", &info.title), ("artist", &info.artist), ("album", &info.album)] {
            if let Some(value) = value {
                sets.push(format!("{column}=?"));
                values.push(text(value));
            }
        }
        if let Some(year) = info.year {
            sets.push("year=?".into());
            values.push(Value::BigInt(year));
        }
        if !sets.is_empty() {
            values.push(track_id.value());
            conn.execute(&format!("UPDATE tracks SET {} WHERE tracks.id = ?", sets.join(", ")), params_from_iter(values))?;
        }
        get_track(conn, track_id)
    })();
    finish(conn, result)
}

/// `TrackRepository.update_genre(track_id, genre, verified=True)`.
pub fn update_genre(conn: &Connection, track_id: BigInt, genre: &str) -> Result<Option<TrackRow>, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        if get_track(conn, track_id)?.is_none() {
            return Ok(None);
        }
        conn.execute(
            "UPDATE tracks SET genre=?, is_genre_verified=? WHERE tracks.id = ?",
            params![genre, true, track_id.value()],
        )?;
        get_track(conn, track_id)
    })();
    finish(conn, result)
}

/// A `track_analyses` row as the metadata route reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackAnalysisRow {
    pub beats_f32: Option<Vec<u8>>,
    pub waveform_u8: Option<Vec<u8>>,
    pub features_extra_json: Option<String>,
}

/// `session.get(TrackAnalysis, track_id)`.
pub fn get_analysis(conn: &Connection, track_id: i64) -> Result<Option<TrackAnalysisRow>, DbError> {
    Ok(conn
        .query_row(
            "SELECT beats_f32, waveform_u8, features_extra_json FROM track_analyses WHERE track_analyses.track_id = ?",
            [track_id],
            |row| Ok(TrackAnalysisRow { beats_f32: row.get(0)?, waveform_u8: row.get(1)?, features_extra_json: row.get(2)? }),
        )
        .optional()?)
}

fn truthy(value: &PyValue) -> bool {
    match value {
        PyValue::None => false,
        PyValue::Bool(flag) => *flag,
        PyValue::Int(number) => *number != 0,
        PyValue::Float(number) => *number != 0.0,
        PyValue::Str(text) => !text.is_empty(),
        PyValue::List(items) => !items.is_empty(),
        PyValue::Dict(pairs) => !pairs.is_empty(),
        #[allow(unreachable_patterns)]
        _ => true,
    }
}

/// `_components(analysis.features_extra)`: the `analysis_components` dict
/// of the row's JSON (`{}` when the JSON is unreadable or not a dict).
fn component(analysis: &TrackAnalysisRow, name: &str) -> Option<PyValue> {
    let extra = analysis.features_extra_json.as_deref().and_then(|json| loads(json).ok())?;
    let PyValue::Dict(pairs) = extra else { return None };
    let dict_get = |pairs: &[(PyValue, PyValue)], key: &str| {
        pairs.iter().rev().find(|(k, _)| matches!(k, PyValue::Str(s) if s == key)).map(|(_, v)| v.clone())
    };
    let PyValue::Dict(components) = dict_get(&pairs, "analysis_components")? else { return None };
    dict_get(&components, name)
}

fn is_unit(value: Option<f64>) -> bool {
    value.is_some_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
}

/// `has_completed_analysis(track, analysis=analysis)` in Python (the
/// metadata route's `analyzed`).
pub fn has_completed_analysis(track: &TrackRow, analysis: Option<&TrackAnalysisRow>) -> bool {
    let positive = |value: Option<f64>| value.is_some_and(|value| value.is_finite() && value > 0.0);
    if !positive(track.bpm) || !positive(track.duration) {
        return false;
    }
    let timbre = [track.energy, track.danceability, track.brightness, track.noisiness];
    if !timbre.iter().all(|value| is_unit(*value)) {
        return false;
    }
    let zeros = timbre.iter().filter(|value| **value == Some(0.0)).count();
    let measured = || analysis.and_then(|a| component(a, "timbre")).is_some_and(|value| truthy(&value));
    if zeros >= 3 && !measured() {
        return false;
    }
    let Some(analysis) = analysis else { return false };
    if !analysis.waveform_u8.as_ref().is_some_and(|blob| !blob.is_empty()) {
        return false;
    }
    if track.analysis_level.as_deref() != Some("light") {
        return true;
    }
    let rhythm = component(analysis, "rhythm");
    let current = rhythm.as_ref().is_some_and(|value| {
        truthy(value) && !matches!(value, PyValue::Str(text) if EXCERPT_LIGHT_RHYTHM.contains(&text.as_str()))
    });
    current && analysis.beats_f32.as_ref().is_some_and(|blob| !blob.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, title, artist, genre, key, bpm, duration, energy, danceability, brightness, noisiness, year, created_at) VALUES
               ('/m/a.mp3', 'Alpha', 'DJ A', 'House', 'F minor', 124, 300, 0.5, 0.5, 0.5, 0.5, 1999, '2026-01-01 00:00:00'),
               ('/m/b.mp3', 'Beta', 'DJ B', 'Techno', '4A', 62, 200, 0, 0, 0, 0.5, NULL, '2026-01-02 00:00:00'),
               ('/m/c.mp3', 'Gamma 100%', NULL, 'House', 'C major', 130, 100, 0.5, 0.5, 0.5, 0.5, 0, '2026-01-02 00:00:00');
             INSERT INTO track_analyses (track_id, waveform_u8, features_extra_json) VALUES (1, '\\x01'::BLOB, '{}'), (2, '\\x01'::BLOB, '{\"analysis_components\": {\"timbre\": \"v1\"}}');
             INSERT INTO lyrics (track_id, content) VALUES (1, 'la la'), (3, '  ');",
        )
        .unwrap();
        conn
    }

    fn ids(hits: &[TrackHit]) -> Vec<i64> {
        hits.iter().map(|hit| hit.track.id).collect()
    }

    #[test]
    fn search_filters_sorts_and_counts() {
        let conn = library();
        let all = TrackFilters::default();
        let hits = search_tracks(&conn, &all, None, "asc", BigInt(100), BigInt(0)).unwrap();
        assert_eq!(ids(&hits), [2, 3, 1]);
        assert_eq!(hits[2].lyrics.as_deref(), Some("la la"));
        assert_eq!(hits.iter().map(|h| h.is_analyzed).collect::<Vec<_>>(), [true, false, true]);
        let minor = TrackFilters { key: Some("Fm".into()), ..TrackFilters::default() };
        assert_eq!(ids(&search_tracks(&conn, &minor, None, "asc", BigInt(100), BigInt(0)).unwrap()), [2, 1]);
        let window = TrackFilters { bpm: Some(124.0), bpm_range: 1.0, ..TrackFilters::default() };
        assert_eq!(count_tracks(&conn, &window).unwrap(), 2);
        let words = TrackFilters { q: Some("alpha mp3 dj".into()), ..TrackFilters::default() };
        assert_eq!(search_track_ids(&conn, &words).unwrap(), [1]);
        let lyric = TrackFilters { lyrics: Some("la".into()), lyrics_status: "set".into(), ..TrackFilters::default() };
        assert_eq!(search_track_ids(&conn, &lyric).unwrap(), [1]);
        let unset = TrackFilters { year_status: "unset".into(), lyrics_status: "unset".into(), ..TrackFilters::default() };
        assert_eq!(count_tracks(&conn, &unset).unwrap(), 2);
        let by_key = search_tracks(&conn, &all, Some("key"), "desc", BigInt(10), BigInt(0)).unwrap();
        assert_eq!(ids(&by_key), [3, 1, 2]);
        let by_title = search_tracks(&conn, &all, Some("title"), "asc", BigInt(1), BigInt(1)).unwrap();
        assert_eq!(ids(&by_title), [2]);
        let unanalyzed = TrackFilters { status: "unanalyzed".into(), ..TrackFilters::default() };
        assert_eq!(search_track_ids(&conn, &unanalyzed).unwrap(), [3]);
    }

    #[test]
    fn updates_commit_and_refresh() {
        let conn = library();
        let info = TrackInfo { title: Some("New".into()), year: Some(2001), ..TrackInfo::default() };
        let row = update_track_info(&conn, BigInt(1), &info).unwrap().unwrap();
        assert_eq!((row.title.as_deref(), row.year, row.artist.as_deref()), (Some("New"), Some(2001), Some("DJ A")));
        assert_eq!(update_track_info(&conn, BigInt(99), &info).unwrap(), None);
        let row = update_genre(&conn, BigInt(2), "Ambient").unwrap().unwrap();
        assert_eq!((row.genre.as_deref(), row.is_genre_verified), (Some("Ambient"), Some(true)));
        assert_eq!(get_track(&conn, BigInt(1i128 << 70)).unwrap(), None);
    }

    #[test]
    fn analyzed_rule_in_python_matches_sql() {
        let conn = library();
        for id in 1..=3 {
            let track = get_track(&conn, BigInt(id)).unwrap().unwrap();
            let analysis = get_analysis(&conn, id as i64).unwrap();
            let sql: bool = conn
                .query_row(&format!("SELECT {COMPLETED_ANALYSIS_SQL} FROM tracks WHERE id = ?"), [id as i64], |row| row.get(0))
                .unwrap();
            assert_eq!(has_completed_analysis(&track, analysis.as_ref()), sql, "track {id}");
        }
    }
}

/// The metadata job's track list (`_run_update`): optionally only `ids`,
/// without the skip cache's ids, and unless `overwrite` only tracks missing
/// the year (`release_date`) or lyrics (`lyrics`).
pub fn metadata_job_tracks(
    conn: &Connection,
    kind: &str,
    overwrite: bool,
    ids: Option<&[i128]>,
    skip: &[i64],
) -> Result<Vec<TrackRow>, DbError> {
    let mut sql = Sql::default();
    sql.push(&format!("SELECT {TRACK_COLUMNS} FROM tracks"));
    if !overwrite && kind == "lyrics" {
        sql.push(" LEFT OUTER JOIN lyrics ON tracks.id = lyrics.track_id");
    }
    sql.push(" WHERE 1=1");
    if let Some(ids) = ids {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sql.push(" AND tracks.id IN ");
        sql.placeholders(ids.iter().map(|id| BigInt(*id).value()));
    }
    if !overwrite {
        if !skip.is_empty() {
            sql.push(" AND tracks.id NOT IN ");
            sql.placeholders(skip.iter().map(|id| Value::BigInt(*id)));
        }
        match kind {
            "release_date" => sql.push(" AND (tracks.year IS NULL OR tracks.year = 0)"),
            "lyrics" => sql.push(" AND (lyrics.track_id IS NULL OR length(trim(lyrics.content)) = 0)"),
            _ => {}
        }
    }
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement.query_map(params_from_iter(sql.params), |row| TrackRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Set one track's year.
pub fn set_year(conn: &Connection, track_id: i64, year: i64) -> Result<(), DbError> {
    conn.execute("UPDATE tracks SET year=? WHERE tracks.id = ?", params![year, track_id])?;
    Ok(())
}

/// `find_all(limit=...)`: the first tracks in table order.
pub fn first_tracks(conn: &Connection, limit: i64) -> Result<Vec<TrackRow>, DbError> {
    let mut statement = conn.prepare(&format!("SELECT {TRACK_COLUMNS} FROM tracks LIMIT ? OFFSET 0"))?;
    let rows = statement.query_map([limit], |row| TrackRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}
