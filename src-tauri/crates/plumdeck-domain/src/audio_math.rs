//! The library's key filter and key sort
//! (backend/infra/repositories/track_repository.py). `normalize_key` and its
//! tables live in plumdeck-key and are re-exported here.

use plumdeck_core::compat::text::{py_lower, py_strip};

pub use plumdeck_key::{normalize_key, ALPHANUMERIC_CODES as CAMELOT_CODES, KEY_TO_CAMELOT};

/// `_key_spellings`: the lowercase spellings stored for the same key as
/// `key` (`'D# Minor'` → `['d# minor', 'eb minor', '2a']`).
pub fn key_spellings(key: &str) -> Vec<String> {
    match normalize_key(Some(key)) {
        None => vec![py_lower(py_strip(key))],
        Some(code) => KEY_TO_CAMELOT
            .iter()
            .filter(|(_, value)| *value == code)
            .map(|(spelling, _)| spelling.to_ascii_lowercase())
            .chain(std::iter::once(code.to_ascii_lowercase()))
            .collect(),
    }
}

fn wheel_position(code: &str) -> i64 {
    let (number, letter) = code.split_at(code.len() - 1);
    let number: i64 = number.parse().expect("camelot number");
    (number - 1) * 2 + if letter.eq_ignore_ascii_case("A") { 1 } else { 2 }
}

/// `CAMELOT_ORDER`: lowercase spelling or code → wheel position (1..=24),
/// in dict order.
pub fn camelot_order() -> Vec<(String, i64)> {
    KEY_TO_CAMELOT
        .iter()
        .map(|(spelling, code)| (spelling.to_ascii_lowercase(), wheel_position(code)))
        .chain(CAMELOT_CODES.iter().map(|code| (code.to_ascii_lowercase(), wheel_position(code))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_key_follows_python() {
        let cases = [
            ("8a", Some("8A")),
            (" 12b ", Some("12B")),
            ("F minor", Some("4A")),
            ("Fm", Some("4A")),
            ("FM", Some("7B")),
            ("f MIN", Some("4A")),
            ("Dbm", Some("12A")),
            ("DB minor", Some("12A")),
            ("D♯ Minor", Some("2A")),
            ("Cb", Some("1B")),
            ("C\u{1c}minor", Some("5A")),
            ("C mınor", Some("8B")),
            ("H minor", None),
            ("C minorx", None),
            ("", None),
            ("  ", None),
            ("13A", None),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_key(Some(input)), expected, "{input:?}");
        }
        assert_eq!(normalize_key(None), None);
    }

    #[test]
    fn spellings_and_wheel_order() {
        assert_eq!(key_spellings("D# Minor"), ["d# minor", "eb minor", "2a"]);
        assert_eq!(key_spellings(" Weird "), ["weird"]);
        let order = camelot_order();
        assert_eq!(order.len(), 66);
        assert_eq!(order[0], ("c major".into(), 16));
        assert_eq!(order.iter().find(|(k, _)| k == "12b").unwrap().1, 24);
    }
}
