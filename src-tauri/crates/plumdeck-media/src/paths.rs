//! `resolve_path` (backend/utils/filesystem.py).

use std::path::Path;

use plumdeck_core::compat::text::{nfc, nfd};

/// `os.path.exists`: follows symlinks; any error (a NUL byte, a permission
/// problem) counts as missing.
fn exists(path: &str) -> bool {
    !path.contains('\0') && Path::new(path).metadata().is_ok()
}

/// The path as given if it exists, else its NFC form, else its NFD form
/// (macOS stores NFD names, Linux and the web send NFC).
pub fn resolve_path(path: &str) -> Option<String> {
    if exists(path) {
        return Some(path.to_owned());
    }
    let composed = nfc(path);
    if exists(&composed) {
        return Some(composed);
    }
    let decomposed = nfd(path);
    exists(&decomposed).then_some(decomposed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_nfc_then_nfd() {
        let dir = std::env::temp_dir().join(format!("plumdeck-media-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let nfd_name = dir.join("cafe\u{301}.mp3");
        std::fs::write(&nfd_name, b"x").unwrap();
        let nfc_query = dir.join("caf\u{e9}.mp3").to_string_lossy().into_owned();
        assert_eq!(resolve_path(&nfc_query).as_deref(), nfd_name.to_str());
        assert_eq!(resolve_path(nfd_name.to_str().unwrap()).as_deref(), nfd_name.to_str());
        assert_eq!(resolve_path(&dir.join("missing.mp3").to_string_lossy()), None);
        assert_eq!(resolve_path("bad\0path"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
