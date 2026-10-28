//! Python path spelling rules used for file identity: `PurePath` parsing and
//! `as_uri()`, `ntpath.splitroot`/`normpath`/`normcase`, and the rekordbox
//! `normalize_path` / `path_variants` helpers.
//!
//! These are pure string functions, so both flavours are available on every
//! platform. `Path.resolve()` itself touches the filesystem and is not here;
//! resolve first, then call [`file_uri`].

use super::text::{nfc, nfd, py_lower};

/// `urllib.parse.quote_from_bytes(s.encode("utf-8"), safe="/")`.
pub fn quote_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `posixpath.splitroot`.
pub fn posix_splitroot(p: &str) -> (&str, &str, &str) {
    if !p.starts_with('/') {
        ("", "", p)
    } else if !p[1..].starts_with('/') || p[2..].starts_with('/') {
        ("", "/", &p[1..])
    } else {
        ("", "//", &p[2..])
    }
}

/// Byte offset just past the first `n` characters (or the string length).
fn char_offset(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(at, _)| at)
}

/// `ntpath.splitroot` (Python 3.12).
pub fn nt_splitroot(p: &str) -> (&str, &str, &str) {
    let normp = p.replace('/', "\\");
    let b = normp.as_bytes();
    if b.first() == Some(&b'\\') {
        if b.get(1) == Some(&b'\\') {
            // UNC (\\server\share, \\?\UNC\server\share) or device (\\.\dev, \\?\dev).
            let start = if b.len() >= 8 && b[..8].eq_ignore_ascii_case(b"\\\\?\\UNC\\") { 8 } else { 2 };
            let Some(index) = normp[start..].find('\\').map(|i| i + start) else {
                return (p, "", "");
            };
            let Some(index2) = normp[index + 1..].find('\\').map(|i| i + index + 1) else {
                return (p, "", "");
            };
            (&p[..index2], &p[index2..index2 + 1], &p[index2 + 1..])
        } else {
            ("", &p[..1], &p[1..])
        }
    } else {
        // Python compares code points: the drive is the first two characters.
        let second = char_offset(p, 1);
        if normp[second..].starts_with(':') {
            let drive_end = second + 1;
            if normp[drive_end..].starts_with('\\') {
                (&p[..drive_end], &p[drive_end..drive_end + 1], &p[drive_end + 1..])
            } else {
                (&p[..drive_end], "", &p[drive_end..])
            }
        } else {
            ("", "", p)
        }
    }
}

/// `ntpath.normpath` (the pure-Python implementation of 3.12).
pub fn nt_normpath(path: &str) -> String {
    let path = path.replace('/', "\\");
    let (drive, root, rest) = nt_splitroot(&path);
    let prefix = format!("{drive}{root}");
    let mut comps: Vec<&str> = rest.split('\\').collect();
    let mut i = 0;
    while i < comps.len() {
        if comps[i].is_empty() || comps[i] == "." {
            comps.remove(i);
        } else if comps[i] == ".." {
            if i > 0 && comps[i - 1] != ".." {
                comps.drain(i - 1..=i);
                i -= 1;
            } else if i == 0 && !root.is_empty() {
                comps.remove(i);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    if prefix.is_empty() && comps.is_empty() {
        comps.push(".");
    }
    prefix + &comps.join("\\")
}

/// `ntpath.normcase` as Python runs it off Windows: `/` to `\`, then
/// `str.lower()`. On Windows CPython lowercases with `LCMapStringEx`
/// (invariant locale) instead; they differ only for a few special letters.
pub fn nt_normcase(s: &str) -> String {
    py_lower(&s.replace('/', "\\"))
}

/// A `PurePath`'s parsed (drive, root, parts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PureParts {
    pub drive: String,
    pub root: String,
    pub parts: Vec<String>,
    windows: bool,
}

impl PureParts {
    /// `PurePosixPath(p)`.
    pub fn posix(p: &str) -> Self {
        let (drive, root, rest) = posix_splitroot(p);
        Self::from_split(drive, root, rest, '/', false)
    }

    /// `PureWindowsPath(p)`.
    pub fn windows(p: &str) -> Self {
        let p = p.replace('/', "\\");
        let (drive, mut root, rest) = nt_splitroot(&p);
        if root.is_empty() && drive.starts_with('\\') && !drive.ends_with('\\') {
            let drive_parts: Vec<&str> = drive.split('\\').collect();
            // `drv_parts[2] not in '?.'` is a substring test in Python.
            if (drive_parts.len() == 4 && !"?.".contains(drive_parts[2])) || drive_parts.len() == 6 {
                root = "\\";
            }
        }
        Self::from_split(drive, root, rest, '\\', true)
    }

    fn from_split(drive: &str, root: &str, rest: &str, sep: char, windows: bool) -> Self {
        let parts = rest.split(sep).filter(|x| !x.is_empty() && *x != ".").map(str::to_owned).collect();
        Self { drive: drive.to_owned(), root: root.to_owned(), parts, windows }
    }

    fn sep(&self) -> &'static str {
        if self.windows { "\\" } else { "/" }
    }

    /// `str(path)`.
    pub fn to_str(&self) -> String {
        let joined = self.parts.join(self.sep());
        let text = if !self.drive.is_empty() || !self.root.is_empty() {
            format!("{}{}{}", self.drive, self.root, joined)
        } else if self.windows
            && self.parts.first().is_some_and(|first| !nt_splitroot(first).0.is_empty())
        {
            format!(".\\{joined}")
        } else {
            joined
        };
        if text.is_empty() { ".".into() } else { text }
    }

    /// `path.is_absolute()`.
    pub fn is_absolute(&self) -> bool {
        if self.windows { !self.drive.is_empty() && !self.root.is_empty() } else { !self.root.is_empty() }
    }

    /// `path.as_uri()`; `None` where Python raises (relative paths).
    pub fn as_uri(&self) -> Option<String> {
        if !self.is_absolute() {
            return None;
        }
        let text = self.to_str();
        if !self.windows {
            return Some(format!("file://{}", quote_path(&text)));
        }
        let posix = text.replace('\\', "/");
        let mut drive_chars = self.drive.chars();
        if self.drive.chars().count() == 2 && drive_chars.nth(1) == Some(':') {
            let rest = &posix[self.drive.len()..];
            Some(format!("file:///{}{}", self.drive, quote_path(rest)))
        } else {
            Some(format!("file:{}", quote_path(&posix)))
        }
    }
}

/// `PurePosixPath(p).as_uri()`.
pub fn posix_file_uri(p: &str) -> Option<String> {
    PureParts::posix(p).as_uri()
}

/// `PureWindowsPath(p).as_uri()`.
pub fn windows_file_uri(p: &str) -> Option<String> {
    PureParts::windows(p).as_uri()
}

/// `Path(p).as_uri()` for the running platform (pass an already resolved path).
pub fn file_uri(p: &str) -> Option<String> {
    if cfg!(windows) { windows_file_uri(p) } else { posix_file_uri(p) }
}

/// rekordbox_library.normalize_path: NFC, plus `ntpath.normpath` on Windows.
pub fn normalize_path(filepath: &str, windows: bool) -> String {
    if windows { nfc(&nt_normpath(filepath)) } else { nfc(filepath) }
}

/// rekordbox_library.path_variants: the observed spelling first, then NFC and
/// NFD, plus slash variants on Windows; duplicates removed in order.
pub fn path_variants(filepath: &str, windows: bool) -> Vec<String> {
    if filepath.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![filepath.to_owned(), nfc(filepath), nfd(filepath)];
    if windows {
        let slashed: Vec<String> = variants.iter().map(|v| v.replace('\\', "/")).collect();
        variants.extend(slashed);
        let backslashed: Vec<String> = variants.iter().map(|v| v.replace('/', "\\")).collect();
        variants.extend(backslashed);
    }
    let mut seen = std::collections::HashSet::new();
    variants.retain(|v| seen.insert(v.clone()));
    variants
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_quotes_everything_but_slash_and_unreserved() {
        assert_eq!(
            posix_file_uri("/tmp/日本語 #%&.wav").unwrap(),
            "file:///tmp/%E6%97%A5%E6%9C%AC%E8%AA%9E%20%23%25%26.wav"
        );
        assert_eq!(windows_file_uri("C:\\a b\\c.mp3").unwrap(), "file:///C:/a%20b/c.mp3");
        assert_eq!(windows_file_uri("\\\\srv\\share\\x").unwrap(), "file://srv/share/x");
        assert_eq!(posix_file_uri("relative"), None);
    }

    #[test]
    fn nt_normpath_collapses() {
        assert_eq!(nt_normpath("C:/a/../b/./c//"), "C:\\b\\c");
        assert_eq!(nt_normpath("..\\x"), "..\\x");
        assert_eq!(nt_normpath(""), ".");
    }
}
