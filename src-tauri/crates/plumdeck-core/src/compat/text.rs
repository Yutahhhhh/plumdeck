//! Python 3.12 string semantics (Unicode 15.0.0): `casefold`, `lower`,
//! `split()`/`strip()`, `unicodedata.normalize`, the backend's
//! `normalize_text`, and the track search tokenizer.
//!
//! Case and character-class tables are generated from Python itself
//! (`tools/parity/gen_unicode_tables.py`); normalization uses the
//! unicode-normalization crate pinned to the same Unicode version.

use unicode_normalization::UnicodeNormalization;

use super::unicode_tables::{CASED, CASEFOLD, CASE_IGNORABLE, LOWER, SPACE, WORD};

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let code = c as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < code {
                std::cmp::Ordering::Less
            } else if lo > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn mapped(table: &'static [(u32, &'static str)], c: char) -> Option<&'static str> {
    table.binary_search_by_key(&(c as u32), |&(code, _)| code).ok().map(|i| table[i].1)
}

/// `str.isspace()` for one character (includes `\x1c`-`\x1f`, unlike Rust).
pub fn is_py_space(c: char) -> bool {
    SPACE.binary_search(&(c as u32)).is_ok()
}

/// `str.split()` with no arguments.
pub fn py_split(s: &str) -> Vec<&str> {
    s.split(is_py_space).filter(|part| !part.is_empty()).collect()
}

/// `str.strip()` with no arguments.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `" ".join(s.split())`.
pub fn collapse_spaces(s: &str) -> String {
    py_split(s).join(" ")
}

/// `str.casefold()`.
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match mapped(CASEFOLD, c) {
            Some(folded) => out.push_str(folded),
            None => out.push(c),
        }
    }
    out
}

/// `str.lower()`, including the final-sigma rule.
pub fn py_lower(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (index, &c) in chars.iter().enumerate() {
        if c == '\u{3a3}' {
            out.push(if is_final_sigma(&chars, index) { '\u{3c2}' } else { '\u{3c3}' });
            continue;
        }
        match mapped(LOWER, c) {
            Some(lowered) => out.push_str(lowered),
            None => out.push(c),
        }
    }
    out
}

fn is_final_sigma(chars: &[char], index: usize) -> bool {
    let cased = |c: char| in_ranges(CASED, c);
    let ignorable = |c: char| in_ranges(CASE_IGNORABLE, c);
    let before = chars[..index].iter().rev().copied().find(|&c| !ignorable(c));
    if !before.is_some_and(cased) {
        return false;
    }
    match chars[index + 1..].iter().copied().find(|&c| !ignorable(c)) {
        None => true,
        Some(next) => !cased(next),
    }
}

/// `unicodedata.normalize("NFC", s)`.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// `unicodedata.normalize("NFD", s)`.
pub fn nfd(s: &str) -> String {
    s.nfd().collect()
}

/// `unicodedata.normalize("NFKC", s)`.
pub fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

/// `unicodedata.normalize("NFKD", s)`.
pub fn nfkd(s: &str) -> String {
    s.nfkd().collect()
}

/// The backend's `normalize_text` (rekordbox_library, assist_filters and
/// phased_setlist_builder share it): NFKC, casefold, collapse whitespace.
/// `None` and `""` give `""`.
pub fn normalize_text(value: Option<&str>) -> String {
    match value {
        None | Some("") => String::new(),
        Some(text) => collapse_spaces(&casefold(&nfkc(text))),
    }
}

/// Matches re `[^\W_]`: `str.isalnum()`. Combining marks (Mn/Mc) are not
/// word characters in Python, so an NFD accent splits a token.
pub fn is_word_char(c: char) -> bool {
    in_ranges(WORD, c)
}

/// `re.findall(r"[^\W_]+(?:'[^\W_]+)?", s)`.
pub fn tokens(s: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut iter = s.char_indices().peekable();
    while let Some(&(start, c)) = iter.peek() {
        if !is_word_char(c) {
            iter.next();
            continue;
        }
        let mut end = start;
        while let Some(&(at, c)) = iter.peek() {
            if !is_word_char(c) {
                break;
            }
            end = at + c.len_utf8();
            iter.next();
        }
        // Optional `'` followed by at least one more word character.
        if s[end..].starts_with('\'') {
            let rest = &s[end + 1..];
            let tail: usize =
                rest.chars().take_while(|&c| is_word_char(c)).map(char::len_utf8).sum();
            if tail > 0 {
                end += 1 + tail;
                while iter.peek().is_some_and(|&(at, _)| at < end) {
                    iter.next();
                }
            }
        }
        found.push(&s[start..end]);
    }
    found
}

/// File-extension tokens the track search ignores (track_repository.py).
pub const SEARCH_TOKEN_SKIP: [&str; 8] = ["mp3", "wav", "aiff", "aif", "flac", "m4a", "aac", "ogg"];

/// The multi-token part of the track search: tokens of `q.strip()` whose
/// `lower()` is not a file extension.
pub fn search_tokens(q: &str) -> Vec<&str> {
    tokens(py_strip(q))
        .into_iter()
        .filter(|token| !SEARCH_TOKEN_SKIP.contains(&py_lower(token).as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_whitespace_and_folding() {
        assert_eq!(py_split("\x1ca\u{3000}b\u{85}"), ["a", "b"]);
        assert_eq!(py_strip("\u{a0} x \x1f"), "x");
        assert_eq!(casefold("Straße"), "strasse");
        assert_eq!(py_lower("ΟΔΟΣ"), "οδος");
        assert_eq!(normalize_text(Some("  Ａｂｃ　ＤＥＦ ")), "abc def");
        assert_eq!(normalize_text(None), "");
    }

    #[test]
    fn tokenizer_matches_python_regex() {
        assert_eq!(tokens("rock'n'roll don't"), ["rock'n", "roll", "don't"]);
        assert_eq!(tokens("cafe\u{301} a_b"), ["cafe", "a", "b"]);
        assert_eq!(search_tokens(" track01 MP3 Flac "), ["track01"]);
    }
}
