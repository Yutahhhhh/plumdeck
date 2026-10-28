//! Wordplay pair rules (backend/app/services/wordplay_app_service.py and the
//! request models of backend/api/routers/wordplay.py): keyword
//! normalization, the source URL check, the cue range checks and the fit
//! flags a pair's answer carries.

use plumdeck_core::compat::pyfmt::round_ndigits;
use plumdeck_core::compat::text::{casefold, py_strip};

pub const EVIDENCE_TYPES: [&str; 3] = ["hypothesis", "edit_listing", "performance"];
pub const VERIFICATION_STATUSES: [&str; 2] = ["unverified", "tested"];
pub const PAIR_STATUSES: [&str; 2] = ["pending", "approved"];
pub const SECTION_POSITIONS: [&str; 4] = ["unknown", "start", "middle", "end"];
pub const SOURCE_CUE_MODES: [&str; 3] = ["section_end", "cue_drumming", "cue_drumming_intro"];

/// `normalize_keyword`: `keyword.strip().casefold()`.
pub fn normalize_keyword(keyword: &str) -> String {
    casefold(py_strip(keyword))
}

/// What `urlparse` raises on a malformed authority (`ValueError`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidUrl;

/// `urlparse(value)` reduced to what the check reads: the scheme
/// (lowercased) and whether the network location is non-empty.
fn scheme_and_netloc(value: &str) -> Result<(String, bool), InvalidUrl> {
    // urlsplit strips leading C0 controls and spaces, and drops tab, CR, LF.
    let url: String = value.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(colon) = url.find(':') {
        let candidate = &url[..colon];
        let valid = colon > 0
            && candidate.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && candidate.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            scheme = candidate.to_ascii_lowercase();
            rest = &url[colon + 1..];
        }
    }
    let Some(after) = rest.strip_prefix("//") else { return Ok((scheme, false)) };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    if netloc.contains('[') != netloc.contains(']') {
        return Err(InvalidUrl);
    }
    Ok((scheme, !netloc.is_empty()))
}

/// `_validate_url` / the `http_url_or_empty` validator: empty, or an
/// http(s) URL with a host. `Err` is urlparse's own `ValueError`.
pub fn is_http_url(value: &str) -> Result<bool, InvalidUrl> {
    let (scheme, has_netloc) = scheme_and_netloc(value)?;
    Ok(matches!(scheme.as_str(), "http" | "https") && has_netloc)
}

/// The track facts the fit checks read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackFacts {
    pub genre: Option<String>,
    pub subgenre: Option<String>,
    pub bpm: Option<f64>,
    pub duration: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub brightness: Option<f64>,
    pub noisiness: Option<f64>,
}

/// `_cue_in_range`: a timestamp inside a track with a positive, finite duration.
pub fn cue_in_range(timestamp: Option<f64>, track: Option<&TrackFacts>) -> bool {
    let (Some(timestamp), Some(track)) = (timestamp, track) else { return false };
    track.duration.is_some_and(|duration| duration.is_finite() && duration > 0.0 && 0.0 <= timestamp && timestamp < duration)
}

/// `_style_fit`: same genre, and the same subgenre or at least three of the
/// four features within 0.20 (rounded to 4 places).
pub fn style_fit(from: Option<&TrackFacts>, to: Option<&TrackFacts>) -> bool {
    let (Some(from), Some(to)) = (from, to) else { return false };
    let normalized = |value: &Option<String>| value.as_deref().map(|text| casefold(py_strip(text))).unwrap_or_default();
    let from_genre = normalized(&from.genre);
    if from_genre.is_empty() || from_genre != normalized(&to.genre) {
        return false;
    }
    let from_subgenre = normalized(&from.subgenre);
    if !from_subgenre.is_empty() && from_subgenre == normalized(&to.subgenre) {
        return true;
    }
    let pairs = [(from.energy, to.energy), (from.danceability, to.danceability), (from.brightness, to.brightness), (from.noisiness, to.noisiness)];
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|pair| match pair {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some(round_ndigits((a - b).abs(), 4)),
            _ => None,
        })
        .collect();
    deltas.len() >= 3 && deltas.iter().all(|delta| *delta <= 0.20)
}

/// `_bpm_delta_percent`, rounded to 4 places; `None` without two positive,
/// finite tempos.
pub fn bpm_delta_percent(from: Option<&TrackFacts>, to: Option<&TrackFacts>) -> Option<f64> {
    let (from, to) = (from?.bpm?, to?.bpm?);
    if !from.is_finite() || !to.is_finite() || from <= 0.0 || to <= 0.0 {
        return None;
    }
    Some(round_ndigits((to - from).abs() / from * 100.0, 4))
}

/// The cue fields of a pair.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cues {
    pub source_section_position: String,
    pub target_section_position: String,
    pub source_cue_mode: String,
    pub from_timestamp: Option<f64>,
    pub source_cue_end_timestamp: Option<f64>,
    pub to_timestamp: Option<f64>,
    pub target_intro_timestamp: Option<f64>,
    pub target_landing_timestamp: Option<f64>,
}

/// The fit flags of `_item`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub bpm_delta_percent: Option<f64>,
    pub style_fit: bool,
    pub source_cue_fit: bool,
    pub target_timing_fit: bool,
    pub boundary_fit: bool,
}

/// `_item`'s flags. Python evaluates `a and b and x < y`: the comparison
/// runs only when both ranges hold, so a `None` there never compares.
pub fn fit(cues: &Cues, from: Option<&TrackFacts>, to: Option<&TrackFacts>) -> Fit {
    let bpm_delta_percent = bpm_delta_percent(from, to);
    let style = style_fit(from, to);
    let drumming = matches!(cues.source_cue_mode.as_str(), "cue_drumming" | "cue_drumming_intro");
    let source_cue_fit = cue_in_range(cues.from_timestamp, from)
        && ((drumming
            && cue_in_range(cues.source_cue_end_timestamp, from)
            && cues.from_timestamp.expect("in range") < cues.source_cue_end_timestamp.expect("in range"))
            || (!drumming && cues.source_section_position == "end"));
    let landing = cues.target_landing_timestamp.or(cues.to_timestamp);
    let target_timing_fit = cue_in_range(cues.target_intro_timestamp, to)
        && cue_in_range(landing, to)
        && cues.target_intro_timestamp.expect("in range") < landing.expect("in range")
        && ((drumming && matches!(cues.target_section_position.as_str(), "start" | "middle"))
            || (!drumming && cues.target_section_position == "start"));
    let boundary_fit = source_cue_fit && target_timing_fit && style && bpm_delta_percent.is_some_and(|delta| delta <= 2.0);
    Fit { bpm_delta_percent, style_fit: style, source_cue_fit, target_timing_fit, boundary_fit }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_urlparse() {
        assert_eq!(is_http_url("https://example.invalid/x"), Ok(true));
        assert_eq!(is_http_url("HTTP://host"), Ok(true));
        assert_eq!(is_http_url("ftp://host"), Ok(false));
        assert_eq!(is_http_url("http:/path"), Ok(false));
        assert_eq!(is_http_url("example.invalid"), Ok(false));
        assert_eq!(is_http_url(" \x01http://h\tost"), Ok(true));
        assert_eq!(is_http_url("http://[::1"), Err(InvalidUrl));
    }

    fn track(genre: &str, subgenre: &str, bpm: f64) -> TrackFacts {
        TrackFacts {
            genre: Some(genre.into()),
            subgenre: Some(subgenre.into()),
            bpm: Some(bpm),
            duration: Some(200.0),
            energy: Some(0.5),
            danceability: Some(0.5),
            brightness: Some(0.5),
            noisiness: Some(0.7),
        }
    }

    #[test]
    fn fit_needs_cues_style_and_tempo() {
        let from = track("House", "Deep", 124.0);
        let to = track(" house ", "", 126.0);
        assert!(style_fit(Some(&from), Some(&to)));
        assert_eq!(bpm_delta_percent(Some(&from), Some(&to)), Some(1.6129));
        let cues = Cues {
            source_section_position: "end".into(),
            target_section_position: "start".into(),
            source_cue_mode: "section_end".into(),
            from_timestamp: Some(10.0),
            target_intro_timestamp: Some(1.0),
            to_timestamp: Some(5.0),
            ..Cues::default()
        };
        let flags = fit(&cues, Some(&from), Some(&to));
        assert!(flags.source_cue_fit && flags.target_timing_fit && flags.boundary_fit);
        let late = Cues { from_timestamp: Some(200.0), ..cues.clone() };
        assert!(!fit(&late, Some(&from), Some(&to)).source_cue_fit);
        assert!(!fit(&cues, Some(&from), None).boundary_fit);
        assert_eq!(normalize_keyword("  Straße "), "strasse");
    }
}
