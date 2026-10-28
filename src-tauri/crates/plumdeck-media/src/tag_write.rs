//! Writing tags into audio files (backend/utils/metadata.py
//! `update_file_tags_extended`, `update_file_metadata`; S8), with lofty.
//!
//! Basic fields are written to the file's primary tag (created when
//! missing); lyrics and front cover art only for MP3, M4A and FLAC, as
//! Python's per-format branches did.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag, TagExt};

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

fn edit(path: &Path, change: impl FnOnce(&mut Tag)) -> Result<(), String> {
    let mut tagged = Probe::open(path).and_then(|probe| probe.read()).map_err(|error| error.to_string())?;
    let kind = tagged.primary_tag_type();
    if tagged.primary_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(kind));
    }
    let tag = tagged.primary_tag_mut().expect("inserted");
    change(tag);
    tag.save_to_path(path, WriteOptions::default()).map_err(|error| error.to_string())
}

/// `update_file_tags_extended`: basic fields, then lyrics when given.
/// `false` when the file cannot be read or written.
pub fn write_tags(path: &Path, basic: &BasicTags, lyrics: Option<&str>) -> bool {
    let wrote = edit(path, |tag| {
        let set = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
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
    edit(path, |tag| {
        if let Some(lyrics) = lyrics {
            tag.remove_key(ItemKey::Lyrics);
            tag.insert_text(ItemKey::Lyrics, lyrics.to_owned());
        }
        if let Some(data) = artwork {
            tag.remove_picture_type(PictureType::CoverFront);
            tag.push_picture(Picture::unchecked(data.to_vec()).pic_type(PictureType::CoverFront).mime_type(MimeType::Jpeg).build());
        }
    })
    .is_ok()
}
