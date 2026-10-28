//! Python `re` character classes the ported patterns use, for patterns that
//! are matched by hand instead of with the regex crate (whose `\w` and `\b`
//! include combining marks and connector punctuation that Python's do not).

use std::sync::LazyLock;

use plumdeck_core::compat::text::{is_py_space, is_word_char};
use regex::Regex;

/// Python `\w`: `str.isalnum()` or `_`.
pub fn is_py_word(c: char) -> bool {
    c == '_' || is_word_char(c)
}

/// Python `\s` (`str.isspace()`).
pub fn is_py_whitespace(c: char) -> bool {
    is_py_space(c)
}

static DECIMAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("decimal class"));

/// Python `\d` (Unicode category Nd).
pub fn is_py_decimal(c: char) -> bool {
    let mut buf = [0u8; 4];
    DECIMAL.is_match(c.encode_utf8(&mut buf))
}

/// The value of a decimal digit (`int('٣') == 3`). Unicode encodes every Nd
/// digit set as ten consecutive code points starting at zero, so the value
/// is the distance from the start of its run, modulo ten.
pub fn decimal_value(c: char) -> Option<u32> {
    if !is_py_decimal(c) {
        return None;
    }
    let code = c as u32;
    let mut start = code;
    while start > 0 && char::from_u32(start - 1).is_some_and(is_py_decimal) {
        start -= 1;
    }
    Some((code - start) % 10)
}

/// The text with every decimal digit replaced by its ASCII digit, as
/// `int()` and `float()` read them.
pub fn ascii_digits(text: &str) -> String {
    text.chars()
        .map(|c| match decimal_value(c) {
            Some(value) if !c.is_ascii_digit() => char::from_digit(value, 10).expect("digit"),
            _ => c,
        })
        .collect()
}

/// Python `\b` at byte offset `at` of `text`.
pub fn is_word_boundary(text: &str, at: usize) -> bool {
    let before = text[..at].chars().next_back().is_some_and(is_py_word);
    let after = text[at..].chars().next().is_some_and(is_py_word);
    before != after
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_follow_python() {
        assert!(is_py_word('_') && is_py_word('é') && is_py_word('²') && !is_py_word('\u{301}'));
        assert!(is_py_whitespace('\x1c') && !is_py_whitespace('x'));
        assert_eq!(decimal_value('٣'), Some(3));
        assert_eq!(decimal_value('１'), Some(1));
        assert_eq!(decimal_value('𝟗'), Some(9));
        assert_eq!(decimal_value('²'), None);
        assert_eq!(ascii_digits("１２:٣٤.5"), "12:34.5");
        assert!(is_word_boundary("a b", 1) && !is_word_boundary("ab", 1) && is_word_boundary("ab", 0));
    }
}
