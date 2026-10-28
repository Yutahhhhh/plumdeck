//! The `setlists` and `setlist_tracks` tables behind the setlist routes
//! (backend/infra/repositories/setlist_repository.py and
//! backend/app/services/setlist_app_service.py, S4).
//!
//! The routes store some request values untyped (`PUT /api/setlists/{id}`
//! sets any model column, `POST .../tracks` copies entry fields as sent).
//! Python hands those values to DuckDB as parameters of their JSON type and
//! DuckDB casts them into the column (or fails, a 500); [`py_param`] binds
//! them the same way so the cast is DuckDB's in both.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDateTime;
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection, OptionalExt, Row};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::compat::pyobj::{py_int, py_str, truthy, IntError};
use plumdeck_core::compat::text::py_strip;
use plumdeck_domain::set_duration::{set_duration_ms, Entry, SetTimingError};

use super::settings::finish;
use super::tracks::{get_track, BigInt, Sql, TrackRow, TRACK_COLUMNS};
use crate::DbError;

/// The `Setlist` model's columns, in model order.
pub const SETLIST_COLUMNS: &str = "setlists.id, setlists.name, setlists.description, setlists.display_order, \
setlists.genre, setlists.target_duration, setlists.rating, setlists.created_at, setlists.updated_at";

/// The model fields `PUT /api/setlists/{id}` may set (`hasattr` on a field).
pub const SETLIST_FIELDS: [&str; 9] =
    ["id", "name", "description", "display_order", "genre", "target_duration", "rating", "created_at", "updated_at"];

/// A `setlists` row as SQLModel loads it.
#[derive(Clone, Debug, PartialEq)]
pub struct SetlistRow {
    pub id: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub display_order: Option<i64>,
    pub genre: Option<String>,
    pub target_duration: Option<f64>,
    pub rating: Option<i64>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

impl SetlistRow {
    fn from_row(row: &Row<'_>) -> duckdb::Result<Self> {
        Ok(Self {
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
    }
}

/// The `SetlistTrack` model's columns, in model order.
pub const ENTRY_COLUMNS: &str = "setlist_tracks.id, setlist_tracks.setlist_id, setlist_tracks.track_id, \
setlist_tracks.position, setlist_tracks.transition_note, setlist_tracks.wordplay_json, setlist_tracks.in_ms, \
setlist_tracks.out_ms, setlist_tracks.playback_rate, setlist_tracks.extra_duration_ms, setlist_tracks.overlap_next_ms, \
setlist_tracks.revision, setlist_tracks.created_at";

/// A `setlist_tracks` row.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryRow {
    pub id: i64,
    pub setlist_id: i64,
    pub track_id: i64,
    pub position: i64,
    pub transition_note: Option<String>,
    pub wordplay_json: Option<String>,
    pub in_ms: f64,
    pub out_ms: Option<f64>,
    pub playback_rate: f64,
    pub extra_duration_ms: f64,
    pub overlap_next_ms: f64,
    pub revision: i64,
    pub created_at: Option<NaiveDateTime>,
}

impl EntryRow {
    fn from_row(row: &Row<'_>, at: usize) -> duckdb::Result<Self> {
        Ok(Self {
            id: row.get(at)?,
            setlist_id: row.get(at + 1)?,
            track_id: row.get(at + 2)?,
            position: row.get(at + 3)?,
            transition_note: row.get(at + 4)?,
            wordplay_json: row.get(at + 5)?,
            in_ms: row.get(at + 6)?,
            out_ms: row.get(at + 7)?,
            playback_rate: row.get(at + 8)?,
            extra_duration_ms: row.get(at + 9)?,
            overlap_next_ms: row.get(at + 10)?,
            revision: row.get(at + 11)?,
            created_at: row.get(at + 12)?,
        })
    }
}

/// `find_all`: newest update first (ties in DuckDB's order, as in Python).
pub fn list_setlists(conn: &Connection) -> Result<Vec<SetlistRow>, DbError> {
    let mut statement = conn.prepare(&format!("SELECT {SETLIST_COLUMNS} FROM setlists ORDER BY setlists.updated_at DESC"))?;
    let rows = statement.query_map([], SetlistRow::from_row)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `session.get(Setlist, id)`.
pub fn get_setlist(conn: &Connection, id: BigInt) -> Result<Option<SetlistRow>, DbError> {
    let sql = format!("SELECT {SETLIST_COLUMNS} FROM setlists WHERE setlists.id = ?");
    Ok(conn.query_row(&sql, [id.value()], SetlistRow::from_row).optional()?)
}

/// `create_setlist(name)`: a new row with the model defaults.
pub fn create_setlist(conn: &Connection, name: &str, now: NaiveDateTime) -> Result<SetlistRow, DbError> {
    let id: i64 = conn.query_row(
        "INSERT INTO setlists (name, description, display_order, genre, target_duration, rating, created_at, updated_at) \
         VALUES (?, NULL, 0, NULL, NULL, NULL, ?, ?) RETURNING id",
        params![name, now, now],
        |row| row.get(0),
    )?;
    Ok(get_setlist(conn, BigInt(id.into()))?.expect("inserted row"))
}

/// A JSON value as the parameter DuckDB's Python binding makes of it
/// (ints as INTEGER, BIGINT or HUGEINT by size, lists as LIST, dicts as
/// STRUCT), written into `sql`. `Err` for a value Python cannot bind.
pub(crate) fn py_param(sql: &mut Sql, value: &PyValue) -> Result<(), DbError> {
    match value {
        PyValue::None => sql.bind(Value::Null),
        PyValue::Bool(flag) => sql.bind(Value::Boolean(*flag)),
        PyValue::Int(number) => sql.bind(match (i32::try_from(*number), i64::try_from(*number)) {
            (Ok(small), _) => Value::Int(small),
            (_, Ok(big)) => Value::BigInt(big),
            _ => Value::HugeInt(*number),
        }),
        PyValue::Float(number) => sql.bind(Value::Double(*number)),
        PyValue::Str(text) => sql.bind(Value::Text(text.clone())),
        PyValue::List(items) => {
            sql.push("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    sql.push(", ");
                }
                py_param(sql, item)?;
            }
            sql.push("]");
        }
        PyValue::Dict(pairs) if !pairs.is_empty() => {
            sql.push("{");
            for (index, (key, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    sql.push(", ");
                }
                let PyValue::Str(key) = key else { return Err(DbError::Unsupported("non-string dict key")) };
                sql.push(&format!("'{}': ", key.replace('\'', "''")));
                py_param(sql, item)?;
            }
            sql.push("}");
        }
        _ => return Err(DbError::Unsupported("value DuckDB cannot bind")),
    }
    Ok(())
}

fn execute(conn: &Connection, sql: Sql) -> Result<usize, DbError> {
    Ok(conn.execute(&sql.text, params_from_iter(sql.params))?)
}

/// `update_setlist`: set each supplied model column, then `updated_at`;
/// `None` when the setlist does not exist. Answers the reloaded row (by
/// its new id when `id` was changed).
pub fn update_setlist(
    conn: &Connection,
    id: BigInt,
    changes: &[(String, PyValue)],
    now: NaiveDateTime,
) -> Result<Option<SetlistRow>, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let Some(current) = get_setlist(conn, id)? else { return Ok(None) };
        // Later duplicates win, as setattr in order does.
        let mut values: BTreeMap<&str, &PyValue> = BTreeMap::new();
        let mut order: Vec<&str> = Vec::new();
        for (key, value) in changes {
            if let Some(field) = SETLIST_FIELDS.iter().find(|field| **field == key) {
                if values.insert(field, value).is_none() {
                    order.push(field);
                }
            }
        }
        values.insert("updated_at", &PyValue::None);
        if !order.contains(&"updated_at") {
            order.push("updated_at");
        }
        let mut sql = Sql::default();
        sql.push("UPDATE setlists SET ");
        for (index, field) in order.iter().enumerate() {
            if index > 0 {
                sql.push(", ");
            }
            sql.push(&format!("\"{field}\"="));
            if *field == "updated_at" {
                sql.bind(Value::Timestamp(duckdb::types::TimeUnit::Microsecond, now.and_utc().timestamp_micros()));
            } else {
                py_param(&mut sql, values[field])?;
            }
        }
        sql.push(" WHERE setlists.id = ");
        sql.bind(Value::BigInt(current.id));
        execute(conn, sql)?;
        let reload = match values.get("id") {
            Some(new_id) => match py_int(new_id) {
                Ok(new_id) => BigInt(new_id),
                Err(_) => BigInt(current.id.into()),
            },
            None => BigInt(current.id.into()),
        };
        get_setlist(conn, reload)
    })();
    finish(conn, result)
}

/// `delete_setlist`: its entries, then the row. `false` when missing.
pub fn delete_setlist(conn: &Connection, id: BigInt) -> Result<bool, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let Some(current) = get_setlist(conn, id)? else { return Ok(false) };
        conn.execute("DELETE FROM setlist_tracks WHERE setlist_id=?", [current.id])?;
        conn.execute("DELETE FROM setlists WHERE setlists.id = ?", [current.id])?;
        Ok(true)
    })();
    finish(conn, result)
}

/// One `get_tracks` row: the entry, its track and the lyrics text.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryTrack {
    pub entry: EntryRow,
    pub track: TrackRow,
    pub lyrics: Option<String>,
}

impl EntryTrack {
    /// `bool(content and content.strip())`.
    pub fn has_lyrics(&self) -> bool {
        self.lyrics.as_deref().is_some_and(|content| !py_strip(content).is_empty())
    }
}

/// `get_tracks`: entries joined to their tracks, by position (ties in
/// DuckDB's order, as in Python).
pub fn setlist_tracks(conn: &Connection, setlist_id: BigInt) -> Result<Vec<EntryTrack>, DbError> {
    let sql = format!(
        "SELECT {ENTRY_COLUMNS}, {TRACK_COLUMNS}, lyrics.content FROM setlist_tracks, tracks \
         LEFT OUTER JOIN lyrics ON tracks.id = lyrics.track_id \
         WHERE setlist_tracks.setlist_id = ? AND setlist_tracks.track_id = tracks.id ORDER BY setlist_tracks.position"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map([setlist_id.value()], |row| {
            Ok(EntryTrack { entry: EntryRow::from_row(row, 0)?, track: TrackRow::from_row(row, 13)?, lyrics: row.get(37)? })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `session.get(SetlistTrack, id)`.
pub fn get_entry(conn: &Connection, id: BigInt) -> Result<Option<EntryRow>, DbError> {
    let sql = format!("SELECT {ENTRY_COLUMNS} FROM setlist_tracks WHERE setlist_tracks.id = ?");
    Ok(conn.query_row(&sql, [id.value()], |row| EntryRow::from_row(row, 0)).optional()?)
}

/// `PATCH /api/setlist-tracks/{id}/wordplay`: set the entry's wordplay JSON
/// text (or NULL); `None` when the entry does not exist.
pub fn set_entry_wordplay(conn: &Connection, id: BigInt, wordplay_json: Option<&str>) -> Result<Option<EntryRow>, DbError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = (|| {
        let Some(entry) = get_entry(conn, id)? else { return Ok(None) };
        conn.execute("UPDATE setlist_tracks SET wordplay_json=? WHERE setlist_tracks.id = ?", params![wordplay_json, entry.id])?;
        get_entry(conn, BigInt(entry.id.into()))
    })();
    finish(conn, result)
}

/// Why `update_setlist_tracks` refused the list.
#[derive(Debug)]
pub enum ReplaceError {
    /// A `ValueError` the route answers as 409 with this detail.
    Conflict(String),
    /// Anything else Python raises (TypeError, OverflowError): a 500.
    Server(&'static str),
    Db(DbError),
}

impl From<DbError> for ReplaceError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl From<duckdb::Error> for ReplaceError {
    fn from(error: duckdb::Error) -> Self {
        Self::Db(error.into())
    }
}

fn int_of(value: &PyValue) -> Result<i128, ReplaceError> {
    py_int(value).map_err(|error| match error {
        IntError::Value(message) => ReplaceError::Conflict(message),
        IntError::Type => ReplaceError::Server("int() of a non-number"),
        IntError::Overflow => ReplaceError::Server("int() overflow"),
    })
}

/// The entry fields a client may set, in Python's order.
const ENTRY_FIELDS: [&str; 6] = ["wordplay_json", "in_ms", "out_ms", "playback_rate", "extra_duration_ms", "overlap_next_ms"];

/// One desired entry: an existing row (its id) or a new one, and its values.
struct Desired {
    id: Option<i64>,
    track_id: i128,
    position: usize,
    revision: i64,
    values: BTreeMap<&'static str, PyValue>,
}

fn dict_get<'a>(pairs: &'a [(PyValue, PyValue)], key: &str) -> Option<&'a PyValue> {
    pairs.iter().rev().find(|(name, _)| matches!(name, PyValue::Str(text) if text == key)).map(|(_, value)| value)
}

fn entry_values(row: &EntryRow) -> BTreeMap<&'static str, PyValue> {
    let float = PyValue::Float;
    BTreeMap::from([
        ("wordplay_json", row.wordplay_json.clone().map_or(PyValue::None, PyValue::Str)),
        ("in_ms", float(row.in_ms)),
        ("out_ms", row.out_ms.map_or(PyValue::None, float)),
        ("playback_rate", float(row.playback_rate)),
        ("extra_duration_ms", float(row.extra_duration_ms)),
        ("overlap_next_ms", float(row.overlap_next_ms)),
    ])
}

fn new_values() -> BTreeMap<&'static str, PyValue> {
    BTreeMap::from([
        ("wordplay_json", PyValue::None),
        ("in_ms", PyValue::Int(0)),
        ("out_ms", PyValue::None),
        ("playback_rate", PyValue::Int(1)),
        ("extra_duration_ms", PyValue::Int(0)),
        ("overlap_next_ms", PyValue::Int(0)),
    ])
}

/// `update_setlist_tracks`: make the setlist's entries `items` (track ids,
/// or objects with `track_id`/`id`, `entry_id`/`setlist_track_id`,
/// `revision` and entry fields), reusing existing entries. `Ok(false)` when
/// the setlist does not exist.
pub fn replace_setlist_tracks(
    conn: &Connection,
    setlist_id: BigInt,
    items: &[PyValue],
    now: NaiveDateTime,
) -> Result<bool, ReplaceError> {
    conn.execute_batch("BEGIN TRANSACTION")?;
    let result = replace_in_transaction(conn, setlist_id, items, now);
    match result {
        Ok(found) => {
            conn.execute_batch("COMMIT")?;
            Ok(found)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn replace_in_transaction(conn: &Connection, setlist_id: BigInt, items: &[PyValue], now: NaiveDateTime) -> Result<bool, ReplaceError> {
    let Some(setlist) = get_setlist(conn, setlist_id)? else { return Ok(false) };
    let existing: Vec<EntryRow> = {
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM setlist_tracks WHERE setlist_tracks.setlist_id = ?");
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([setlist.id], |row| EntryRow::from_row(row, 0))?.collect::<Result<_, _>>()?;
        rows
    };
    let by_id: BTreeMap<i64, &EntryRow> = existing.iter().map(|row| (row.id, row)).collect();
    let mut ordered: Vec<&EntryRow> = existing.iter().collect();
    ordered.sort_by_key(|row| (row.position, row.id));
    let mut reusable: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for row in &ordered {
        reusable.entry(row.track_id).or_default().push(row.id);
    }
    let previous_next: BTreeMap<i64, Option<i64>> =
        ordered.iter().enumerate().map(|(index, row)| (row.id, ordered.get(index + 1).map(|next| next.id))).collect();
    let mut desired: Vec<Desired> = Vec::new();
    let mut claimed: BTreeSet<i64> = BTreeSet::new();
    let mut tracks: BTreeMap<i128, TrackRow> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let (tid, entry_id, data): (&PyValue, Option<&PyValue>, &[(PyValue, PyValue)]) = match item {
            PyValue::Dict(pairs) => (
                dict_get(pairs, "track_id").or_else(|| dict_get(pairs, "id")).unwrap_or(&PyValue::None),
                dict_get(pairs, "entry_id").or_else(|| dict_get(pairs, "setlist_track_id")),
                pairs,
            ),
            other => (other, None, &[]),
        };
        if *tid == PyValue::None {
            continue;
        }
        let track_id = int_of(tid)?;
        let Some(track) = get_track(conn, BigInt(track_id))? else {
            return Err(ReplaceError::Conflict(format!("Track not found: {}", py_str(tid))));
        };
        tracks.insert(track_id, track);
        let entry_id = entry_id.filter(|value| truthy(value));
        let mut row = match entry_id {
            Some(value) => {
                let wanted = int_of(value)?;
                match i64::try_from(wanted).ok().and_then(|id| by_id.get(&id)) {
                    Some(row) => Some(*row),
                    None => return Err(ReplaceError::Conflict("Setlist entry is stale or belongs to another setlist".into())),
                }
            }
            None => None,
        };
        if row.is_none() {
            // Older clients send only track ids: keep the Nth occurrence.
            row = i64::try_from(track_id)
                .ok()
                .and_then(|id| reusable.get(&id))
                .and_then(|ids| ids.iter().find(|id| !claimed.contains(id)))
                .map(|id| by_id[id]);
        }
        if row.is_some_and(|row| claimed.contains(&row.id)) {
            return Err(ReplaceError::Conflict("The same setlist entry was supplied more than once".into()));
        }
        let mut entry = match row {
            Some(row) => {
                claimed.insert(row.id);
                if let Some(expected) = dict_get(data, "revision").filter(|value| **value != PyValue::None) {
                    if int_of(expected)? != i128::from(row.revision) {
                        return Err(ReplaceError::Conflict("Setlist entry was changed in another view".into()));
                    }
                }
                Desired { id: Some(row.id), track_id, position: index, revision: row.revision, values: entry_values(row) }
            }
            None => Desired { id: None, track_id, position: index, revision: 1, values: new_values() },
        };
        for field in ENTRY_FIELDS {
            if let Some(value) = dict_get(data, field) {
                entry.values.insert(field, value.clone());
            }
        }
        desired.push(entry);
    }
    // An overlap belongs to an exact adjacent pair; moving either side clears it.
    let next_ids: Vec<Option<i64>> = (0..desired.len()).map(|index| desired.get(index + 1).and_then(|next| next.id)).collect();
    for (entry, next_id) in desired.iter_mut().zip(next_ids) {
        if let Some(id) = entry.id {
            if previous_next.get(&id).copied().flatten() != next_id {
                entry.values.insert("overlap_next_ms", PyValue::Int(0));
            }
        }
    }
    let timing: Vec<Entry> = desired
        .iter()
        .map(|entry| Entry {
            duration: tracks[&entry.track_id].duration,
            in_ms: entry.values["in_ms"].clone(),
            out_ms: entry.values["out_ms"].clone(),
            playback_rate: entry.values["playback_rate"].clone(),
            extra_duration_ms: entry.values["extra_duration_ms"].clone(),
            overlap_next_ms: entry.values["overlap_next_ms"].clone(),
        })
        .collect();
    set_duration_ms(&timing).map_err(|error| match error {
        SetTimingError::Value(message) => ReplaceError::Conflict(message),
        SetTimingError::Overflow => ReplaceError::Server("float() overflow"),
    })?;
    // SQLAlchemy's flush: inserts and updates in session order, then deletes.
    for entry in &desired {
        let mut sql = Sql::default();
        match entry.id {
            Some(id) => {
                sql.push("UPDATE setlist_tracks SET track_id=");
                py_param(&mut sql, &PyValue::Int(entry.track_id))?;
                sql.push(", position=");
                sql.bind(Value::BigInt(entry.position as i64));
                for field in ENTRY_FIELDS {
                    sql.push(&format!(", {field}="));
                    py_param(&mut sql, &entry.values[field])?;
                }
                sql.push(", revision=");
                sql.bind(Value::BigInt(entry.revision + 1));
                sql.push(" WHERE setlist_tracks.id = ");
                sql.bind(Value::BigInt(id));
            }
            None => {
                sql.push("INSERT INTO setlist_tracks (setlist_id, track_id, position, transition_note, ");
                sql.push(&ENTRY_FIELDS.join(", "));
                sql.push(", revision, created_at) VALUES (");
                sql.bind(Value::BigInt(setlist.id));
                sql.push(", ");
                py_param(&mut sql, &PyValue::Int(entry.track_id))?;
                sql.push(", ");
                sql.bind(Value::BigInt(entry.position as i64));
                sql.push(", NULL");
                for field in ENTRY_FIELDS {
                    sql.push(", ");
                    py_param(&mut sql, &entry.values[field])?;
                }
                sql.push(", 1, ");
                sql.bind(Value::Timestamp(duckdb::types::TimeUnit::Microsecond, now.and_utc().timestamp_micros()));
                sql.push(")");
            }
        }
        execute(conn, sql)?;
    }
    for row in &existing {
        if !claimed.contains(&row.id) {
            conn.execute("DELETE FROM setlist_tracks WHERE setlist_tracks.id = ?", [row.id])?;
        }
    }
    conn.execute("UPDATE setlists SET updated_at=? WHERE setlists.id = ?", params![now, setlist.id])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tracks (filepath, title, duration) VALUES ('/a', 'A', 300.0), ('/b', 'B', 60.0);
             INSERT INTO setlists (name, created_at, updated_at) VALUES ('S', '2026-01-01', '2026-01-01');
             INSERT INTO setlist_tracks (setlist_id, track_id, position, overlap_next_ms) VALUES (1, 1, 0, 5000), (1, 2, 1, 0);",
        )
        .unwrap();
        conn
    }

    fn noon() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap().and_hms_opt(12, 0, 0).unwrap()
    }

    fn conflict(result: Result<bool, ReplaceError>) -> String {
        match result {
            Err(ReplaceError::Conflict(message)) => message,
            other => panic!("not a conflict: {other:?}"),
        }
    }

    #[test]
    fn reorder_keeps_entries_and_clears_moved_overlaps() {
        let conn = library();
        let items = [PyValue::Int(2), PyValue::Str(" 1 ".into())];
        assert!(replace_setlist_tracks(&conn, BigInt(1), &items, noon()).unwrap());
        let rows = setlist_tracks(&conn, BigInt(1)).unwrap();
        let ids: Vec<(i64, i64, f64, i64)> =
            rows.iter().map(|row| (row.entry.id, row.entry.track_id, row.entry.overlap_next_ms, row.entry.revision)).collect();
        assert_eq!(ids, vec![(2, 2, 0.0, 2), (1, 1, 0.0, 2)]);
        assert!(!replace_setlist_tracks(&conn, BigInt(9), &items, noon()).unwrap());
    }

    #[test]
    fn refusals_match_python_messages() {
        let conn = library();
        let replace = |items: &[PyValue]| replace_setlist_tracks(&conn, BigInt(1), items, noon());
        assert_eq!(conflict(replace(&[PyValue::Int(7)])), "Track not found: 7");
        assert_eq!(conflict(replace(&[PyValue::Str("x".into())])), "invalid literal for int() with base 10: 'x'");
        let stale = PyValue::Dict(vec![(PyValue::Str("track_id".into()), PyValue::Int(1)), (PyValue::Str("entry_id".into()), PyValue::Int(99))]);
        assert_eq!(conflict(replace(&[stale])), "Setlist entry is stale or belongs to another setlist");
        let late = PyValue::Dict(vec![(PyValue::Str("track_id".into()), PyValue::Int(2)), (PyValue::Str("out_ms".into()), PyValue::Int(90_000))]);
        assert_eq!(conflict(replace(&[late])), "OUT が曲の長さを超えています");
        assert!(matches!(replace(&[PyValue::List(vec![])]), Err(ReplaceError::Server(_))));
        // Nothing was written.
        assert_eq!(setlist_tracks(&conn, BigInt(1)).unwrap().len(), 2);
    }

    #[test]
    fn update_casts_like_duckdb() {
        let conn = library();
        let changes = vec![("name".to_string(), PyValue::Int(5)), ("rating".to_string(), PyValue::Str("4".into())), ("bogus".to_string(), PyValue::Int(1))];
        let row = update_setlist(&conn, BigInt(1), &changes, noon()).unwrap().unwrap();
        assert_eq!((row.name.as_deref(), row.rating, row.updated_at), (Some("5"), Some(4), Some(noon())));
        let bad = vec![("rating".to_string(), PyValue::Str("x".into()))];
        assert!(update_setlist(&conn, BigInt(1), &bad, noon()).is_err());
        assert_eq!(update_setlist(&conn, BigInt(2), &[], noon()).unwrap(), None);
    }
}
