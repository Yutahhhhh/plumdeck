//! The `wordplay_pairs` table (backend/domain/models/wordplay.py and the
//! queries of backend/app/services/wordplay_app_service.py, S4).

use chrono::NaiveDateTime;
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection, OptionalExt, Row};

use super::tracks::{BigInt, Sql};
use crate::DbError;

/// The `WordplayPair` model's columns, in model order.
pub const PAIR_COLUMNS: &str = "wordplay_pairs.id, wordplay_pairs.from_track_id, wordplay_pairs.to_track_id, \
wordplay_pairs.keyword, wordplay_pairs.normalized_keyword, wordplay_pairs.source_phrase, wordplay_pairs.target_phrase, \
wordplay_pairs.source_section_position, wordplay_pairs.target_section_position, wordplay_pairs.source_cue_mode, \
wordplay_pairs.from_timestamp, wordplay_pairs.source_cue_end_timestamp, wordplay_pairs.to_timestamp, \
wordplay_pairs.target_intro_timestamp, wordplay_pairs.target_landing_timestamp, wordplay_pairs.transition_notes, \
wordplay_pairs.source_url, wordplay_pairs.evidence_type, wordplay_pairs.verification_status, wordplay_pairs.status, \
wordplay_pairs.created_at, wordplay_pairs.updated_at";

/// A `wordplay_pairs` row.
#[derive(Clone, Debug, PartialEq)]
pub struct PairRow {
    pub id: i64,
    pub from_track_id: i64,
    pub to_track_id: i64,
    pub keyword: String,
    pub normalized_keyword: String,
    pub source_phrase: String,
    pub target_phrase: String,
    pub source_section_position: String,
    pub target_section_position: String,
    pub source_cue_mode: String,
    pub from_timestamp: Option<f64>,
    pub source_cue_end_timestamp: Option<f64>,
    pub to_timestamp: Option<f64>,
    pub target_intro_timestamp: Option<f64>,
    pub target_landing_timestamp: Option<f64>,
    pub transition_notes: String,
    pub source_url: String,
    pub evidence_type: String,
    pub verification_status: String,
    pub status: String,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

impl PairRow {
    fn from_row(row: &Row<'_>) -> duckdb::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            from_track_id: row.get(1)?,
            to_track_id: row.get(2)?,
            keyword: row.get(3)?,
            normalized_keyword: row.get(4)?,
            source_phrase: row.get(5)?,
            target_phrase: row.get(6)?,
            source_section_position: row.get(7)?,
            target_section_position: row.get(8)?,
            source_cue_mode: row.get(9)?,
            from_timestamp: row.get(10)?,
            source_cue_end_timestamp: row.get(11)?,
            to_timestamp: row.get(12)?,
            target_intro_timestamp: row.get(13)?,
            target_landing_timestamp: row.get(14)?,
            transition_notes: row.get(15)?,
            source_url: row.get(16)?,
            evidence_type: row.get(17)?,
            verification_status: row.get(18)?,
            status: row.get(19)?,
            created_at: row.get(20)?,
            updated_at: row.get(21)?,
        })
    }
}

fn query(conn: &Connection, sql: Sql) -> Result<Vec<PairRow>, DbError> {
    let mut statement = conn.prepare(&sql.text)?;
    let rows = statement.query_map(params_from_iter(sql.params), PairRow::from_row)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `list_pairs`' query: by id, optionally one status and one source track.
pub fn list_pairs(conn: &Connection, status: Option<&str>, from_track_id: Option<BigInt>) -> Result<Vec<PairRow>, DbError> {
    let mut sql = Sql::default();
    sql.push(&format!("SELECT {PAIR_COLUMNS} FROM wordplay_pairs"));
    let mut glue = " WHERE ";
    if let Some(status) = status {
        sql.push(glue);
        sql.push("wordplay_pairs.status = ");
        sql.bind(Value::Text(status.into()));
        glue = " AND ";
    }
    if let Some(from) = from_track_id {
        sql.push(glue);
        sql.push("wordplay_pairs.from_track_id = ");
        sql.bind(from.value());
    }
    sql.push(" ORDER BY wordplay_pairs.id");
    query(conn, sql)
}

/// `session.get(WordplayPair, id)`.
pub fn get_pair(conn: &Connection, id: BigInt) -> Result<Option<PairRow>, DbError> {
    let sql = format!("SELECT {PAIR_COLUMNS} FROM wordplay_pairs WHERE wordplay_pairs.id = ?");
    Ok(conn.query_row(&sql, [id.value()], PairRow::from_row).optional()?)
}

/// `_approved_wordplay`: approved pairs, a tested one preferred where
/// several words connect the same two tracks; one per `(from, to)`.
pub fn approved_edges(conn: &Connection, from_track_id: Option<i64>) -> Result<Vec<PairRow>, DbError> {
    let mut pairs = list_pairs(conn, Some("approved"), from_track_id.map(|id| BigInt(id.into())))?;
    pairs.sort_by_key(|pair| (pair.verification_status != "tested", pair.id));
    let mut seen = std::collections::BTreeSet::new();
    pairs.retain(|pair| seen.insert((pair.from_track_id, pair.to_track_id)));
    Ok(pairs)
}

/// The existing pair for one direction and normalized keyword.
pub fn find_pair(conn: &Connection, from: i64, to: i64, normalized_keyword: &str) -> Result<Option<PairRow>, DbError> {
    let sql = format!(
        "SELECT {PAIR_COLUMNS} FROM wordplay_pairs WHERE wordplay_pairs.from_track_id = ? \
         AND wordplay_pairs.to_track_id = ? AND wordplay_pairs.normalized_keyword = ? LIMIT 1"
    );
    Ok(conn.query_row(&sql, params![from, to, normalized_keyword], PairRow::from_row).optional()?)
}

fn write_values(pair: &PairRow) -> [Value; 19] {
    let text = |value: &str| Value::Text(value.to_owned());
    let float = |value: Option<f64>| value.map_or(Value::Null, Value::Double);
    [
        Value::BigInt(pair.from_track_id),
        Value::BigInt(pair.to_track_id),
        text(&pair.keyword),
        text(&pair.normalized_keyword),
        text(&pair.source_phrase),
        text(&pair.target_phrase),
        text(&pair.source_section_position),
        text(&pair.target_section_position),
        text(&pair.source_cue_mode),
        float(pair.from_timestamp),
        float(pair.source_cue_end_timestamp),
        float(pair.to_timestamp),
        float(pair.target_intro_timestamp),
        float(pair.target_landing_timestamp),
        text(&pair.transition_notes),
        text(&pair.source_url),
        text(&pair.evidence_type),
        text(&pair.verification_status),
        text(&pair.status),
    ]
}

const WRITE_COLUMNS: &str = "from_track_id, to_track_id, keyword, normalized_keyword, source_phrase, target_phrase, \
source_section_position, target_section_position, source_cue_mode, from_timestamp, source_cue_end_timestamp, \
to_timestamp, target_intro_timestamp, target_landing_timestamp, transition_notes, source_url, evidence_type, \
verification_status, status";

/// Insert `pair` (its `id` is ignored); `Err` on the unique constraint.
pub fn insert_pair(conn: &Connection, pair: &PairRow) -> Result<PairRow, DbError> {
    let mut values: Vec<Value> = write_values(pair).into();
    values.push(pair.created_at.map_or(Value::Null, timestamp));
    values.push(pair.updated_at.map_or(Value::Null, timestamp));
    let id: i64 = conn.query_row(
        &format!("INSERT INTO wordplay_pairs ({WRITE_COLUMNS}, created_at, updated_at) VALUES ({}) RETURNING id", vec!["?"; 21].join(", ")),
        params_from_iter(values),
        |row| row.get(0),
    )?;
    Ok(get_pair(conn, BigInt(id.into()))?.expect("inserted row"))
}

fn timestamp(value: NaiveDateTime) -> Value {
    Value::Timestamp(duckdb::types::TimeUnit::Microsecond, value.and_utc().timestamp_micros())
}

/// Write every column of `pair` (by its id) and `updated_at`.
pub fn update_pair(conn: &Connection, pair: &PairRow) -> Result<PairRow, DbError> {
    let assignments: Vec<String> = WRITE_COLUMNS.split(", ").map(|column| format!("{column}=?")).collect();
    let mut values: Vec<Value> = write_values(pair).into();
    values.push(pair.updated_at.map_or(Value::Null, timestamp));
    values.push(Value::BigInt(pair.id));
    conn.execute(
        &format!("UPDATE wordplay_pairs SET {}, updated_at=? WHERE wordplay_pairs.id = ?", assignments.join(", ")),
        params_from_iter(values),
    )?;
    Ok(get_pair(conn, BigInt(pair.id.into()))?.expect("updated row"))
}

/// Delete one pair; `false` when it does not exist.
pub fn delete_pair(conn: &Connection, id: BigInt) -> Result<bool, DbError> {
    let Some(pair) = get_pair(conn, id)? else { return Ok(false) };
    conn.execute("DELETE FROM wordplay_pairs WHERE wordplay_pairs.id = ?", [pair.id])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_edges_prefer_tested_pairs() {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO wordplay_pairs (from_track_id, to_track_id, keyword, normalized_keyword, source_phrase, target_phrase, status, verification_status) VALUES
               (1, 2, 'a', 'a', 's', 't', 'approved', 'unverified'),
               (1, 2, 'b', 'b', 's', 't', 'approved', 'tested'),
               (1, 3, 'c', 'c', 's', 't', 'pending', 'unverified'),
               (2, 3, 'd', 'd', 's', 't', 'approved', 'unverified');",
        )
        .unwrap();
        let edges: Vec<(i64, i64, i64)> =
            approved_edges(&conn, None).unwrap().iter().map(|pair| (pair.id, pair.from_track_id, pair.to_track_id)).collect();
        assert_eq!(edges, vec![(2, 1, 2), (4, 2, 3)]);
        assert_eq!(approved_edges(&conn, Some(2)).unwrap().len(), 1);
        assert_eq!(list_pairs(&conn, Some("pending"), None).unwrap().len(), 1);
        let mut pair = get_pair(&conn, BigInt(3)).unwrap().unwrap();
        pair.keyword = "z".into();
        assert_eq!(update_pair(&conn, &pair).unwrap().keyword, "z");
        assert!(delete_pair(&conn, BigInt(3)).unwrap() && !delete_pair(&conn, BigInt(3)).unwrap());
    }
}
