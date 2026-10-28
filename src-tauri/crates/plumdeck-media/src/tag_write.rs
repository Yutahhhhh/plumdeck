//! Writing tags into audio files (backend/utils/metadata.py
//! `update_file_tags_extended`, `update_file_metadata`; S8), with lofty.
//!
//! Basic fields are written to the file's primary tag (created when
//! missing); lyrics and front cover art only for MP3, M4A and FLAC, as
//! Python's per-format branches did.

use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag, TagExt, TagType};

/// The basic fields `update_file_tags_extended` sets (each only when truthy).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasicTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub genre: Option<String>,
}

fn rich_format(path: &Path) -> bool {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(extension.as_str(), "mp3" | "m4a" | "flac")
}

/// What `mutagen.File` makes of a file: `Ok(None)` when no format claims
/// it, an error when the claimed format does not load. mutagen also claims
/// by extension, so an unrecognised `.mp3`/`.flac`/`.wav`/`.aif(f)` fails.
fn open(path: &Path, properties: bool) -> Result<Option<TaggedFile>, String> {
    let probe = Probe::open(path).map_err(|error| error.to_string())?;
    let probe = probe.guess_file_type().map_err(|error| error.to_string())?;
    if probe.file_type().is_none() {
        let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if matches!(extension.as_str(), "mp3" | "mp2" | "mpg" | "mpeg" | "flac" | "wav" | "aif" | "aiff" | "aifc") {
            return Err("can't sync to a frame of the claimed format".into());
        }
        return Ok(None);
    }
    let file = probe.options(ParseOptions::new().read_properties(properties)).read().map_err(|error| error.to_string())?;
    Ok(Some(file))
}

/// Edit and save the primary tag (created when missing). Tags only:
/// mutagen edits files whose audio properties do not parse, so lofty does
/// not read them.
fn edit_file(path: &Path, mut tagged: TaggedFile, change: impl FnOnce(&mut Tag)) -> Result<(), String> {
    let kind = tagged.primary_tag_type();
    if tagged.primary_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(kind));
    }
    let tag = tagged.primary_tag_mut().expect("inserted");
    change(tag);
    // lofty asserts on some atom layouts mutagen writes through; a panic is a failed write.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tag.save_to_path(path, WriteOptions::default())))
        .map_err(|_| "tag writer panicked".to_owned())?
        .map_err(|error| error.to_string())
}

/// `update_file_tags_extended`: basic fields, then lyrics when given.
/// `false` when the file cannot be read or written.
pub fn write_tags(path: &Path, basic: &BasicTags, lyrics: Option<&str>) -> bool {
    let set = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
    let any = set(&basic.title).is_some()
        || set(&basic.artist).is_some()
        || set(&basic.album).is_some()
        || basic.year.is_some_and(|year| year != 0)
        || set(&basic.genre).is_some();
    let tagged = match open(path, false) {
        Ok(tagged) => tagged,
        Err(_) => return false,
    };
    if let Some(tagged) = tagged {
        match tagged.file_type() {
            // EasyMP3 needs an MPEG frame to load.
            FileType::Mpeg if open(path, true).is_err() => return false,
            // WAVE/AIFF have no easy keys: assigning any field raises TypeError.
            FileType::Wav | FileType::Aiff if any => return false,
            _ => {}
        }
        let wrote = edit_file(path, tagged, |tag| {
            if let Some(title) = set(&basic.title) {
                tag.set_title(title);
            }
            if let Some(artist) = set(&basic.artist) {
                tag.set_artist(artist);
            }
            if let Some(album) = set(&basic.album) {
                tag.set_album(album);
            }
            if let Some(year) = basic.year.filter(|year| *year != 0) {
                tag.insert_text(ItemKey::RecordingDate, year.to_string());
            }
            if let Some(genre) = set(&basic.genre) {
                tag.set_genre(genre);
            }
        });
        if wrote.is_err() {
            return false;
        }
    }
    if let Some(lyrics) = lyrics {
        // Python ignored the lyrics write's result here.
        let _ = write_lyrics_and_art(path, Some(lyrics), None);
    }
    true
}

/// `update_file_metadata`: lyrics (replacing any) and a JPEG front cover.
pub fn write_lyrics_and_art(path: &Path, lyrics: Option<&str>, artwork: Option<&[u8]>) -> bool {
    if !rich_format(path) {
        return false;
    }
    let Ok(Some(tagged)) = open(path, false) else { return false };
    // ID3(filepath) raises without an ID3v2 header.
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) && !tagged.contains_tag_type(TagType::Id3v2) {
        return false;
    }
    edit_file(path, tagged, |tag| {
        if let Some(lyrics) = lyrics {
            tag.remove_key(ItemKey::Lyrics);
            tag.insert_text(ItemKey::Lyrics, lyrics.to_owned());
        }
        if let Some(data) = artwork {
            // ID3 delall("APIC"), MP4 covr = [new], FLAC clear_pictures():
            // every picture is replaced.
            while tag.picture_count() > 0 {
                tag.remove_picture(0);
            }
            tag.push_picture(Picture::unchecked(data.to_vec()).pic_type(PictureType::CoverFront).mime_type(MimeType::Jpeg).build());
        }
    })
    .is_ok()
}
