//! The library's answer shapes (S3).
//!
//! - [`track_dump`]: `Track.model_dump()` of a loaded row (no validation;
//!   NULL columns stay `None`), what `jsonable_encoder` gets for dict routes
//!   and for a returned `Track` object.
//! - [`track_read`]: the `TrackRead` response model. FastAPI validates the
//!   route's dicts against it, so a NULL in a required field (`title`,
//!   `duration`, `created_at`, ...) or an `analysis_level` other than
//!   `light`/`full` is a ResponseValidationError, i.e. a 500. `None` here.
//! - [`lyrics_read`]: `LyricsRead`, the same way.

use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_db::repos::lyrics::LyricsRow;
use plumdeck_db::repos::tracks::TrackRow;

use crate::response::{boolean, datetime, dict, float, int, opt, text};

/// `Track.model_dump()`, in model field order.
pub fn track_dump(row: &TrackRow) -> Vec<(&'static str, PyValue)> {
    let s = |value: &Option<String>| opt(value.clone(), text);
    let f = |value: Option<f64>| opt(value, float);
    vec![
        ("id", int(row.id)),
        ("filepath", text(row.filepath.clone())),
        ("title", s(&row.title)),
        ("artist", s(&row.artist)),
        ("album", s(&row.album)),
        ("genre", s(&row.genre)),
        ("subgenre", s(&row.subgenre)),
        ("year", opt(row.year, int)),
        ("bpm", f(row.bpm)),
        ("key", s(&row.key)),
        ("scale", s(&row.scale)),
        ("duration", f(row.duration)),
        ("analysis_level", s(&row.analysis_level)),
        ("energy", f(row.energy)),
        ("danceability", f(row.danceability)),
        ("loudness", f(row.loudness)),
        ("brightness", f(row.brightness)),
        ("noisiness", f(row.noisiness)),
        ("contrast", f(row.contrast)),
        ("loudness_range", f(row.loudness_range)),
        ("spectral_flux", f(row.spectral_flux)),
        ("spectral_rolloff", f(row.spectral_rolloff)),
        ("is_genre_verified", opt(row.is_genre_verified, boolean)),
        ("created_at", opt(row.created_at.as_ref(), datetime)),
    ]
}

/// `track_dump` with extra keys appended, as a dict.
pub fn track_dict(row: &TrackRow, extra: Vec<(&'static str, PyValue)>) -> PyValue {
    let mut pairs = track_dump(row);
    pairs.extend(extra);
    dict(pairs)
}

/// The extra keys the library routes add to a dumped track.
#[derive(Clone, Debug, Default)]
pub struct TrackExtras {
    pub is_analyzed: Option<bool>,
    pub lyrics: Option<String>,
    pub has_lyrics: bool,
}

/// `TrackRead.model_validate(dict)` then serialization, or `None` when
/// validation fails (FastAPI answers 500).
pub fn track_read(row: &TrackRow, extras: &TrackExtras) -> Option<PyValue> {
    let required_str = |value: &Option<String>| value.clone().map(text);
    let required_float = |value: Option<f64>| value.map(float);
    let analysis_level = match row.analysis_level.as_deref() {
        None => PyValue::None,
        Some(level @ ("light" | "full")) => text(level),
        Some(_) => return None,
    };
    Some(dict([
        ("id", int(row.id)),
        ("filepath", text(row.filepath.clone())),
        ("title", required_str(&row.title)?),
        ("artist", required_str(&row.artist)?),
        ("album", opt(row.album.clone(), text)),
        ("genre", required_str(&row.genre)?),
        ("subgenre", required_str(&row.subgenre)?),
        ("year", opt(row.year, int)),
        ("duration", required_float(row.duration)?),
        ("bpm", opt(row.bpm, float)),
        ("is_analyzed", opt(extras.is_analyzed, boolean)),
        ("analysis_level", analysis_level),
        ("key", required_str(&row.key)?),
        ("scale", required_str(&row.scale)?),
        ("energy", required_float(row.energy)?),
        ("danceability", required_float(row.danceability)?),
        ("loudness", required_float(row.loudness)?),
        ("brightness", required_float(row.brightness)?),
        ("noisiness", required_float(row.noisiness)?),
        ("contrast", required_float(row.contrast)?),
        ("loudness_range", required_float(row.loudness_range)?),
        ("spectral_flux", required_float(row.spectral_flux)?),
        ("spectral_rolloff", required_float(row.spectral_rolloff)?),
        ("lyrics", opt(extras.lyrics.clone(), text)),
        ("has_lyrics", boolean(extras.has_lyrics)),
        ("is_genre_verified", boolean(row.is_genre_verified?)),
        ("created_at", datetime(row.created_at.as_ref()?)),
    ]))
}

/// `TrackRead` for a list: `None` when any element fails.
pub fn track_reads<'a>(items: impl IntoIterator<Item = (&'a TrackRow, TrackExtras)>) -> Option<PyValue> {
    items.into_iter().map(|(row, extras)| track_read(row, &extras)).collect::<Option<Vec<_>>>().map(PyValue::List)
}

/// `LyricsRead`, or `None` when validation fails.
pub fn lyrics_read(row: &LyricsRow) -> Option<PyValue> {
    Some(dict([
        ("content", text(row.content.clone()?)),
        ("source", opt(row.source.clone(), text)),
        ("language", opt(row.language.clone(), text)),
        ("track_id", int(row.track_id)),
        ("created_at", datetime(row.created_at.as_ref()?)),
        ("updated_at", datetime(row.updated_at.as_ref()?)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> TrackRow {
        TrackRow {
            id: 1,
            filepath: "/a.mp3".into(),
            title: Some("A".into()),
            artist: Some("B".into()),
            album: None,
            genre: Some("House".into()),
            subgenre: Some(String::new()),
            year: None,
            bpm: Some(f64::NAN),
            key: Some(String::new()),
            scale: Some(String::new()),
            duration: Some(1.5),
            analysis_level: Some("light".into()),
            energy: Some(0.0),
            danceability: Some(0.0),
            loudness: Some(-60.0),
            brightness: Some(0.0),
            noisiness: Some(0.0),
            contrast: Some(0.0),
            loudness_range: Some(0.0),
            spectral_flux: Some(0.0),
            spectral_rolloff: Some(0.0),
            is_genre_verified: Some(false),
            created_at: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap().and_hms_opt(3, 4, 5),
        }
    }

    #[test]
    fn response_model_validation_fails_like_pydantic() {
        let extras = TrackExtras::default();
        let value = track_read(&row(), &extras).unwrap();
        let PyValue::Dict(pairs) = &value else { panic!() };
        assert_eq!(pairs.len(), 27);
        assert!(track_read(&TrackRow { title: None, ..row() }, &extras).is_none());
        assert!(track_read(&TrackRow { analysis_level: Some("draft".into()), ..row() }, &extras).is_none());
        assert!(track_read(&TrackRow { is_genre_verified: None, ..row() }, &extras).is_none());
        assert!(track_read(&TrackRow { album: None, bpm: None, ..row() }, &extras).is_some());
        assert_eq!(track_dump(&row()).len(), 24);
    }
}
