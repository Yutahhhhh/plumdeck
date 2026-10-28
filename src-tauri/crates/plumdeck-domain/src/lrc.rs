//! LRC time tags (backend/api/routers/lyrics.py).

use std::sync::LazyLock;

use plumdeck_core::compat::text::py_strip;
use regex::Regex;

use crate::pychars::ascii_digits;

/// `LRC_TIME_TAG_RE`: `[mm:ss.xx]` tags only, not other bracketed text.
static TIME_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\d{1,2}:\d{2}(?:\.\d+)?\]").expect("time tag"));

/// The pattern of `parse_lrc_timestamp`.
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(\d+):(\d+(?:\.\d+)?)\]").expect("timestamp"));

/// Why a timestamp could not be computed where Python raised (a 500).
#[derive(Debug, PartialEq, Eq)]
pub struct TimestampOverflow;

/// `parse_lrc_timestamp`: the first `[m:ss.xx]` tag as seconds.
pub fn parse_timestamp(line: &str) -> Result<Option<f64>, TimestampOverflow> {
    let Some(captures) = TIMESTAMP.captures(line) else { return Ok(None) };
    let minutes = ascii_digits(&captures[1]);
    let seconds: f64 = ascii_digits(&captures[2]).parse().expect("decimal seconds");
    // `int(m) * 60 + float(s)`: the exact product, rounded once to a float.
    let product = minutes
        .trim_start_matches('0')
        .parse::<u128>()
        .ok()
        .or_else(|| minutes.chars().all(|c| c == '0').then_some(0))
        .and_then(|m| m.checked_mul(60));
    let minutes_seconds = match product {
        Some(exact) => exact as f64,
        // Beyond u128 the product is far past f64 precision; Python's
        // int-to-float rounding only matters near overflow.
        None => format!("{minutes}e0").parse::<f64>().expect("digits") * 60.0,
    };
    if !minutes_seconds.is_finite() {
        return Err(TimestampOverflow);
    }
    Ok(Some(minutes_seconds + seconds))
}

/// `LRC_TIME_TAG_RE.sub('', line).strip()`.
pub fn clean_line(line: &str) -> String {
    py_strip(&TIME_TAG.replace_all(line, "")).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_clean_lines() {
        assert_eq!(parse_timestamp("[01:02.50] la"), Ok(Some(62.5)));
        assert_eq!(parse_timestamp("x [123:4] y"), Ok(Some(7384.0)));
        assert_eq!(parse_timestamp("[１:０２] la"), Ok(Some(62.0)));
        assert_eq!(parse_timestamp("[1:2.] la"), Ok(None));
        assert_eq!(parse_timestamp("no tag"), Ok(None));
        assert_eq!(parse_timestamp(&format!("[{}:00]", "9".repeat(400))), Err(TimestampOverflow));
        assert_eq!(clean_line(" [00:01.00][00:02]hello [chorus] [123:45] \x1c"), "hello [chorus] [123:45]");
    }
}
