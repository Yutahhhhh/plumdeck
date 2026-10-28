//! Wall-clock helpers matching the Python backend: local naive timestamps at
//! microsecond precision, `datetime.isoformat()` and `str(datetime)`.
//!
//! The existing database stores local naive TIMESTAMPs, so SQL must bind
//! [`local_now_naive`] instead of calling `now()`/`CURRENT_TIMESTAMP`.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, Timelike};

/// `datetime.now()`: local wall-clock time without a zone, truncated to µs.
pub fn local_now_naive() -> NaiveDateTime {
    truncate_to_micros(Local::now().naive_local())
}

/// Drops sub-microsecond precision (Python and DuckDB keep microseconds).
pub fn truncate_to_micros(value: NaiveDateTime) -> NaiveDateTime {
    let nanos = value.nanosecond();
    value.with_nanosecond(nanos - nanos % 1_000).unwrap_or(value)
}

fn micros(value: &NaiveDateTime) -> u32 {
    // chrono encodes a leap second as nanosecond >= 1e9; Python has none.
    (value.nanosecond() % 1_000_000_000) / 1_000
}

fn format_with(value: &NaiveDateTime, separator: char) -> String {
    let mut out = format!(
        "{:04}-{:02}-{:02}{}{:02}:{:02}:{:02}",
        value.year(),
        value.month(),
        value.day(),
        separator,
        value.hour(),
        value.minute(),
        value.second()
    );
    let micro = micros(value);
    if micro != 0 {
        out.push_str(&format!(".{micro:06}"));
    }
    out
}

/// `datetime.isoformat()` for a naive datetime (also pydantic's JSON form):
/// `YYYY-MM-DDTHH:MM:SS`, plus `.ffffff` only when microseconds are non-zero.
pub fn py_isoformat(value: &NaiveDateTime) -> String {
    format_with(value, 'T')
}

/// `str(datetime)`: like [`py_isoformat`] with a space separator.
pub fn py_str_datetime(value: &NaiveDateTime) -> String {
    format_with(value, ' ')
}

/// `date.isoformat()` / `str(date)`.
pub fn py_date_isoformat(value: &NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", value.year(), value.month(), value.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microseconds_only_when_present() {
        let base = NaiveDate::from_ymd_opt(5, 1, 2).unwrap().and_hms_opt(3, 4, 5).unwrap();
        assert_eq!(py_isoformat(&base), "0005-01-02T03:04:05");
        let with_micros = base.with_nanosecond(123_456_789).unwrap();
        assert_eq!(py_str_datetime(&with_micros), "0005-01-02 03:04:05.123456");
        assert_eq!(truncate_to_micros(with_micros).nanosecond(), 123_456_000);
        assert_eq!(local_now_naive().nanosecond() % 1_000, 0);
    }
}
