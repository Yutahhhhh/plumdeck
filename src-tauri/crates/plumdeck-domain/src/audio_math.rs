//! Musical keys (backend/utils/audio_math.py) and the library's key filter
//! and key sort (backend/infra/repositories/track_repository.py).

use plumdeck_core::compat::text::{py_lower, py_strip};

use crate::pychars::is_py_whitespace;

/// `CAMELOT_ADJACENCY` keys, in dict order.
pub const CAMELOT_CODES: [&str; 24] = [
    "1A", "1B", "2A", "2B", "3A", "3B", "4A", "4B", "5A", "5B", "6A", "6B", "7A", "7B", "8A", "8B", "9A", "9B",
    "10A", "10B", "11A", "11B", "12A", "12B",
];

/// `KEY_TO_CAMELOT`, in dict order.
pub const KEY_TO_CAMELOT: [(&str, &str); 42] = [
    ("C Major", "8B"),
    ("C Minor", "5A"),
    ("C# Major", "3B"),
    ("Db Major", "3B"),
    ("C# Minor", "12A"),
    ("D Major", "10B"),
    ("D Minor", "7A"),
    ("D# Major", "5B"),
    ("Eb Major", "5B"),
    ("D# Minor", "2A"),
    ("Eb Minor", "2A"),
    ("E Major", "12B"),
    ("E Minor", "9A"),
    ("F Major", "7B"),
    ("F Minor", "4A"),
    ("F# Major", "2B"),
    ("Gb Major", "2B"),
    ("F# Minor", "11A"),
    ("G Major", "9B"),
    ("G Minor", "6A"),
    ("G# Major", "4B"),
    ("Ab Major", "4B"),
    ("G# Minor", "1A"),
    ("A Major", "11B"),
    ("A Minor", "8A"),
    ("A# Major", "6B"),
    ("Bb Major", "6B"),
    ("A# Minor", "3A"),
    ("Bb Minor", "3A"),
    ("B Major", "1B"),
    ("B Minor", "10A"),
    ("Db Minor", "12A"),
    ("Gb Minor", "11A"),
    ("Ab Minor", "1A"),
    ("Cb Major", "1B"),
    ("Cb Minor", "10A"),
    ("E# Major", "7B"),
    ("E# Minor", "4A"),
    ("Fb Major", "12B"),
    ("Fb Minor", "9A"),
    ("B# Major", "8B"),
    ("B# Minor", "5A"),
];

fn camelot_of(name: &str) -> Option<&'static str> {
    KEY_TO_CAMELOT.iter().find(|(spelling, _)| *spelling == name).map(|(_, code)| *code)
}

/// `re.fullmatch(..., re.IGNORECASE)` of one of `words` against `text`.
/// With IGNORECASE Python also matches `ı` (U+0131) and `İ` (U+0130) to `i`.
fn matches_word_ignorecase(text: &str, word: &str) -> bool {
    let mut chars = text.chars();
    for expected in word.chars() {
        let Some(c) = chars.next() else { return false };
        let same = c.eq_ignore_ascii_case(&expected) || (expected == 'i' && matches!(c, '\u{131}' | '\u{130}'));
        if !same {
            return false;
        }
    }
    chars.next().is_none()
}

/// `normalize_key`: any accepted spelling (`'F minor'`, `'Fm'`, `'4a'`,
/// `'D♯ Minor'`) to its Camelot code, or `None`.
pub fn normalize_key(key: Option<&str>) -> Option<&'static str> {
    let key = key?;
    let stripped = py_strip(key);
    if stripped.is_empty() {
        return None;
    }
    let key = stripped.replace('♯', "#").replace('♭', "b");
    // `key_str.upper() in CAMELOT_ADJACENCY`: only ASCII letters can
    // uppercase into a code.
    let upper = key.to_ascii_uppercase();
    if let Some(code) = CAMELOT_CODES.iter().find(|code| **code == upper) {
        return Some(code);
    }
    // ([A-Ga-g])([#b]?)\s*(major|minor|maj|min|m)? under IGNORECASE.
    let mut rest = key.as_str();
    let note = rest.chars().next().filter(|c| matches!(c.to_ascii_lowercase(), 'a'..='g'))?;
    rest = &rest[note.len_utf8()..];
    let accidental = match rest.chars().next() {
        Some(c @ ('#' | 'b' | 'B')) => {
            rest = &rest[1..];
            c.to_ascii_lowercase().to_string()
        }
        _ => String::new(),
    };
    rest = rest.trim_start_matches(is_py_whitespace);
    let scale = if rest.is_empty() {
        None
    } else if ["major", "minor", "maj", "min", "m"].iter().any(|word| matches_word_ignorecase(rest, word)) {
        Some(rest)
    } else {
        return None;
    };
    let minor = scale.is_some_and(|scale| scale != "M" && ["minor", "min", "m"].contains(&py_lower(scale).as_str()));
    let mode = if minor { "Minor" } else { "Major" };
    camelot_of(&format!("{}{accidental} {mode}", note.to_ascii_uppercase()))
}

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
