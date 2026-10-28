//! File identity rules shared with `backend/infra/rekordbox_library.py`.
//!
//! master.db stores paths exactly as rekordbox saw them. macOS hands out
//! decomposed (NFD) names while tags, imports and `lsof` may carry the
//! composed (NFC) spelling of the same file, so lookups query every plausible
//! spelling and then prove identity on the filesystem. Case is never folded on
//! POSIX: on a case-sensitive volume two spellings really are two files.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use plumdeck_core::compat::pathnorm;
pub use plumdeck_core::compat::text::normalize_text;

/// Whether the running platform uses Windows path rules.
pub const WINDOWS: bool = cfg!(windows);

/// `normalize_path`: NFC, plus `ntpath.normpath` on Windows.
pub fn normalize_path(filepath: &str) -> String {
    pathnorm::normalize_path(filepath, WINDOWS)
}

/// `path_variants`: the observed spelling first, then NFC and NFD (and slash
/// variants on Windows), duplicates removed in order.
pub fn path_variants(filepath: &str) -> Vec<String> {
    pathnorm::path_variants(filepath, WINDOWS)
}

/// `same_path`: equal strings, or two spellings that the filesystem says are
/// the same file. On POSIX the spellings must also agree after NFC.
pub fn same_path(left: &str, right: &str) -> bool {
    same_path_with(left, right, WINDOWS)
}

pub(crate) fn same_path_with(left: &str, right: &str, windows: bool) -> bool {
    if left == right {
        return true;
    }
    if !windows && pathnorm::normalize_path(left, false) != pathnorm::normalize_path(right, false) {
        return false;
    }
    same_file(Path::new(left), Path::new(right))
}

/// `os.path.samefile`: device and inode on Unix; elsewhere the canonical
/// paths (std has no stable file index on Windows).
pub fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(left), std::fs::metadata(right)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// The collection's path set, indexed the way `is_registered_path` needs.
#[derive(Debug, Clone, Default)]
pub struct RegisteredPaths {
    paths: HashSet<String>,
    /// Windows only: `normcase(normalize_path(p))` → spellings.
    folded: HashMap<String, Vec<String>>,
}

impl RegisteredPaths {
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let paths: HashSet<String> = paths.into_iter().filter(|p| !p.is_empty()).collect();
        let mut folded: HashMap<String, Vec<String>> = HashMap::new();
        if WINDOWS {
            for path in &paths {
                folded.entry(windows_fold(path)).or_default().push(path.clone());
            }
        }
        Self { paths, folded }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains_exact(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.paths.iter()
    }

    /// `is_registered_path`: whether `filepath` is a file rekordbox knows.
    pub fn contains(&self, filepath: &str) -> bool {
        if WINDOWS {
            // A case-insensitive candidate must still prove filesystem
            // identity: Windows directories can be case-sensitive too.
            return self
                .folded
                .get(&windows_fold(filepath))
                .is_some_and(|candidates| candidates.iter().any(|p| same_path(filepath, p)));
        }
        path_variants(filepath)
            .iter()
            .any(|variant| self.paths.contains(variant) && same_path(filepath, variant))
    }
}

fn windows_fold(path: &str) -> String {
    pathnorm::nt_normcase(&pathnorm::normalize_path(path, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_rejects_different_names_without_touching_disk() {
        assert!(same_path_with("/a/b.mp3", "/a/b.mp3", false));
        assert!(!same_path_with("/nope/a.mp3", "/nope/b.mp3", false));
        // Same NFC form but the files do not exist: not provably the same.
        assert!(!same_path_with("/nope/Cafe\u{301}.mp3", "/nope/Caf\u{e9}.mp3", false));
    }

    #[test]
    fn same_path_accepts_nfd_spelling_of_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("plumdeck-rb-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let composed = dir.join("Caf\u{e9}.txt");
        std::fs::write(&composed, b"x").unwrap();
        let decomposed = dir.join("Cafe\u{301}.txt");
        let composed_s = composed.to_str().unwrap();
        let decomposed_s = decomposed.to_str().unwrap();
        // APFS/HFS+ resolve both spellings to one file; other filesystems may
        // not, in which case the second spelling simply does not exist.
        let expected = decomposed.exists();
        assert_eq!(same_path_with(composed_s, decomposed_s, false), expected);
        let registered = RegisteredPaths::new([decomposed_s.to_owned()]);
        if !WINDOWS {
            assert_eq!(registered.contains(composed_s), expected);
            assert!(!registered.contains(&format!("{composed_s}.other")));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn normalize_text_folds_case_width_and_space() {
        assert_eq!(normalize_text(Some("  ＡＢＣ   Def ")), "abc def");
        assert_eq!(normalize_text(None), "");
    }
}
