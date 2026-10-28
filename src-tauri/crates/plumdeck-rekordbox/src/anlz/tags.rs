//! Section bodies. Offsets are from the start of the section (byte 0 is the
//! fourcc), as in the Deep Symmetry byte-field diagrams.

use super::bytes::{array, fourcc_at, slice, u16_at, u32_at, u8_at, utf16be};
use super::phrase::{self, SongStructure};

/// One parsed section.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    /// PQTZ.
    BeatGrid(BeatGrid),
    /// PQT2.
    ExtendedBeatGrid(ExtendedBeatGrid),
    /// PCOB.
    CueList(CueList),
    /// PCO2.
    ExtendedCueList(ExtendedCueList),
    /// PPTH: the audio path the analysis belongs to.
    Path(String),
    /// PVBR.
    Vbr(VbrIndex),
    /// PWAV, PWV2, PWV3, PWV4, PWV5, PWV6, PWV7.
    Waveform(Waveform),
    /// PWVC.
    WaveformCalibration(WaveformCalibration),
    /// PSSI.
    SongStructure(SongStructure),
    /// A known section whose body did not match the format.
    Invalid { fourcc: [u8; 4], reason: &'static str },
    /// A section this parser does not know; skipped.
    Unknown { fourcc: [u8; 4], len: usize },
}

pub(super) fn parse_tag(fourcc: [u8; 4], tag: &[u8]) -> Tag {
    let parsed = match &fourcc {
        b"PQTZ" => parse_pqtz(tag).map(Tag::BeatGrid),
        b"PQT2" => parse_pqt2(tag).map(Tag::ExtendedBeatGrid),
        b"PCOB" => parse_pcob(tag).map(Tag::CueList),
        b"PCO2" => parse_pco2(tag).map(Tag::ExtendedCueList),
        b"PPTH" => parse_ppth(tag).map(Tag::Path),
        b"PVBR" => parse_pvbr(tag).map(Tag::Vbr),
        b"PWAV" => parse_preview(tag, WaveformKind::Preview).map(Tag::Waveform),
        b"PWV2" => parse_preview(tag, WaveformKind::TinyPreview).map(Tag::Waveform),
        b"PWV3" => parse_table(tag, WaveformKind::Detail, 0x18).map(Tag::Waveform),
        b"PWV4" => parse_table(tag, WaveformKind::ColorPreview, 0x18).map(Tag::Waveform),
        b"PWV5" => parse_table(tag, WaveformKind::ColorDetail, 0x18).map(Tag::Waveform),
        b"PWV6" => parse_table(tag, WaveformKind::ThreeBandPreview, 0x14).map(Tag::Waveform),
        b"PWV7" => parse_table(tag, WaveformKind::ThreeBandDetail, 0x18).map(Tag::Waveform),
        b"PWVC" => parse_pwvc(tag).map(Tag::WaveformCalibration),
        b"PSSI" => phrase::parse_pssi(tag).map(Tag::SongStructure),
        _ => return Tag::Unknown { fourcc, len: tag.len() },
    };
    parsed.unwrap_or_else(|reason| Tag::Invalid { fourcc, reason })
}

type Parsed<T> = Result<T, &'static str>;

// ------------------------------------------------------------------- beats

/// One PQTZ entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beat {
    /// Position in the bar, 1–4 (1 is the downbeat).
    pub beat_number: u16,
    /// Tempo at this beat, BPM × 100.
    pub tempo_x100: u16,
    /// Milliseconds on rekordbox's timeline.
    pub time_ms: u32,
}

/// PQTZ: every beat of the track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatGrid {
    pub beats: Vec<Beat>,
}

impl BeatGrid {
    /// Median tempo in BPM (Python's `statistics.median` of the tempos).
    pub fn median_bpm(&self) -> Option<f64> {
        let mut tempos: Vec<u16> = self.beats.iter().map(|b| b.tempo_x100).collect();
        if tempos.is_empty() {
            return None;
        }
        tempos.sort_unstable();
        let mid = tempos.len() / 2;
        let median = if tempos.len() % 2 == 1 {
            f64::from(tempos[mid])
        } else {
            (f64::from(tempos[mid - 1]) + f64::from(tempos[mid])) / 2.0
        };
        Some(median / 100.0)
    }

    /// Time of the 1-based beat `number` (the numbering PSSI uses).
    pub fn time_of_beat(&self, number: u16) -> Option<u32> {
        let index = usize::from(number).checked_sub(1)?;
        self.beats.get(index).map(|b| b.time_ms)
    }
}

fn beat_at(tag: &[u8], at: usize) -> Option<Beat> {
    Some(Beat { beat_number: u16_at(tag, at)?, tempo_x100: u16_at(tag, at + 2)?, time_ms: u32_at(tag, at + 4)? })
}

fn parse_pqtz(tag: &[u8]) -> Parsed<BeatGrid> {
    let count = u32_at(tag, 0x14).ok_or("truncated header")? as usize;
    let entries = array(tag, 0x18, count, 8).ok_or("beat count exceeds section")?;
    let beats = entries
        .chunks_exact(8)
        .filter_map(|e| beat_at(e, 0))
        .collect();
    Ok(BeatGrid { beats })
}

/// PQT2: the extended grid of `.EXT` files. Not described by Deep Symmetry;
/// the layout (two bracketing beats at 0x18 and 0x20, an entry count at 0x28
/// and 2-byte entries from 0x38) follows pyrekordbox's (MIT) partial
/// definition and parsed cleanly on real files. The value at 0x10, which
/// pyrekordbox requires to be 0x01000002, is not checked (other values occur).
/// Kept raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedBeatGrid {
    pub edge_beats: [Beat; 2],
    pub entries: Vec<[u8; 2]>,
}

fn parse_pqt2(tag: &[u8]) -> Parsed<ExtendedBeatGrid> {
    let first = beat_at(tag, 0x18).ok_or("truncated header")?;
    let last = beat_at(tag, 0x20).ok_or("truncated header")?;
    let count = u32_at(tag, 0x28).ok_or("truncated header")? as usize;
    if tag.len() < 0x38 {
        return Err("truncated header");
    }
    let raw = array(tag, 0x38, count, 2).ok_or("entry count exceeds section")?;
    Ok(ExtendedBeatGrid { edge_beats: [first, last], entries: raw.chunks_exact(2).map(|p| [p[0], p[1]]).collect() })
}

// -------------------------------------------------------------------- cues

/// PCOB / PCO2 list type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueListType {
    Memory,
    HotCue,
    Unknown(u32),
}

impl CueListType {
    fn from_raw(value: u32) -> Self {
        match value {
            0 => CueListType::Memory,
            1 => CueListType::HotCue,
            other => CueListType::Unknown(other),
        }
    }
}

/// PCPT, one entry of a PCOB list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuePoint {
    /// 0 for a memory point, else the hot cue number (A = 1).
    pub hot_cue: u32,
    /// 0 normal, 4 active loop.
    pub status: u32,
    pub order_first: u16,
    pub order_last: u16,
    /// 1 point, 2 loop.
    pub cue_type: u8,
    pub time_ms: u32,
    /// Loop end, meaningful when `cue_type == 2`.
    pub loop_time_ms: u32,
}

/// PCOB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueList {
    pub list_type: CueListType,
    pub memory_count: u32,
    pub entries: Vec<CuePoint>,
    /// Fewer well-formed entries were found than the header announced.
    pub truncated: bool,
}

fn parse_pcob(tag: &[u8]) -> Parsed<CueList> {
    let list_type = CueListType::from_raw(u32_at(tag, 0x0c).ok_or("truncated header")?);
    let count = usize::from(u16_at(tag, 0x12).ok_or("truncated header")?);
    let memory_count = u32_at(tag, 0x14).ok_or("truncated header")?;
    let mut entries = Vec::new();
    let mut offset = 0x18usize;
    for _ in 0..count {
        let Some((entry, len)) = entry_envelope(tag, offset, b"PCPT", 0x28) else { break };
        entries.push(CuePoint {
            hot_cue: u32_at(entry, 0x0c).unwrap_or(0),
            status: u32_at(entry, 0x10).unwrap_or(0),
            order_first: u16_at(entry, 0x18).unwrap_or(0),
            order_last: u16_at(entry, 0x1a).unwrap_or(0),
            cue_type: u8_at(entry, 0x1c).unwrap_or(0),
            time_ms: u32_at(entry, 0x20).unwrap_or(0),
            loop_time_ms: u32_at(entry, 0x24).unwrap_or(0),
        });
        offset += len;
    }
    Ok(CueList { list_type, memory_count, truncated: entries.len() < count, entries })
}

/// PCP2, one entry of a PCO2 list. Incomplete entries (the docs warn they
/// exist) keep what they have: no comment and/or no colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedCuePoint {
    /// 0 for a memory point, else the hot cue number (A = 1).
    pub hot_cue: u32,
    /// 1 point, 2 loop.
    pub cue_type: u8,
    pub time_ms: u32,
    pub loop_time_ms: u32,
    /// Memory point / loop colour row (0 none).
    pub color_id: Option<u8>,
    pub loop_numerator: Option<u16>,
    pub loop_denominator: Option<u16>,
    pub comment: Option<String>,
    /// Hot cue palette code (0 default green, 0x01–0x3e palette).
    pub color_code: Option<u8>,
    /// LED colour.
    pub color_rgb: Option<[u8; 3]>,
}

/// PCO2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedCueList {
    pub list_type: CueListType,
    pub entries: Vec<ExtendedCuePoint>,
    pub truncated: bool,
}

fn parse_pco2(tag: &[u8]) -> Parsed<ExtendedCueList> {
    let list_type = CueListType::from_raw(u32_at(tag, 0x0c).ok_or("truncated header")?);
    let count = usize::from(u16_at(tag, 0x10).ok_or("truncated header")?);
    let mut entries = Vec::new();
    let mut offset = 0x14usize;
    for _ in 0..count {
        let Some((entry, len)) = entry_envelope(tag, offset, b"PCP2", 0x1c) else { break };
        let len_comment = u32_at(entry, 0x28).map(|v| v as usize);
        let comment_bytes = len_comment.and_then(|n| slice(entry, 0x2c, n));
        let color_at = len_comment.and_then(|n| n.checked_add(0x2c));
        let color = color_at.and_then(|at| slice(entry, at, 4));
        entries.push(ExtendedCuePoint {
            hot_cue: u32_at(entry, 0x0c).unwrap_or(0),
            cue_type: u8_at(entry, 0x10).unwrap_or(0),
            time_ms: u32_at(entry, 0x14).unwrap_or(0),
            loop_time_ms: u32_at(entry, 0x18).unwrap_or(0),
            color_id: u8_at(entry, 0x1c),
            loop_numerator: u16_at(entry, 0x24),
            loop_denominator: u16_at(entry, 0x26),
            comment: comment_bytes.map(utf16be).filter(|c| !c.is_empty()),
            color_code: color.map(|c| c[0]),
            color_rgb: color.map(|c| [c[1], c[2], c[3]]),
        });
        offset += len;
    }
    Ok(ExtendedCueList { list_type, truncated: entries.len() < count, entries })
}

/// A cue entry's own envelope (`fourcc`, `len_header`, `len_entry`) at
/// `offset`: the entry bytes and their length, when the fourcc matches, the
/// entry is at least `min_len` long and it fits in the section.
fn entry_envelope<'a>(tag: &'a [u8], offset: usize, magic: &[u8; 4], min_len: usize) -> Option<(&'a [u8], usize)> {
    if fourcc_at(tag, offset)? != *magic {
        return None;
    }
    let len = u32_at(tag, offset.checked_add(8)?)? as usize;
    if len < min_len {
        return None;
    }
    Some((slice(tag, offset, len)?, len))
}

// -------------------------------------------------------------- path / vbr

fn parse_ppth(tag: &[u8]) -> Parsed<String> {
    let len = u32_at(tag, 0x0c).ok_or("truncated header")? as usize;
    let bytes = slice(tag, 0x10, len).ok_or("path length exceeds section")?;
    Ok(utf16be(bytes))
}

/// PVBR: believed to be a seek index for VBR files. Kept raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VbrIndex {
    pub unknown: u32,
    /// Every u32 after the header (400 index entries and a trailer in
    /// practice).
    pub entries: Vec<u32>,
}

fn parse_pvbr(tag: &[u8]) -> Parsed<VbrIndex> {
    let unknown = u32_at(tag, 0x0c).ok_or("truncated header")?;
    let body = tag.get(0x10..).unwrap_or(&[]);
    let entries = body.chunks_exact(4).map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]])).collect();
    Ok(VbrIndex { unknown, entries })
}

// --------------------------------------------------------------- waveforms

/// Which waveform section a [`Waveform`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaveformKind {
    /// PWAV: 400 columns, low 5 bits height, high 3 bits whiteness.
    Preview,
    /// PWV2: 100 columns, low 4 bits height.
    TinyPreview,
    /// PWV3: 150 columns per second, same byte layout as PWAV.
    Detail,
    /// PWV4: 1200 columns × 6 bytes (colour preview).
    ColorPreview,
    /// PWV5: 150 columns per second, one big-endian u16 each.
    ColorDetail,
    /// PWV6: 1200 columns × 3 bytes, three bands.
    ThreeBandPreview,
    /// PWV7: 150 columns per second × 3 bytes, three bands.
    ThreeBandDetail,
}

impl WaveformKind {
    /// Bytes per column.
    pub const fn entry_bytes(self) -> usize {
        match self {
            WaveformKind::Preview | WaveformKind::TinyPreview | WaveformKind::Detail => 1,
            WaveformKind::ColorDetail => 2,
            WaveformKind::ThreeBandPreview | WaveformKind::ThreeBandDetail => 3,
            WaveformKind::ColorPreview => 6,
        }
    }

    pub const fn fourcc(self) -> &'static str {
        match self {
            WaveformKind::Preview => "PWAV",
            WaveformKind::TinyPreview => "PWV2",
            WaveformKind::Detail => "PWV3",
            WaveformKind::ColorPreview => "PWV4",
            WaveformKind::ColorDetail => "PWV5",
            WaveformKind::ThreeBandPreview => "PWV6",
            WaveformKind::ThreeBandDetail => "PWV7",
        }
    }
}

/// Raw column data of one waveform section; use the typed accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waveform {
    pub kind: WaveformKind,
    /// Header bytes 0x10–0x13 (PWAV/PWV2) or 0x14–0x17 (PWV3/4/5/7); 0 for PWV6.
    pub unknown: u32,
    /// `columns() * kind.entry_bytes()` bytes.
    pub data: Vec<u8>,
}

/// One monochrome column (PWAV, PWV2, PWV3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonoColumn {
    pub height: u8,
    /// 0–7 (PWV2 has none: 0).
    pub whiteness: u8,
}

/// One PWV5 column: 3-bit colour channels, 5-bit height, 2 low bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorColumn {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub height: u8,
    /// The two low-order bits (unused per Deep Symmetry; finer height per
    /// other readers).
    pub low_bits: u8,
}

/// One PWV6/PWV7 column. Byte order is low, mid, high: Deep Symmetry's text
/// says mid, high, low, but band-energy correlation against decoded audio on
/// real files showed low, mid, high (parity research, RB-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreeBandColumn {
    pub low: u8,
    pub mid: u8,
    pub high: u8,
}

impl Waveform {
    pub fn columns(&self) -> usize {
        self.data.len() / self.kind.entry_bytes()
    }

    /// PWAV / PWV2 / PWV3 columns; empty for other kinds.
    pub fn mono(&self) -> impl Iterator<Item = MonoColumn> + '_ {
        let (height_mask, white_shift) = match self.kind {
            WaveformKind::Preview | WaveformKind::Detail => (0x1f, Some(5)),
            WaveformKind::TinyPreview => (0x0f, None),
            _ => (0, None),
        };
        let data: &[u8] = if height_mask == 0 { &[] } else { &self.data };
        data.iter().map(move |&b| MonoColumn {
            height: b & height_mask,
            whiteness: white_shift.map_or(0, |s| b >> s),
        })
    }

    /// PWV5 columns; empty for other kinds.
    pub fn color_detail(&self) -> impl Iterator<Item = ColorColumn> + '_ {
        let data: &[u8] = if self.kind == WaveformKind::ColorDetail { &self.data } else { &[] };
        data.chunks_exact(2).map(|p| {
            let v = u16::from_be_bytes([p[0], p[1]]);
            ColorColumn {
                red: (v >> 13) as u8 & 0x7,
                green: (v >> 10) as u8 & 0x7,
                blue: (v >> 7) as u8 & 0x7,
                height: (v >> 2) as u8 & 0x1f,
                low_bits: v as u8 & 0x3,
            }
        })
    }

    /// PWV4 columns as raw 6-byte groups; empty for other kinds.
    pub fn color_preview(&self) -> impl Iterator<Item = [u8; 6]> + '_ {
        let data: &[u8] = if self.kind == WaveformKind::ColorPreview { &self.data } else { &[] };
        data.chunks_exact(6).map(|c| [c[0], c[1], c[2], c[3], c[4], c[5]])
    }

    /// PWV6 / PWV7 columns; empty for other kinds.
    pub fn three_band(&self) -> impl Iterator<Item = ThreeBandColumn> + '_ {
        let data: &[u8] = match self.kind {
            WaveformKind::ThreeBandPreview | WaveformKind::ThreeBandDetail => &self.data,
            _ => &[],
        };
        data.chunks_exact(3).map(|c| ThreeBandColumn { low: c[0], mid: c[1], high: c[2] })
    }
}

/// PWAV / PWV2: `len_preview` at 0x0c, unknown at 0x10, data from 0x14.
fn parse_preview(tag: &[u8], kind: WaveformKind) -> Parsed<Waveform> {
    let len = u32_at(tag, 0x0c).ok_or("truncated header")? as usize;
    let unknown = u32_at(tag, 0x10).ok_or("truncated header")?;
    let data = slice(tag, 0x14, len).ok_or("preview length exceeds section")?;
    Ok(Waveform { kind, unknown, data: data.to_vec() })
}

/// PWV3–PWV7: `len_entry_bytes` at 0x0c, `len_entries` at 0x10, then (except
/// PWV6) an unknown u32 at 0x14; entries from `data_at`.
fn parse_table(tag: &[u8], kind: WaveformKind, data_at: usize) -> Parsed<Waveform> {
    let entry_bytes = u32_at(tag, 0x0c).ok_or("truncated header")? as usize;
    if entry_bytes != kind.entry_bytes() {
        return Err("unexpected entry size");
    }
    let count = u32_at(tag, 0x10).ok_or("truncated header")? as usize;
    let unknown = if data_at > 0x14 { u32_at(tag, 0x14).ok_or("truncated header")? } else { 0 };
    let data = array(tag, data_at, count, entry_bytes).ok_or("entry count exceeds section")?;
    Ok(Waveform { kind, unknown, data: data.to_vec() })
}

/// PWVC: an unknown u16 then three u16 values (believed to be low/mid/high
/// band gain calibration). Kept raw; the reserved value is not enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaveformCalibration {
    pub unknown: u16,
    pub values: [u16; 3],
}

fn parse_pwvc(tag: &[u8]) -> Parsed<WaveformCalibration> {
    let read = |at| u16_at(tag, at).ok_or("truncated section");
    Ok(WaveformCalibration { unknown: read(0x0c)?, values: [read(0x0e)?, read(0x10)?, read(0x12)?] })
}
