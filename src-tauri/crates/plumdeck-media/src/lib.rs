//! Audio file access for the backend (no DuckDB):
//!
//! - [`paths`]: `resolve_path` (backend/utils/filesystem.py), the NFC/NFD
//!   fallback for paths written on another OS.
//! - [`mime`]: the Content-Type Starlette's FileResponse sends.
//! - [`tags`]: the tag fields of `extract_full_metadata`
//!   (backend/utils/metadata.py), read with lofty instead of TinyTag.
//! - [`timing`]: MP3/M4A header facts for the rekordbox timing shift (S5).

pub mod mime;
pub mod paths;
pub mod tags;
pub mod timing;
