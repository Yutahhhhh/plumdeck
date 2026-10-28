//! Typed rows of the master.db tables plumdeck reads.
//!
//! Column meanings come from pyrekordbox's db6 table definitions (MIT) and
//! from checks against a real library; see the rekordbox data map in the
//! parity design.

/// One `djmdContent` row (joined with its artist name and key name).
#[derive(Debug, Clone, PartialEq)]
pub struct Content {
    pub id: String,
    /// Absolute audio path, exactly as rekordbox stores it.
    pub folder_path: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// `BPM`: tempo × 100; 0 means not analysed.
    pub bpm_x100: Option<i64>,
    pub key_id: Option<String>,
    /// `djmdKey.ScaleName` of `KeyID`, as stored ("Fm", "8A", or junk).
    pub key_name: Option<String>,
    /// Track length in seconds.
    pub length_s: Option<i64>,
    /// `/PIONEER/USBANLZ/…/ANLZ0000.DAT`, relative to the share folder.
    pub analysis_data_path: Option<String>,
    /// 0–5 stars.
    pub rating: Option<i64>,
    pub color_id: Option<String>,
    /// `Commnt`.
    pub comment: Option<String>,
    /// 1 mp3, 4 m4a, 11 wav, 12 aiff, 19/20/26 streaming services.
    pub file_type: Option<i64>,
    pub rb_local_usn: Option<i64>,
    pub analysis_updated: Option<String>,
    pub cue_updated: Option<String>,
}

impl Content {
    /// BPM as a float; `None` for 0 or missing (Python: `bpm / 100 if bpm`).
    pub fn bpm(&self) -> Option<f64> {
        self.bpm_x100.filter(|&v| v != 0).map(|v| v as f64 / 100.0)
    }
}

/// `djmdKey`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyRow {
    pub id: String,
    pub scale_name: Option<String>,
    pub seq: Option<i64>,
}

/// Which pad or list a `djmdCue` row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CueSlot {
    /// `Kind` 0.
    Memory,
    /// Hot cue pad, 0 = A … 15 = P. `Kind` 1–3 are A–C, 5–9 are D–H (4 is
    /// never used) and 10–17 are I–P.
    Hot(u8),
    /// Any other `Kind` value.
    Unknown(i64),
}

impl CueSlot {
    pub fn from_kind(kind: i64) -> Self {
        match kind {
            0 => CueSlot::Memory,
            1..=3 => CueSlot::Hot((kind - 1) as u8),
            5..=17 => CueSlot::Hot((kind - 2) as u8),
            other => CueSlot::Unknown(other),
        }
    }

    /// The pad letter of a hot cue ("A" … "P").
    pub fn letter(self) -> Option<char> {
        match self {
            CueSlot::Hot(slot) if slot < 16 => Some(char::from(b'A' + slot)),
            _ => None,
        }
    }
}

/// A quantized loop length in beats, `numerator / denominator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeatLoop {
    pub numerator: u16,
    pub denominator: u16,
}

impl BeatLoop {
    /// `BeatLoopSize` packs `(numerator << 16) | denominator`; 0 is none.
    pub fn from_packed(value: i64) -> Option<Self> {
        let value = u32::try_from(value).ok().filter(|&v| v != 0)?;
        Some(Self { numerator: (value >> 16) as u16, denominator: (value & 0xffff) as u16 })
    }

    pub fn beats(self) -> Option<f64> {
        (self.denominator != 0).then(|| f64::from(self.numerator) / f64::from(self.denominator))
    }
}

/// `djmdCue`: hot cues, memory cues and loops.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub id: String,
    pub content_id: String,
    pub kind: i64,
    pub slot: CueSlot,
    /// Position in milliseconds on rekordbox's own timeline.
    pub in_msec: Option<i64>,
    /// `InFrame`, 1/150 s units.
    pub in_frame: Option<i64>,
    /// Loop end in milliseconds; `None` when this is not a loop (`-1`).
    pub out_msec: Option<i64>,
    /// `Color`: −1 none, 255 seen for coloured cues.
    pub color: Option<i64>,
    /// Hot-cue palette index (PCO2 `color_code` 0x01–0x3e); 0 is default.
    pub color_table_index: Option<i64>,
    pub active_loop: bool,
    pub comment: Option<String>,
    pub beat_loop: Option<BeatLoop>,
}

impl Cue {
    pub fn is_loop(&self) -> bool {
        self.out_msec.is_some()
    }
}

/// `djmdMixerParam`: rekordbox's Auto Gain for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerParam {
    pub content_id: String,
    pub gain_high: i64,
    pub gain_low: i64,
    pub peak_high: i64,
    pub peak_low: i64,
}

impl MixerParam {
    /// Linear gain: `(GainHigh << 16) | GainLow` read as an f32 bit pattern.
    pub fn gain(&self) -> Option<f32> {
        pack_f32(self.gain_high, self.gain_low)
    }

    /// Linear peak, packed the same way.
    pub fn peak(&self) -> Option<f32> {
        pack_f32(self.peak_high, self.peak_low)
    }

    /// `20·log10(gain)`; `None` for a non-positive or non-finite gain.
    pub fn gain_db(&self) -> Option<f64> {
        let gain = f64::from(self.gain()?);
        (gain.is_finite() && gain > 0.0).then(|| 20.0 * gain.log10())
    }
}

fn pack_f32(high: i64, low: i64) -> Option<f32> {
    let high = u16::try_from(high).ok()?;
    let low = u16::try_from(low).ok()?;
    Some(f32::from_bits((u32::from(high) << 16) | u32::from(low)))
}

/// `djmdMyTag`: a MyTag (or one of its column headers when it has children).
#[derive(Debug, Clone, PartialEq)]
pub struct MyTag {
    pub id: String,
    pub seq: Option<i64>,
    pub name: Option<String>,
    pub attribute: Option<i64>,
    pub parent_id: Option<String>,
}

/// `djmdSongMyTag`: one tag assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct SongMyTag {
    pub my_tag_id: String,
    pub content_id: String,
    pub track_no: Option<i64>,
}

/// `djmdColor`: the eight track colours.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorRow {
    pub id: String,
    pub color_code: Option<i64>,
    pub sort_key: Option<i64>,
    pub comment: Option<String>,
}

/// `djmdPlaylist.Attribute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistKind {
    Playlist,
    Folder,
    Smart,
    Unknown(i64),
}

impl PlaylistKind {
    pub fn from_attribute(value: Option<i64>) -> Self {
        match value {
            Some(0) => PlaylistKind::Playlist,
            Some(1) => PlaylistKind::Folder,
            Some(4) => PlaylistKind::Smart,
            Some(other) => PlaylistKind::Unknown(other),
            None => PlaylistKind::Unknown(-1),
        }
    }

    /// The names `rekordbox_playlists.py` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            PlaylistKind::Playlist => "playlist",
            PlaylistKind::Folder => "folder",
            PlaylistKind::Smart => "smart",
            PlaylistKind::Unknown(_) => "unknown",
        }
    }
}

/// `djmdPlaylist`, with its live track count.
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub kind: PlaylistKind,
    pub seq: Option<i64>,
    /// `SmartList` rule XML for intelligent playlists.
    pub smart_list: Option<String>,
    pub track_count: i64,
}

/// One playlist node of [`super::MasterDb::playlist_tree`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistNode {
    pub id: String,
    /// Name with control characters removed and trimmed.
    pub name: String,
    /// Parent ID, `"root"` at the top level.
    pub parent_id: String,
    pub kind: PlaylistKind,
    pub seq: i64,
    pub track_count: i64,
    /// `Folder/Sub/Name`.
    pub path: String,
    pub children: Vec<PlaylistNode>,
}

/// One track slot of a playlist, in the DJ's own order.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistSong {
    pub track_no: i64,
    pub content_id: String,
    pub folder_path: String,
    pub title: String,
    pub artist: String,
}

/// `djmdHistory`.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub id: String,
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub attribute: Option<i64>,
    pub seq: Option<i64>,
    pub date_created: Option<String>,
}

/// One HISTORY playlist: the files played, in play order.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaySession {
    pub history_id: String,
    /// `DateCreated` as stored (`YYYY-MM-DD…`).
    pub date_created: Option<String>,
    pub filepaths: Vec<String>,
}

/// `contentFile`: one file rekordbox keeps for a track (ANLZ files, artwork).
#[derive(Debug, Clone, PartialEq)]
pub struct ContentFile {
    pub content_id: String,
    /// Path relative to the share folder (`/PIONEER/USBANLZ/…`).
    pub path: Option<String>,
    /// Absolute local path.
    pub rb_local_path: Option<String>,
    pub size: Option<i64>,
}

/// `djmdRecommendLike`: a Matching pair.
#[derive(Debug, Clone, PartialEq)]
pub struct RecommendLike {
    pub content_id_1: String,
    pub content_id_2: String,
    pub like_rate: Option<i64>,
}

/// `djmdRelatedTracks`: a Related Tracks rule (JSON criteria).
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedTracks {
    pub id: String,
    pub name: Option<String>,
    pub attribute: Option<i64>,
    pub parent_id: Option<String>,
    pub criteria: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_kinds_map_to_pads() {
        assert_eq!(CueSlot::from_kind(0), CueSlot::Memory);
        assert_eq!(CueSlot::from_kind(1).letter(), Some('A'));
        assert_eq!(CueSlot::from_kind(3).letter(), Some('C'));
        assert_eq!(CueSlot::from_kind(4), CueSlot::Unknown(4));
        assert_eq!(CueSlot::from_kind(5).letter(), Some('D'));
        assert_eq!(CueSlot::from_kind(9).letter(), Some('H'));
        assert_eq!(CueSlot::from_kind(10).letter(), Some('I'));
        assert_eq!(CueSlot::from_kind(17).letter(), Some('P'));
        assert_eq!(CueSlot::from_kind(18), CueSlot::Unknown(18));
        assert_eq!(CueSlot::from_kind(-1), CueSlot::Unknown(-1));
    }

    #[test]
    fn beat_loop_unpacks() {
        let half = BeatLoop::from_packed((1 << 16) | 2).unwrap();
        assert_eq!(half.beats(), Some(0.5));
        assert_eq!(BeatLoop::from_packed(4 << 16 | 1).unwrap().beats(), Some(4.0));
        assert_eq!(BeatLoop::from_packed(0), None);
        assert_eq!(BeatLoop::from_packed(-1), None);
    }

    #[test]
    fn mixer_gain_is_a_packed_float() {
        let bits = 0.5f32.to_bits();
        let param = MixerParam {
            content_id: "1".into(),
            gain_high: i64::from(bits >> 16),
            gain_low: i64::from(bits & 0xffff),
            peak_high: 0,
            peak_low: 0,
        };
        assert_eq!(param.gain(), Some(0.5));
        assert!((param.gain_db().unwrap() + 6.0206).abs() < 1e-3);
        assert_eq!(param.peak(), Some(0.0));
        let bad = MixerParam { gain_high: 70000, ..param.clone() };
        assert_eq!(bad.gain(), None);
        let zero = MixerParam { gain_high: 0, gain_low: 0, ..param };
        assert_eq!(zero.gain_db(), None);
    }
}
