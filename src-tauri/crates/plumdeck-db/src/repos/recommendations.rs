//! The recommenders' queries (backend/infra/repositories/recommendation_repository.py, S4):
//! the candidate pool, the globally ranked next-track page and the
//! embedding reads behind the genre suggestions.
//!
//! The SQL is Python's text with its named parameters bound in order of
//! appearance, so DuckDB evaluates the same expressions (tempo distance,
//! FLOAT[200] cosine, ordering). A tuple parameter (`IN :genres`) is a LIST
//! in DuckDB's Python binding; here it is the same list written inline.

use duckdb::types::{TimeUnit, Value, ValueRef};
use duckdb::{params_from_iter, Connection, OptionalExt};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_domain::mixability::{embedding_space, parse_embedding, LEGACY_MODELS};
use plumdeck_domain::targets::Targets;

use super::tracks::Sql;
use crate::DbError;

/// `EMBEDDING_DIM`.
pub const EMBEDDING_DIM: usize = 200;

/// `DEFAULT_MIXABILITY_WEIGHTS` as the ranked page binds them.
const BPM_WEIGHT: f64 = 0.35;
const VECTOR_WEIGHT: f64 = 0.4;

/// One pool row: what `fetch_candidates_pool` selects.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolRow {
    pub id: i64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub duration: Option<f64>,
    pub album: Option<String>,
    pub filepath: String,
    pub year: Option<i64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub loudness: Option<f64>,
    pub contrast: Option<f64>,
    pub noisiness: Option<f64>,
    pub vector: Option<Vec<f64>>,
    pub embedding_model: Option<String>,
    pub has_lyrics: bool,
}

/// `fetch_candidates_pool`'s filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolQuery<'a> {
    pub targets: Targets,
    pub genres: Option<&'a [String]>,
    pub subgenres: Option<&'a [String]>,
    pub limit: i64,
    pub exclude_ids: &'a [i64],
    pub candidate_ids: Option<&'a [i64]>,
}

fn list(sql: &mut Sql, values: &[String]) {
    sql.push("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            sql.push(", ");
        }
        sql.bind(Value::Text(value.clone()));
    }
    sql.push("]");
}

/// `int(x)` bound the way DuckDB's Python binding binds a Python int.
fn int_param(value: i64) -> Value {
    match i32::try_from(value) {
        Ok(small) => Value::Int(small),
        Err(_) => Value::BigInt(value),
    }
}

fn tempo_distance(sql: &mut Sql, target: f64) {
    sql.push("CASE WHEN t.bpm > 0 AND isfinite(t.bpm) THEN LEAST(\n                ABS(LOG2(t.bpm / ");
    sql.bind(Value::Double(target));
    sql.push(")),\n                ABS(LOG2(t.bpm / ");
    sql.bind(Value::Double(target));
    sql.push(") - 1),\n                ABS(LOG2(t.bpm / ");
    sql.bind(Value::Double(target));
    sql.push(") + 1)\n            ) ELSE 1000 END");
}

fn ids_text(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

fn genre_filter(sql: &mut Sql, genres: Option<&[String]>, subgenres: Option<&[String]>) {
    let genres = genres.filter(|values| !values.is_empty());
    let subgenres = subgenres.filter(|values| !values.is_empty());
    if genres.is_none() && subgenres.is_none() {
        return;
    }
    sql.push(" AND (");
    if let Some(genres) = genres {
        sql.push("t.genre IN ");
        list(sql, genres);
    }
    if let Some(subgenres) = subgenres {
        if genres.is_some() {
            sql.push(" OR ");
        }
        sql.push("t.subgenre IN ");
        list(sql, subgenres);
    }
    sql.push(")");
}

/// `fetch_candidates_pool`.
pub fn candidate_pool(conn: &Connection, query: &PoolQuery<'_>) -> Result<Vec<PoolRow>, DbError> {
    let mut sql = Sql::default();
    sql.push(
        "SELECT t.id, t.title, t.artist, t.bpm, t.key, t.genre, t.subgenre, t.duration, t.album, t.filepath, t.year, \
         t.energy, t.danceability, t.brightness, t.loudness, t.contrast, t.noisiness, te.embedding_json, te.model_name, \
         (l.content IS NOT NULL AND length(trim(l.content)) > 0) as db_has_lyrics \
         FROM tracks t LEFT JOIN track_embeddings te ON t.id = te.track_id LEFT JOIN lyrics l ON t.id = l.track_id WHERE 1=1",
    );
    if let Some(candidates) = query.candidate_ids {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        sql.push(" AND t.id IN (SELECT CAST(unnest(string_split(");
        sql.bind(Value::Text(ids_text(candidates)));
        sql.push(", ',')) AS BIGINT))");
    }
    if !query.exclude_ids.is_empty() {
        sql.push(" AND t.id NOT IN (SELECT CAST(unnest(string_split(");
        sql.bind(Value::Text(ids_text(query.exclude_ids)));
        sql.push(", ',')) AS BIGINT))");
    }
    genre_filter(&mut sql, query.genres, query.subgenres);
    let targets = &query.targets;
    let mut order: Vec<Sql> = Vec::new();
    if let Some(bpm) = targets.bpm.filter(|bpm| bpm.is_finite() && *bpm > 0.0) {
        let max = targets.max_tempo_distance.filter(|max| max.is_finite() && *max > 0.0).unwrap_or(1.4f64.log2());
        sql.push(" AND ((");
        tempo_distance(&mut sql, bpm);
        sql.push(") <= ");
        sql.bind(Value::Double(max));
        sql.push(" OR t.bpm = 0 OR t.bpm IS NULL)");
        let mut term = Sql::default();
        tempo_distance(&mut term, bpm);
        order.push(term);
    }
    for (value, operator) in [(targets.year_min, ">="), (targets.year_max, "<=")] {
        if let Some(year) = value.filter(|year| year.is_finite()) {
            sql.push(&format!(" AND t.year {operator} "));
            sql.bind(int_param(year as i64));
        }
    }
    if let Some(energy) = targets.energy.filter(|energy| energy.is_finite()) {
        sql.push(" AND t.energy BETWEEN ");
        sql.bind(Value::Double((energy - 0.4).max(0.0)));
        sql.push(" AND ");
        sql.bind(Value::Double((energy + 0.4).min(1.0)));
        let mut term = Sql::default();
        term.push("ABS(t.energy - ");
        term.bind(Value::Double(energy));
        term.push(")");
        order.push(term);
    }
    for (feature, value) in [("danceability", targets.danceability), ("brightness", targets.brightness), ("noisiness", targets.noisiness)] {
        if let Some(target) = value.filter(|target| target.is_finite()) {
            let mut term = Sql::default();
            term.push(&format!("COALESCE(ABS(t.{feature} - "));
            term.bind(Value::Double(target));
            term.push("), 1.0)");
            order.push(term);
        }
    }
    if order.is_empty() {
        sql.push(" ORDER BY t.created_at DESC, t.id ASC");
    } else {
        sql.push(" ORDER BY (");
        for (index, term) in order.into_iter().enumerate() {
            if index > 0 {
                sql.push(" + ");
            }
            sql.push(&term.text);
            sql.params.extend(term.params);
        }
        sql.push(") ASC, t.id ASC");
    }
    sql.push(" LIMIT ");
    sql.bind(int_param(query.limit));
    let float = |row: &duckdb::Row<'_>, index: usize| -> duckdb::Result<Option<f64>> { Ok(row.get::<_, Option<f32>>(index)?.map(f64::from)) };
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement
        .query_map(params_from_iter(sql.params), |row| {
            let embedding: Option<String> = row.get(17)?;
            Ok(PoolRow {
                id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                bpm: float(row, 3)?,
                key: row.get(4)?,
                genre: row.get(5)?,
                subgenre: row.get(6)?,
                duration: float(row, 7)?,
                album: row.get(8)?,
                filepath: row.get(9)?,
                year: row.get(10)?,
                energy: float(row, 11)?,
                danceability: float(row, 12)?,
                brightness: float(row, 13)?,
                loudness: float(row, 14)?,
                contrast: float(row, 15)?,
                noisiness: float(row, 16)?,
                vector: parse_embedding(embedding.as_deref()),
                embedding_model: row.get(18)?,
                has_lyrics: row.get::<_, Option<bool>>(19)?.unwrap_or(false),
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `session.get(TrackEmbedding, track_id)`: `(model_name, embedding_json)`.
pub fn embedding(conn: &Connection, track_id: i64) -> Result<Option<(Option<String>, Option<String>)>, DbError> {
    Ok(conn
        .query_row(
            "SELECT track_embeddings.model_name, track_embeddings.embedding_json FROM track_embeddings WHERE track_embeddings.track_id = ?",
            [track_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// A result column as the Python value `dict(row._mapping)` holds.
fn py_value(value: ValueRef<'_>) -> PyValue {
    match value {
        ValueRef::Null => PyValue::None,
        ValueRef::Boolean(flag) => PyValue::Bool(flag),
        ValueRef::TinyInt(n) => PyValue::Int(n.into()),
        ValueRef::SmallInt(n) => PyValue::Int(n.into()),
        ValueRef::Int(n) => PyValue::Int(n.into()),
        ValueRef::BigInt(n) => PyValue::Int(n.into()),
        ValueRef::HugeInt(n) => PyValue::Int(n),
        ValueRef::UTinyInt(n) => PyValue::Int(n.into()),
        ValueRef::USmallInt(n) => PyValue::Int(n.into()),
        ValueRef::UInt(n) => PyValue::Int(n.into()),
        ValueRef::UBigInt(n) => PyValue::Int(n.into()),
        ValueRef::Float(x) => PyValue::Float(x.into()),
        ValueRef::Double(x) => PyValue::Float(x),
        ValueRef::Text(bytes) => PyValue::Str(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Timestamp(unit, raw) => {
            let micros = match unit {
                TimeUnit::Second => raw * 1_000_000,
                TimeUnit::Millisecond => raw * 1_000,
                TimeUnit::Microsecond => raw,
                TimeUnit::Nanosecond => raw / 1_000,
            };
            chrono::DateTime::from_timestamp_micros(micros).map_or(PyValue::None, |at| PyValue::DateTime(at.naive_utc()))
        }
        other => PyValue::Str(format!("{other:?}")),
    }
}

/// `fetch_ranked_page`'s answer: rows as dicts (`t.*` plus `lyrics`,
/// `has_lyrics` and `recommendation_score`) and the exact total.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedPage {
    pub items: Vec<Vec<(String, PyValue)>>,
    pub total: i64,
}

/// The target track's facts the ranked page reads.
#[derive(Clone, Debug, PartialEq)]
pub struct RankTarget {
    pub id: i64,
    pub bpm: Option<f64>,
}

/// `fetch_ranked_page`: every eligible candidate scored in SQL, then paged.
pub fn ranked_page(
    conn: &Connection,
    target: &RankTarget,
    targets: &Targets,
    genres: Option<&[String]>,
    subgenres: Option<&[String]>,
    limit: i64,
    offset: i64,
) -> Result<RankedPage, DbError> {
    let mut filter = Sql::default();
    filter.push("t.id <> ");
    filter.bind(int_param(target.id));
    let mut genre_sql = Sql::default();
    genre_filter(&mut genre_sql, genres, subgenres);
    if !genre_sql.text.is_empty() {
        // genre_filter writes " AND (...)".
        filter.push(&genre_sql.text);
        filter.params.extend(genre_sql.params);
    }
    let mut order_terms: Vec<Sql> = Vec::new();
    let filter_bpm = targets.bpm.filter(|bpm| bpm.is_finite() && *bpm > 0.0).or(target.bpm.filter(|bpm| bpm.is_finite()));
    if let Some(bpm) = filter_bpm.filter(|bpm| *bpm > 0.0) {
        filter.push(" AND ((t.bpm > 0 AND isfinite(t.bpm) AND (");
        tempo_distance(&mut filter, bpm);
        filter.push(") <= ");
        filter.bind(Value::Double(1.4f64.log2()));
        filter.push(") OR t.bpm = 0 OR t.bpm IS NULL)");
        let mut term = Sql::default();
        tempo_distance(&mut term, bpm);
        order_terms.push(term);
    }
    let score_bpm = target.bpm.filter(|bpm| bpm.is_finite() && *bpm > 0.0);
    for (value, operator) in [(targets.year_min, ">="), (targets.year_max, "<=")] {
        if let Some(year) = value.filter(|year| year.is_finite()) {
            filter.push(&format!(" AND t.year {operator} "));
            filter.bind(int_param(year as i64));
        }
    }
    if let Some(energy) = targets.energy.filter(|energy| energy.is_finite()) {
        filter.push(" AND t.energy BETWEEN ");
        filter.bind(Value::Double((energy - 0.4).max(0.0)));
        filter.push(" AND ");
        filter.bind(Value::Double((energy + 0.4).min(1.0)));
        let mut term = Sql::default();
        term.push("ABS(t.energy - ");
        term.bind(Value::Double(energy));
        term.push(")");
        order_terms.push(term);
    }
    for (feature, value) in [("danceability", targets.danceability), ("brightness", targets.brightness), ("noisiness", targets.noisiness)] {
        if let Some(goal) = value.filter(|goal| goal.is_finite()) {
            let mut term = Sql::default();
            term.push(&format!("COALESCE(ABS(t.{feature} - "));
            term.bind(Value::Double(goal));
            term.push("), 1.0)");
            order_terms.push(term);
        }
    }

    // The score: tempo closeness and the stored-vector cosine, plus the bonus.
    let bpm_score = |sql: &mut Sql| match score_bpm {
        Some(bpm) => {
            sql.push("EXP(-0.5 * POWER((");
            tempo_distance(sql, bpm);
            sql.push(") / ");
            sql.bind(Value::Double(1.08f64.log2()));
            sql.push(", 2))");
        }
        None => sql.push("0"),
    };
    let stored = embedding(conn, target.id)?;
    let usable = stored.as_ref().and_then(|(model, json)| {
        let vector = parse_embedding(json.as_deref())?;
        let norm = vector.iter().map(|x| x * x).sum::<f64>().sqrt();
        (vector.len() == EMBEDDING_DIM && norm > 0.0).then(|| (model.clone(), json.clone().unwrap_or_default()))
    });
    let vector_score = |sql: &mut Sql| match &usable {
        None => sql.push("NULL"),
        Some((model, json)) => {
            let model_filter = |sql: &mut Sql| {
                if embedding_space(model.as_deref()) == "musicnn" {
                    sql.push("(te.model_name IN ");
                    list(sql, &LEGACY_MODELS.iter().map(|name| name.to_string()).collect::<Vec<_>>());
                    sql.push(" OR te.model_name IS NULL)");
                } else {
                    sql.push("te.model_name = ");
                    sql.bind(model.clone().map_or(Value::Null, Value::Text));
                }
            };
            let candidate = format!("TRY_CAST(te.embedding_json AS FLOAT[{EMBEDDING_DIM}])");
            let target_array = |sql: &mut Sql| {
                sql.push("CAST(");
                sql.bind(Value::Text(json.clone()));
                sql.push(&format!(" AS FLOAT[{EMBEDDING_DIM}])"));
            };
            let candidate_norm = format!("TRY(array_inner_product({candidate}, {candidate}))");
            let target_norm = |sql: &mut Sql| {
                sql.push("TRY(array_inner_product(");
                target_array(sql);
                sql.push(", ");
                target_array(sql);
                sql.push("))");
            };
            let similarity = |sql: &mut Sql| {
                sql.push("CASE WHEN ");
                model_filter(sql);
                sql.push(&format!(" AND isfinite({candidate_norm}) AND ({candidate_norm}) > 0 AND isfinite("));
                target_norm(sql);
                sql.push(") AND (");
                target_norm(sql);
                sql.push(&format!(") > 0 THEN TRY(array_cosine_similarity({candidate}, "));
                target_array(sql);
                sql.push(")) ELSE NULL END");
            };
            sql.push("CASE WHEN isfinite(");
            similarity(sql);
            sql.push(") THEN GREATEST(0.0, LEAST(1.0, ");
            similarity(sql);
            sql.push(")) ELSE NULL END");
        }
    };
    let mut score = Sql::default();
    score.push("(((");
    bpm_score(&mut score);
    score.push(") * ");
    score.bind(Value::Double(BPM_WEIGHT));
    score.push(" + COALESCE((");
    vector_score(&mut score);
    score.push("), 0.0) * ");
    score.bind(Value::Double(VECTOR_WEIGHT));
    score.push(") / (");
    score.bind(Value::Double(BPM_WEIGHT));
    score.push(" + CASE WHEN (");
    vector_score(&mut score);
    score.push(") IS NOT NULL THEN ");
    score.bind(Value::Double(VECTOR_WEIGHT));
    score.push(" ELSE 0 END) + CASE WHEN EXISTS (SELECT 1 FROM wordplay_pairs wp WHERE wp.from_track_id=");
    score.bind(int_param(target.id));
    score.push(" AND wp.to_track_id=t.id AND wp.status='approved') THEN 0.25 ELSE 0 END)");

    let mut count = Sql::default();
    count.push("SELECT count(*) FROM tracks t WHERE ");
    count.push(&filter.text);
    count.params.extend(filter.params.clone());
    let total: i64 = conn.query_row(&count.text, params_from_iter(count.params), |row| row.get(0))?;

    let mut page = Sql::default();
    page.push("SELECT t.*, l.content AS lyrics, (l.content IS NOT NULL AND length(trim(l.content)) > 0) AS has_lyrics, ");
    page.push(&score.text);
    page.params.extend(score.params);
    page.push(" AS recommendation_score FROM tracks t LEFT JOIN track_embeddings te ON te.track_id=t.id \
               LEFT JOIN lyrics l ON l.track_id=t.id WHERE ");
    page.push(&filter.text);
    page.params.extend(filter.params);
    page.push(" ORDER BY recommendation_score DESC, ");
    if order_terms.is_empty() {
        page.push("t.created_at DESC, t.id ASC");
    } else {
        page.push("(");
        for (index, term) in order_terms.into_iter().enumerate() {
            if index > 0 {
                page.push(" + ");
            }
            page.push(&term.text);
            page.params.extend(term.params);
        }
        page.push(") ASC, t.id ASC");
    }
    page.push(" LIMIT ");
    page.bind(int_param(limit));
    page.push(" OFFSET ");
    page.bind(int_param(offset));
    let mut statement = conn.prepare(&page.text)?;
    let mut rows = statement.query(params_from_iter(page.params))?;
    let names: Vec<String> = rows.as_ref().expect("statement").column_names();
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let mut item = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            item.push((name.clone(), py_value(row.get_ref(index)?)));
        }
        items.push(item);
    }
    Ok(RankedPage { items, total })
}

/// The model condition of `_model_condition` for a target's space.
fn model_condition(sql: &mut Sql, model: Option<&str>) {
    if embedding_space(model) == "musicnn" {
        sql.push(" AND (track_embeddings.model_name IN ");
        list(sql, &LEGACY_MODELS.iter().map(|name| name.to_string()).collect::<Vec<_>>());
        sql.push(" OR track_embeddings.model_name IS NULL)");
    } else {
        sql.push(" AND track_embeddings.model_name = ");
        sql.bind(model.map_or(Value::Null, |name| Value::Text(name.into())));
    }
}

/// `get_verified_tracks_with_embeddings`: `(genre, vector)` of verified
/// tracks with a non-empty genre and a parseable embedding in the space of
/// `model` (when given), except `exclude` (when non-zero).
pub fn verified_genre_vectors(conn: &Connection, exclude: i64, model: Option<Option<&str>>) -> Result<Vec<(String, Vec<f64>)>, DbError> {
    let mut sql = Sql::default();
    sql.push(
        "SELECT tracks.genre, track_embeddings.embedding_json FROM tracks JOIN track_embeddings \
         ON tracks.id = track_embeddings.track_id WHERE tracks.is_genre_verified = true",
    );
    if let Some(model) = model {
        model_condition(&mut sql, model);
    }
    if exclude != 0 {
        sql.push(" AND tracks.id != ");
        sql.bind(Value::BigInt(exclude));
    }
    let mut statement = conn.prepare(&sql.text)?;
    let rows: Vec<(Option<String>, Option<String>)> =
        statement.query_map(params_from_iter(sql.params), |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(genre, json)| {
            let genre = genre.filter(|genre| !genre.is_empty())?;
            Some((genre, parse_embedding(json.as_deref())?))
        })
        .collect())
}

/// `get_parent_vectors`: `(id, vector, space)` of verified tracks.
pub fn parent_vectors(conn: &Connection) -> Result<Vec<(i64, Vec<f64>, String)>, DbError> {
    let mut statement = conn.prepare(
        "SELECT tracks.id, track_embeddings.embedding_json, track_embeddings.model_name FROM tracks JOIN track_embeddings \
         ON tracks.id = track_embeddings.track_id WHERE tracks.is_genre_verified = true",
    )?;
    let rows: Vec<(i64, Option<String>, Option<String>)> =
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, json, model)| Some((id, parse_embedding(json.as_deref())?, embedding_space(model.as_deref()).to_owned())))
        .collect())
}

/// `get_candidates_with_ids` (and, without the ids, `get_candidate_vectors`):
/// tracks still to classify (`mode == "subgenre"`: no subgenre; otherwise
/// unverified) with a parseable embedding in `model`'s space.
pub fn candidate_vectors(conn: &Connection, mode: &str, model: Option<&str>) -> Result<Vec<(i64, Vec<f64>)>, DbError> {
    let mut sql = Sql::default();
    sql.push(
        "SELECT tracks.id, track_embeddings.embedding_json FROM tracks JOIN track_embeddings \
         ON tracks.id = track_embeddings.track_id WHERE 1=1",
    );
    model_condition(&mut sql, model);
    if mode == "subgenre" {
        sql.push(" AND (tracks.subgenre IS NULL OR tracks.subgenre = '')");
    } else {
        sql.push(" AND tracks.is_genre_verified = false");
    }
    let mut statement = conn.prepare(&sql.text)?;
    let rows: Vec<(i64, Option<String>)> =
        statement.query_map(params_from_iter(sql.params), |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok(rows.into_iter().filter_map(|(id, json)| Some((id, parse_embedding(json.as_deref())?))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, title, genre, subgenre, bpm, energy, is_genre_verified, created_at) VALUES
               ('/1', 'A', 'House', 'Deep', 124, 0.5, true, '2026-01-01'),
               ('/2', 'B', 'House', '', 126, 0.6, false, '2026-01-02'),
               ('/3', 'C', 'Techno', '', 62, 0.9, false, '2026-01-03'),
               ('/4', 'D', 'Techno', '', 174.5, 0.1, false, '2026-01-04');
             INSERT INTO track_embeddings (track_id, model_name, embedding_json) VALUES
               (1, 'musicnn', '[1, 0]'), (2, NULL, '[1, 1]'), (3, 'other', '[0, 1]');
             INSERT INTO lyrics (track_id, content) VALUES (2, ' la ');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn pool_filters_tempo_and_genres() {
        let conn = library();
        let targets = Targets { bpm: Some(124.0), ..Targets::default() };
        let query = PoolQuery { targets, limit: 10, exclude_ids: &[1], ..PoolQuery::default() };
        let ids: Vec<i64> = candidate_pool(&conn, &query).unwrap().iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![3, 2], "62 is half of 124; 174.5 is just past log2(1.4)");
        let genres = vec!["House".to_string()];
        let query = PoolQuery { genres: Some(&genres), limit: 10, ..PoolQuery::default() };
        let rows = candidate_pool(&conn, &query).unwrap();
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2, 1]);
        assert!(rows[0].has_lyrics && rows[0].vector == Some(vec![1.0, 1.0]));
        let none = PoolQuery { candidate_ids: Some(&[]), limit: 10, ..PoolQuery::default() };
        assert!(candidate_pool(&conn, &none).unwrap().is_empty());
    }

    #[test]
    fn ranked_page_counts_every_candidate() {
        let conn = library();
        let page = ranked_page(&conn, &RankTarget { id: 1, bpm: Some(124.0) }, &Targets::default(), None, None, 1, 0).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 1);
        let keys: Vec<&str> = page.items[0].iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys.first(), Some(&"id"));
        assert_eq!(&keys[keys.len() - 3..], &["lyrics", "has_lyrics", "recommendation_score"]);
    }

    #[test]
    fn embedding_reads_by_space() {
        let conn = library();
        assert_eq!(verified_genre_vectors(&conn, 0, Some(None)).unwrap(), vec![("House".into(), vec![1.0, 0.0])]);
        assert!(verified_genre_vectors(&conn, 1, None).unwrap().is_empty());
        assert_eq!(candidate_vectors(&conn, "genre", None).unwrap(), vec![(2, vec![1.0, 1.0])]);
        assert_eq!(candidate_vectors(&conn, "genre", Some("other")).unwrap(), vec![(3, vec![0.0, 1.0])]);
        assert_eq!(parent_vectors(&conn).unwrap(), vec![(1, vec![1.0, 0.0], "musicnn".into())]);
    }
}
