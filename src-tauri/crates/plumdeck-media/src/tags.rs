//! The tag part of `extract_full_metadata` (backend/utils/metadata.py),
//! which used TinyTag 2.x: title, artist, album, `lyrics` and the cover as
//! base64.
//!
//! Bug-compatible details of the Python code:
//! - `lyrics` is always `""`: it read `tag.extra.get('lyrics', '')`, but
//!   TinyTag 2 keeps lyrics in `tag.other` and `extra` is always empty.
//! - An empty text value counts as missing (TinyTag skips empty fields).
//! - MP3: ID3v1 at the end fills fields ID3v2 left empty.
//! - The cover is TinyTag's `images.any`: the first front cover, else back
//!   cover, else media picture, else the first other picture.
//! - Any read failure gives every field empty (`None` and `lyrics` `""`).

use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use lofty::config::ParseOptions;
use lofty::file::{FileType, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, Tag, TagType};

/// The fields of `extract_full_metadata`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Always `""` (see the module docs).
    pub lyrics: String,
    /// The cover image, base64 (standard alphabet, padded).
    pub artwork: Option<String>,
}

fn non_empty(value: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    value.map(|text| text.into_owned()).filter(|text| !text.is_empty())
}

/// TinyTag's `images.any` over the pictures in tag order.
fn cover<'a>(pictures: impl Iterator<Item = &'a Picture> + Clone) -> Option<&'a Picture> {
    for kind in [PictureType::CoverFront, PictureType::CoverBack, PictureType::Media] {
        if let Some(picture) = pictures.clone().find(|picture| picture.pic_type() == kind) {
            return Some(picture);
        }
    }
    pictures.clone().next()
}

/// Read the tags of `path`; never fails (TinyTag errors were swallowed).
pub fn extract_full_metadata(path: &Path) -> TagMetadata {
    read(path).unwrap_or_default()
}

fn read(path: &Path) -> Option<TagMetadata> {
    let tagged = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let mp3 = tagged.file_type() == FileType::Mpeg;
    // The tags TinyTag reads for the container, in its order of precedence.
    let order: &[TagType] = match tagged.file_type() {
        FileType::Mpeg => &[TagType::Id3v2, TagType::Id3v1],
        FileType::Flac => &[TagType::VorbisComments, TagType::Id3v2],
        FileType::Mp4 => &[TagType::Mp4Ilst],
        FileType::Wav => &[TagType::RiffInfo, TagType::Id3v2],
        FileType::Aiff => &[TagType::Id3v2, TagType::AiffText],
        FileType::Vorbis | FileType::Opus | FileType::Speex => &[TagType::VorbisComments],
        _ => &[],
    };
    let tags: Vec<&Tag> = order.iter().filter_map(|kind| tagged.tag(*kind)).collect();
    let first = |field: fn(&Tag) -> Option<String>| tags.iter().find_map(|tag| field(tag));
    let mut out = TagMetadata {
        title: first(|tag| non_empty(tag.title())),
        artist: first(|tag| non_empty(tag.artist())),
        album: first(|tag| non_empty(tag.album())),
        lyrics: String::new(),
        artwork: None,
    };
    // ID3v1 carries no pictures; MP3 covers come from ID3v2 only.
    let pictures = tags.iter().filter(|tag| !mp3 || tag.tag_type() == TagType::Id3v2).flat_map(|tag| tag.pictures());
    out.artwork = cover(pictures).map(|picture| STANDARD.encode(picture.data()));
    Some(out)
}

/// `mutagen.File(path).info.length` in seconds; `None` when unreadable.
pub fn duration_seconds(path: &Path) -> Option<f64> {
    use lofty::file::AudioFile;
    let tagged = Probe::open(path).ok()?.options(ParseOptions::new()).read().ok()?;
    Some(tagged.properties().duration().as_secs_f64())
}

/// TinyTag's fields the analyzer copies into a new track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasicInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub duration: Option<f64>,
}

/// Title, artist, album, genre, year and duration (`None` fields when absent).
pub fn basic_info(path: &Path) -> BasicInfo {
    use lofty::file::AudioFile;
    let Some(tagged) = Probe::open(path).ok().and_then(|probe| probe.options(ParseOptions::new()).read().ok()) else { return BasicInfo::default() };
    let duration = Some(tagged.properties().duration().as_secs_f64());
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { return BasicInfo { duration, ..BasicInfo::default() } };
    BasicInfo {
        title: non_empty(tag.title()),
        artist: non_empty(tag.artist()),
        album: non_empty(tag.album()),
        genre: non_empty(tag.genre()),
        year: tag.get_string(lofty::tag::ItemKey::Year).or_else(|| tag.get_string(lofty::tag::ItemKey::RecordingDate)).map(str::to_owned),
        duration,
    }
}
