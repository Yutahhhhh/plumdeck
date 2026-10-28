//! Genre label rules of backend/app/services/genre_app_service.py and
//! backend/domain/constants.py.

use plumdeck_core::compat::text::{py_lower, py_strip};

use crate::pychars::{is_py_whitespace, is_word_boundary};

/// `GENRE_ALIASES`.
pub const GENRE_ALIASES: [(&str, &str); 16] = [
    ("hip hop", "Hip-Hop"),
    ("hip-hop", "Hip-Hop"),
    ("rap", "Hip-Hop"),
    ("rnb", "R&B"),
    ("r&b", "R&B"),
    ("r and b", "R&B"),
    ("afrobeat", "Afrobeats"),
    ("afrobeats", "Afrobeats"),
    ("afro beats", "Afrobeats"),
    ("amapiano", "Amapiano"),
    ("reggae", "Reggae"),
    ("dancehall", "Dancehall"),
    ("reggaeton", "Reggaeton"),
    ("latin", "Latin"),
    ("latin pop", "Latin"),
    ("pop", "Pop"),
];

/// `SUBGENRE_ALIASES`.
pub const SUBGENRE_ALIASES: [(&str, &str); 8] = [
    ("contemporary r&b", "Contemporary R&B"),
    ("contemporary rnb", "Contemporary R&B"),
    ("crunk&b", "Crunk&B"),
    ("crunk b", "Crunk&B"),
    ("trap soul", "Trap Soul"),
    ("afro pop", "Afropop"),
    ("afropop", "Afropop"),
    ("popiano", "Popiano"),
];

fn alias(table: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(from, _)| *from == key).map(|(_, to)| *to)
}

fn is_quote(c: char) -> bool {
    c == '"' || c == '\''
}

/// `re.sub(r'^[\"\']|[\"\']$', '', value)`: one quote at the start and one
/// at the end, where `$` also matches before a final newline.
fn strip_one_quote_each_side(value: &str) -> String {
    let mut text = value;
    let mut out = String::new();
    if text.starts_with(is_quote) {
        text = &text[1..];
    }
    // `$` after the quote: the end of the string, or a final "\n".
    let (body, tail) = match text.strip_suffix('\n') {
        Some(body) if body.ends_with(is_quote) => (&body[..body.len() - 1], "\n"),
        _ if text.ends_with(is_quote) => (&text[..text.len() - 1], ""),
        _ => (text, ""),
    };
    out.push_str(body);
    out.push_str(tail);
    out
}

/// `re.sub(r'\s+', ' ', label)`.
fn collapse_whitespace_runs(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for c in value.chars() {
        if is_py_whitespace(c) {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// `GenreAppService._sanitize_label`.
pub fn sanitize_label(value: &str) -> String {
    let quoted = strip_one_quote_each_side(value);
    let mut label = collapse_whitespace_runs(py_strip(&quoted));
    if let Some((first, _)) = label.split_once('/') {
        label = py_strip(first).to_owned();
    }
    if let Some((first, _)) = label.split_once(',') {
        label = py_strip(first).to_owned();
    }
    label
}

/// `{g.lower(): g for g in labels}`: the last label of each lowercase form.
pub fn label_map(labels: &[String]) -> Vec<(String, String)> {
    let mut map: Vec<(String, String)> = Vec::new();
    for label in labels {
        let key = py_lower(label);
        match map.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = label.clone(),
            None => map.push((key, label.clone())),
        }
    }
    map
}

fn lookup<'a>(map: &'a [(String, String)], key: &str) -> Option<&'a str> {
    map.iter().find(|(existing, _)| existing == key).map(|(_, label)| label.as_str())
}

/// `_normalize_genre_label`: reuse the library's spelling, else an alias.
pub fn normalize_genre_label(value: &str, existing: &[(String, String)]) -> String {
    let label = sanitize_label(value);
    if label.is_empty() {
        return "Unknown".into();
    }
    let lowered = py_lower(&label);
    if let Some(found) = lookup(existing, &lowered).filter(|found| !found.is_empty()) {
        return found.to_owned();
    }
    alias(&GENRE_ALIASES, &lowered).map_or(label, str::to_owned)
}

/// `_normalize_subgenre_label`.
pub fn normalize_subgenre_label(value: &str, existing: &[(String, String)]) -> String {
    let label = sanitize_label(value);
    if label.is_empty() {
        return String::new();
    }
    let lowered = py_lower(&label);
    if let Some(found) = lookup(existing, &lowered).filter(|found| !found.is_empty()) {
        return found.to_owned();
    }
    alias(&SUBGENRE_ALIASES, &lowered).map_or(label, str::to_owned)
}

/// `str(confidence).strip().title()` is one of `High` / `Medium`: anything
/// else counts as `Low`. Python's `title()` gives those words only for their
/// ASCII case variants.
pub fn confidence_is_not_low(confidence: &str) -> bool {
    let stripped = py_strip(confidence);
    stripped.eq_ignore_ascii_case("high") || stripped.eq_ignore_ascii_case("medium")
}

/// `re.sub(pattern, replacement, s)` for a pattern of literal letters
/// separated by `['\s]*`, between `\b`s: `\bdnb\b`, `\br['\s]*n['\s]*b\b`,
/// `\brock['\s]*n['\s]*roll\b`.
fn replace_spelled(s: &str, parts: &[&str], gaps: bool, replacement: &str) -> String {
    let gap = |c: char| c == '\'' || is_py_whitespace(c);
    let try_match = |start: usize| -> Option<usize> {
        if !is_word_boundary(s, start) {
            return None;
        }
        let mut at = start;
        for (index, part) in parts.iter().enumerate() {
            if index > 0 && gaps {
                // Greedy, but backtrack is never needed: a gap character
                // cannot start a part.
                at += s[at..].chars().take_while(|&c| gap(c)).map(char::len_utf8).sum::<usize>();
            }
            if !s[at..].starts_with(part) {
                return None;
            }
            at += part.len();
        }
        is_word_boundary(s, at).then_some(at)
    };
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    while at < s.len() {
        if let Some(end) = try_match(at) {
            out.push_str(replacement);
            at = end;
            continue;
        }
        let c = s[at..].chars().next().expect("char");
        out.push(c);
        at += c.len_utf8();
    }
    out
}

fn is_separator(c: char) -> bool {
    is_py_whitespace(c) || matches!(c, '-' | '.' | '/' | '_' | ',')
}

/// `normalize_genre` inside `get_cleanup_suggestions`: abbreviations
/// spelled out, `&` as "and", separators dropped, tokens sorted.
pub fn cleanup_key(genre: &str) -> String {
    let mut s = py_lower(genre);
    s = replace_spelled(&s, &["dnb"], false, "drum and bass");
    s = replace_spelled(&s, &["r", "n", "b"], true, "r and b");
    s = replace_spelled(&s, &["rock", "n", "roll"], true, "rock and roll");
    let s = s.replace('&', " and ");
    let mut tokens: Vec<&str> = s.split(is_separator).filter(|token| !token.is_empty()).collect();
    tokens.sort_unstable();
    tokens.concat()
}

/// `re.search(r'\band\b', k.lower())`.
fn has_and_word(label: &str) -> bool {
    let lowered = py_lower(label);
    lowered
        .match_indices("and")
        .any(|(at, word)| is_word_boundary(&lowered, at) && is_word_boundary(&lowered, at + word.len()))
}

/// The primary spelling of a variant group: the sort key of
/// `sorted_variants` (`&` first, no " and ", most tracks, shortest, then
/// code point order). `variants` is `(label, track count)`.
pub fn primary_variant<'a>(variants: &[(&'a str, usize)]) -> &'a str {
    variants
        .iter()
        .min_by(|(a, na), (b, nb)| {
            let key = |label: &str, count: usize| {
                (!label.contains('&'), has_and_word(label), std::cmp::Reverse(count), label.chars().count())
            };
            key(a, *na).cmp(&key(b, *nb)).then_with(|| a.cmp(b))
        })
        .map(|(label, _)| *label)
        .expect("a group has variants")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_python() {
        assert_eq!(sanitize_label("\"House\""), "House");
        assert_eq!(sanitize_label("'x\"\n"), "x");
        assert_eq!(sanitize_label("\""), "");
        assert_eq!(sanitize_label("  Deep   House / Tech , x"), "Deep House");
        assert_eq!(sanitize_label("a,b/c"), "a");
        assert_eq!(sanitize_label("\x1cHip\u{3000}Hop\t"), "Hip Hop");
    }

    #[test]
    fn labels_reuse_the_library_and_aliases() {
        let existing = label_map(&["House".into(), "house".into(), "R&B".into()]);
        assert_eq!(normalize_genre_label("HOUSE", &existing), "house");
        assert_eq!(normalize_genre_label("rap", &existing), "Hip-Hop");
        assert_eq!(normalize_genre_label(" ", &existing), "Unknown");
        assert_eq!(normalize_genre_label("Kawaii", &existing), "Kawaii");
        assert_eq!(normalize_subgenre_label("trap soul", &[]), "Trap Soul");
        assert_eq!(normalize_subgenre_label("", &[]), "");
        assert!(confidence_is_not_low(" high\n") && confidence_is_not_low("MEDIUM"));
        assert!(!confidence_is_not_low("Low") && !confidence_is_not_low("very high"));
    }

    #[test]
    fn cleanup_keys_group_spellings() {
        assert_eq!(cleanup_key("Drum & Bass"), cleanup_key("drum-and-bass"));
        assert_eq!(cleanup_key("DnB"), cleanup_key("Drum and Bass"));
        assert_eq!(cleanup_key("R'n'B"), "andbr");
        assert_eq!(cleanup_key("rock n roll"), "androckroll");
        assert_eq!(cleanup_key("dnbx"), "dnbx");
        assert_eq!(cleanup_key("..."), "");
        let primary = primary_variant(&[("drum and bass", 3), ("Drum & Bass", 1), ("Drum&Bass", 1)]);
        assert_eq!(primary, "Drum&Bass");
    }
}
