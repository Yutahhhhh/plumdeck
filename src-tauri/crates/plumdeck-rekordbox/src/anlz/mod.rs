//! rekordbox analysis files (`ANLZ0000.DAT`, `.EXT`, `.2EX`; `.3EX` tolerated).
//!
//! Written from Deep Symmetry's format description
//! (<https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>)
//! and checks against real files. All numbers are big-endian. A file is
//! `PMAI`, `len_header`, `len_file`, then tagged sections, each
//! `fourcc`, `len_header`, `len_tag`, body. Tag fields are read at their
//! documented offsets from the tag start (the docs advise ignoring
//! `len_header`), and every read is bounds-checked.
//!
//! Failure policy (unlike strict parsers, one odd value never loses a file):
//! - a bad file header is an [`AnlzError`];
//! - a broken section envelope stops the walk, keeps the tags read so far and
//!   records an [`Issue`];
//! - a section whose body does not parse becomes [`Tag::Invalid`];
//! - unknown sections (PQT2 variants, `.3EX` content, future tags) become
//!   [`Tag::Unknown`] and are skipped.

mod bytes;
pub mod phrase;
pub mod tags;

use std::io::Read;
use std::path::Path;

pub use phrase::{Bank, Mood, Phrase, SongStructure};
pub use tags::*;

use bytes::{fourcc_at, u32_at};

/// Files above this size are refused unread.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const FILE_MAGIC: [u8; 4] = *b"PMAI";
const ENVELOPE: usize = 12;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnlzError {
    #[error("not an ANLZ file")]
    NotAnlz,
    #[error("invalid ANLZ file header")]
    BadHeader,
    #[error("ANLZ file is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge,
    #[error("could not read the ANLZ file: {0}")]
    Io(String),
}

/// Something wrong with the file envelope that did not stop parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// `len_file` disagrees with the bytes present; the smaller one was used.
    FileLengthMismatch { declared: u32, actual: usize },
    /// A section envelope at `offset` was unreadable; the walk stopped there.
    BrokenSection { offset: usize },
}

/// A parsed analysis file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AnlzFile {
    pub tags: Vec<Tag>,
    pub issues: Vec<Issue>,
}

impl AnlzFile {
    /// Parses a whole file held in memory.
    pub fn parse(data: &[u8]) -> Result<Self, AnlzError> {
        if data.len() as u64 > MAX_FILE_BYTES {
            return Err(AnlzError::TooLarge);
        }
        if fourcc_at(data, 0) != Some(FILE_MAGIC) {
            return Err(AnlzError::NotAnlz);
        }
        let len_header = u32_at(data, 4).ok_or(AnlzError::BadHeader)? as usize;
        let len_file = u32_at(data, 8).ok_or(AnlzError::BadHeader)?;
        if len_header < ENVELOPE || len_header > data.len() {
            return Err(AnlzError::BadHeader);
        }
        let mut file = AnlzFile::default();
        let end = if len_file as usize == data.len() {
            data.len()
        } else {
            file.issues.push(Issue::FileLengthMismatch { declared: len_file, actual: data.len() });
            data.len().min(len_file as usize)
        };
        let mut offset = len_header;
        while offset < end {
            let Some((fourcc, len_tag)) = envelope(data, offset, end) else {
                file.issues.push(Issue::BrokenSection { offset });
                break;
            };
            // `envelope` guarantees offset + len_tag <= end and len_tag >= 12.
            let Some(section) = data.get(offset..offset + len_tag) else {
                file.issues.push(Issue::BrokenSection { offset });
                break;
            };
            file.tags.push(tags::parse_tag(fourcc, section));
            offset += len_tag;
        }
        Ok(file)
    }

    /// Reads and parses a file, opened read-only, refusing files over
    /// [`MAX_FILE_BYTES`].
    pub fn read(path: &Path) -> Result<Self, AnlzError> {
        let file = std::fs::File::open(path).map_err(|e| AnlzError::Io(e.kind().to_string()))?;
        let mut data = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|e| AnlzError::Io(e.kind().to_string()))?;
        Self::parse(&data)
    }

    /// The first tag matching `pick`.
    pub fn find<'a, T>(&'a self, pick: impl FnMut(&'a Tag) -> Option<T>) -> Option<T> {
        self.tags.iter().find_map(pick)
    }

    pub fn beat_grid(&self) -> Option<&BeatGrid> {
        self.find(|t| match t {
            Tag::BeatGrid(v) => Some(v),
            _ => None,
        })
    }

    pub fn extended_beat_grid(&self) -> Option<&ExtendedBeatGrid> {
        self.find(|t| match t {
            Tag::ExtendedBeatGrid(v) => Some(v),
            _ => None,
        })
    }

    /// Both PCOB lists (memory points and hot cues) in file order.
    pub fn cue_lists(&self) -> impl Iterator<Item = &CueList> {
        self.tags.iter().filter_map(|t| match t {
            Tag::CueList(v) => Some(v),
            _ => None,
        })
    }

    /// Both PCO2 lists in file order.
    pub fn extended_cue_lists(&self) -> impl Iterator<Item = &ExtendedCueList> {
        self.tags.iter().filter_map(|t| match t {
            Tag::ExtendedCueList(v) => Some(v),
            _ => None,
        })
    }

    pub fn path(&self) -> Option<&str> {
        self.find(|t| match t {
            Tag::Path(v) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn vbr(&self) -> Option<&VbrIndex> {
        self.find(|t| match t {
            Tag::Vbr(v) => Some(v),
            _ => None,
        })
    }

    /// The waveform of the given kind (PWAV, PWV2–PWV7).
    pub fn waveform(&self, kind: WaveformKind) -> Option<&Waveform> {
        self.find(|t| match t {
            Tag::Waveform(v) if v.kind == kind => Some(v),
            _ => None,
        })
    }

    pub fn waveform_calibration(&self) -> Option<&WaveformCalibration> {
        self.find(|t| match t {
            Tag::WaveformCalibration(v) => Some(v),
            _ => None,
        })
    }

    pub fn song_structure(&self) -> Option<&SongStructure> {
        self.find(|t| match t {
            Tag::SongStructure(v) => Some(v),
            _ => None,
        })
    }

    /// Sections that failed to parse, as `(fourcc, reason)`.
    pub fn invalid_tags(&self) -> impl Iterator<Item = (&str, &'static str)> {
        self.tags.iter().filter_map(|t| match t {
            Tag::Invalid { fourcc, reason } => Some((std::str::from_utf8(fourcc).unwrap_or("????"), *reason)),
            _ => None,
        })
    }
}

/// Whether a PPTH value plausibly names `audio_path`, to catch an analysis
/// file attached to the wrong track.
///
/// Local library analyses store only `?/<file name>` (checked on a real
/// library: the folder is not recorded), so for that form the NFC file names
/// are compared. Other forms (exported media) must equal the NFC path or be a
/// `/`-aligned suffix of it.
pub fn ppth_matches(ppth: &str, audio_path: &str) -> bool {
    use plumdeck_core::compat::text::nfc;
    let audio = nfc(&audio_path.replace('\\', "/"));
    if let Some(name) = ppth.strip_prefix("?/") {
        let file_name = audio.rsplit('/').next().unwrap_or(&audio);
        return !name.is_empty() && nfc(name) == file_name;
    }
    let ppth = nfc(&ppth.replace('\\', "/"));
    if ppth.is_empty() {
        return false;
    }
    audio == ppth
        || (ppth.starts_with('/') && audio.ends_with(&ppth))
        || audio.strip_suffix(&ppth).is_some_and(|head| head.ends_with('/'))
}

/// `(fourcc, len_tag)` of the section at `offset`, when its envelope is sane
/// and the whole section lies before `end`.
fn envelope(data: &[u8], offset: usize, end: usize) -> Option<([u8; 4], usize)> {
    let fourcc = fourcc_at(data, offset)?;
    let len_header = u32_at(data, offset + 4)? as usize;
    let len_tag = u32_at(data, offset + 8)? as usize;
    let fits = offset.checked_add(len_tag).is_some_and(|stop| stop <= end);
    (len_header >= ENVELOPE && len_header <= len_tag && fits).then_some((fourcc, len_tag))
}
