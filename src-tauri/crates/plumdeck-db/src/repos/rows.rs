//! Raw SQL rows as `dict(row._mapping)` after `jsonable_encoder` (S7: the
//! Play routes answer their queries' rows as they come).

use duckdb::types::{TimeUnit, Value};
use duckdb::{params_from_iter, Connection};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_core::time::py_isoformat;

use crate::DbError;

fn timestamp(unit: TimeUnit, value: i64) -> PyValue {
    let micros = match unit {
        TimeUnit::Second => value.saturating_mul(1_000_000),
        TimeUnit::Millisecond => value.saturating_mul(1_000),
        TimeUnit::Microsecond => value,
        TimeUnit::Nanosecond => value / 1_000,
    };
    match chrono::DateTime::from_timestamp_micros(micros) {
        Some(at) => PyValue::Str(py_isoformat(&at.naive_utc())),
        None => PyValue::None,
    }
}

/// One DuckDB value as the Python binding hands it over, JSON-encoded.
pub fn json_value(value: Value) -> PyValue {
    match value {
        Value::Null => PyValue::None,
        Value::Boolean(b) => PyValue::Bool(b),
        Value::TinyInt(n) => PyValue::Int(n.into()),
        Value::SmallInt(n) => PyValue::Int(n.into()),
        Value::Int(n) => PyValue::Int(n.into()),
        Value::BigInt(n) => PyValue::Int(n.into()),
        Value::HugeInt(n) => PyValue::Int(n),
        Value::UTinyInt(n) => PyValue::Int(n.into()),
        Value::USmallInt(n) => PyValue::Int(n.into()),
        Value::UInt(n) => PyValue::Int(n.into()),
        Value::UBigInt(n) => PyValue::Int(n.into()),
        Value::Float(x) => PyValue::Float(x.into()),
        Value::Double(x) => PyValue::Float(x),
        Value::Text(s) => PyValue::Str(s),
        Value::Timestamp(unit, v) => timestamp(unit, v),
        Value::Date32(days) => match chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163) {
            Some(date) => PyValue::Str(plumdeck_core::time::py_date_isoformat(&date)),
            None => PyValue::None,
        },
        other => PyValue::Str(format!("{other:?}")),
    }
}

/// Every row of `sql` as an ordered dict of its columns.
pub fn json_rows(conn: &Connection, sql: &str, params: Vec<Value>) -> Result<Vec<PyValue>, DbError> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = statement.query(params_from_iter(params))?;
    let names: Vec<String> = rows.as_ref().expect("statement").column_names();
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut pairs = Vec::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            pairs.push((PyValue::Str(name.clone()), json_value(row.get::<_, Value>(index)?)));
        }
        out.push(PyValue::Dict(pairs));
    }
    Ok(out)
}

/// The first column of the first row as an integer (`count(*)` and friends).
pub fn scalar_int(conn: &Connection, sql: &str, params: Vec<Value>) -> Result<Option<i64>, DbError> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = statement.query(params_from_iter(params))?;
    Ok(match rows.next()? {
        Some(row) => row.get::<_, Option<i64>>(0)?,
        None => None,
    })
}
