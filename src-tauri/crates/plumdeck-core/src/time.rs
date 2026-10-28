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

/// The wall clock, injectable so parity runs and tests can freeze it.
///
/// Rows get their timestamps from here, bound as parameters: the bundled
/// DuckDB has no ICU, so `now()`/`CURRENT_TIMESTAMP` in SQL would be UTC
/// where the Python backend stores local time (plan R3).
pub trait Clock: Send + Sync {
    /// `datetime.now()`: local naive time at microsecond precision.
    fn now_naive(&self) -> NaiveDateTime;
    /// `time.time()`: seconds since the Unix epoch.
    fn epoch_seconds(&self) -> f64;
}

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_naive(&self) -> NaiveDateTime {
        local_now_naive()
    }

    fn epoch_seconds(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64())
    }
}

/// A clock that always answers the same instant (tests, `freeze_now`).
#[derive(Clone, Copy, Debug)]
pub struct FixedClock {
    pub naive: NaiveDateTime,
    pub epoch: f64,
}

impl Clock for FixedClock {
    fn now_naive(&self) -> NaiveDateTime {
        self.naive
    }

    fn epoch_seconds(&self) -> f64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_reads_local_time_and_epoch_seconds() {
        let clock = SystemClock;
        let before = local_now_naive();
        let now = clock.now_naive();
        assert!(now >= before && now.nanosecond() % 1_000 == 0);
        assert!(clock.epoch_seconds() > 1.7e9);
        let fixed = FixedClock { naive: before, epoch: 1.5 };
        assert_eq!((fixed.now_naive(), fixed.epoch_seconds()), (before, 1.5));
    }

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
