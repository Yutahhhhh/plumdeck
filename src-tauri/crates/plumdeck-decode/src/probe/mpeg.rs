//! MPEG audio and ADTS probing (spec section 3).

use std::io::{Read, Seek};

use super::io::{be_u16, be_u32, Src};
use super::{Codec, Container, PrimingSource, ProbeInfo, TimingCase};
use crate::error::{DecodeError, Result, Unsupported};

/// Decoder filterbank delay plus alignment sample (spec 3.4).
pub const MP3_DECODER_DELAY: u64 = 529;

/// How far past the ID3v2 tags the first frame is searched for.
const SYNC_WINDOW: u64 = 1 << 20;

/// Frames that must follow the candidate back to back.
const CHAIN: usize = 3;

/// Info tag found in the first frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoTag {
    None,
    Xing,
    Info,
    Vbri,
}

/// LAME-format extension of a Xing/Info tag (spec 3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LameTag {
    /// The 9-byte encoder string, trailing NULs and spaces removed.
    pub encoder: String,
    pub revision: u8,
    pub vbr_method: u8,
    pub enc_delay: u16,
    pub enc_padding: u16,
    pub crc_stored: u16,
    pub crc_computed: u16,
}

impl LameTag {
    pub fn crc_valid(&self) -> bool {
        self.crc_stored == self.crc_computed
    }
}

/// MPEG-audio specific probe details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp3Details {
    /// Total bytes of leading ID3v2 tags.
    pub id3v2_bytes: u64,
    /// File offset of the first frame (the info frame, when there is one).
    pub audio_offset: u64,
    /// Bytes skipped between the ID3v2 tags and the first frame.
    pub junk_bytes: u64,
    /// `"1"`, `"2"` or `"2.5"`.
    pub version: &'static str,
    pub layer: u8,
    pub bitrate_kbps: u32,
    pub samples_per_frame: u32,
    pub info_tag: InfoTag,
    pub xing_frames: Option<u32>,
    pub xing_bytes: Option<u32>,
    pub lame: Option<LameTag>,
    pub vbri_delay: Option<u16>,
    pub vbri_frames: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    /// 1 = MPEG-1, 2 = MPEG-2, 25 = MPEG-2.5.
    version: u8,
    layer: u8,
    protected: bool,
    bitrate_kbps: u32,
    sample_rate: u32,
    mono: bool,
    frame_len: usize,
    spf: u32,
}

impl FrameHeader {
    fn version_str(&self) -> &'static str {
        match self.version {
            1 => "1",
            2 => "2",
            _ => "2.5",
        }
    }

    fn same_stream(&self, other: &Self) -> bool {
        self.version == other.version && self.layer == other.layer && self.sample_rate == other.sample_rate
    }

    /// Average bytes per frame at this bitrate (padding ignored).
    fn bytes_per_frame(&self) -> f64 {
        let coef = match (self.layer, self.version) {
            (1, _) => 48.0,
            (3, 2 | 25) => 72.0,
            _ => 144.0,
        };
        coef * f64::from(self.bitrate_kbps) * 1000.0 / f64::from(self.sample_rate)
    }
}

const BITRATES: [[u32; 15]; 5] = [
    // MPEG-1 layer I, II, III
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    // MPEG-2/2.5 layer I, layers II and III
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

/// Parses a 32-bit MPEG audio frame header (spec 3.2).
pub(crate) fn parse_header(h: u32) -> Option<FrameHeader> {
    if h >> 21 != 0x7FF {
        return None;
    }
    let version = match (h >> 19) & 3 {
        0 => 25,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let layer = match (h >> 17) & 3 {
        1 => 3,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let protected = (h >> 16) & 1 == 0;
    let br_index = ((h >> 12) & 0xF) as usize;
    if br_index == 0 || br_index == 15 {
        return None;
    }
    let sr_index = ((h >> 10) & 3) as usize;
    if sr_index == 3 {
        return None;
    }
    let padding = (h >> 9) & 1;
    let mono = (h >> 6) & 3 == 3;
    let table = match (version, layer) {
        (1, l) => (l - 1) as usize,
        (_, 1) => 3,
        _ => 4,
    };
    let bitrate_kbps = BITRATES[table][br_index];
    let base = [44_100, 48_000, 32_000][sr_index];
    let sample_rate = match version {
        1 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let br = bitrate_kbps * 1000;
    let frame_len = match layer {
        1 => (12 * br / sample_rate + padding) * 4,
        3 if version != 1 => 72 * br / sample_rate + padding,
        _ => 144 * br / sample_rate + padding,
    } as usize;
    let spf = match (layer, version) {
        (1, _) => 384,
        (3, 2 | 25) => 576,
        _ => 1152,
    };
    if frame_len < 4 {
        return None;
    }
    Some(FrameHeader { version, layer, protected, bitrate_kbps, sample_rate, mono, frame_len, spf })
}

/// Skips consecutive ID3v2 tags starting at `start` (spec 3.1).
pub(crate) fn skip_id3v2<R: Read + Seek>(src: &mut Src<'_, R>, start: u64) -> Result<u64> {
    let mut pos = start;
    for _ in 0..16 {
        let mut h = [0u8; 10];
        if src.read_at_most(pos, &mut h)? < 10 || &h[..3] != b"ID3" || h[3] == 0xFF || h[4] == 0xFF {
            break;
        }
        if h[6..10].iter().any(|b| b & 0x80 != 0) {
            break;
        }
        let size = h[6..10].iter().fold(0u64, |acc, &b| (acc << 7) | u64::from(b));
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        let next = pos + 10 + size + footer;
        if next > src.len {
            break;
        }
        pos = next;
    }
    Ok(pos)
}

/// CRC-16 used by the LAME tag (spec 3.4).
pub(crate) fn crc16_lame(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

fn read_u32_at<R: Read + Seek>(src: &mut Src<'_, R>, off: u64) -> Result<Option<u32>> {
    let mut b = [0u8; 4];
    Ok((src.read_at_most(off, &mut b)? == 4).then(|| u32::from_be_bytes(b)))
}

/// Checks that `CHAIN` frames follow the candidate at `off` back to back.
fn chain_ok<R: Read + Seek>(src: &mut Src<'_, R>, off: u64, first: &FrameHeader) -> Result<bool> {
    let mut pos = off + first.frame_len as u64;
    for _ in 0..CHAIN {
        if pos + 4 > src.len {
            return Ok(true);
        }
        let Some(h) = read_u32_at(src, pos)?.and_then(parse_header) else {
            return Ok(false);
        };
        if !h.same_stream(first) {
            return Ok(false);
        }
        pos += h.frame_len as u64;
    }
    Ok(true)
}

/// ADTS frame header fields needed by the probe.
#[derive(Debug, Clone, Copy)]
struct AdtsHeader {
    profile: u8,
    sample_rate: u32,
    channels: u8,
    frame_len: usize,
}

const AAC_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

fn parse_adts(b: &[u8]) -> Option<AdtsHeader> {
    if b.len() < 7 || b[0] != 0xFF || b[1] & 0xF6 != 0xF0 {
        return None;
    }
    let profile = b[2] >> 6;
    let sfi = ((b[2] >> 2) & 0xF) as usize;
    let sample_rate = *AAC_RATES.get(sfi)?;
    let channels = ((b[2] & 1) << 2) | (b[3] >> 6);
    let frame_len = (((b[3] & 3) as usize) << 11) | ((b[4] as usize) << 3) | ((b[5] >> 5) as usize);
    let header_len = if b[1] & 1 == 0 { 9 } else { 7 };
    if frame_len < header_len {
        return None;
    }
    Some(AdtsHeader { profile, sample_rate, channels, frame_len })
}

fn adts_chain_ok<R: Read + Seek>(src: &mut Src<'_, R>, off: u64, first: &AdtsHeader) -> Result<bool> {
    let mut pos = off + first.frame_len as u64;
    for _ in 0..CHAIN {
        let mut b = [0u8; 7];
        if src.read_at_most(pos, &mut b)? < 7 {
            return Ok(true);
        }
        let Some(h) = parse_adts(&b) else { return Ok(false) };
        if h.sample_rate != first.sample_rate || h.profile != first.profile {
            return Ok(false);
        }
        pos += h.frame_len as u64;
    }
    Ok(true)
}

/// Audio byte range end, excluding ID3v1 and APEv2 trailers (spec 3.1).
fn audio_end<R: Read + Seek>(src: &mut Src<'_, R>, start: u64) -> Result<u64> {
    let mut end = src.len;
    if end >= start + 128 {
        let mut tag = [0u8; 3];
        if src.read_at_most(end - 128, &mut tag)? == 3 && &tag == b"TAG" {
            end -= 128;
        }
    }
    if end >= start + 32 {
        let mut footer = [0u8; 32];
        if src.read_at_most(end - 32, &mut footer)? == 32 && &footer[..8] == b"APETAGEX" {
            let size = u64::from(u32::from_le_bytes(footer[12..16].try_into().unwrap_or_default()));
            let flags = u32::from_le_bytes(footer[20..24].try_into().unwrap_or_default());
            let total = size + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
            if total <= end - start {
                end -= total;
            }
        }
    }
    Ok(end)
}

/// Probes MPEG audio or ADTS starting after the ID3v2 tags at `start`.
pub(crate) fn probe<R: Read + Seek>(src: &mut Src<'_, R>, start: u64) -> Result<ProbeInfo> {
    let window = src.read_vec(start, SYNC_WINDOW + 4, (SYNC_WINDOW + 4) as usize)?;
    let mut i = 0usize;
    while i + 4 <= window.len() {
        if window[i] != 0xFF || window[i + 1] & 0xE0 != 0xE0 {
            i += 1;
            continue;
        }
        let off = start + i as u64;
        if let Some(adts) = parse_adts(&window[i..]) {
            if adts_chain_ok(src, off, &adts)? {
                return probe_adts(src, off, adts);
            }
        } else if let Some(h) = parse_header(u32::from_be_bytes([window[i], window[i + 1], window[i + 2], window[i + 3]])) {
            if chain_ok(src, off, &h)? {
                return probe_mpeg(src, start, off, h);
            }
        }
        i += 1;
    }
    Err(DecodeError::Unsupported(Unsupported::Container))
}

fn probe_adts<R: Read + Seek>(src: &mut Src<'_, R>, off: u64, first: AdtsHeader) -> Result<ProbeInfo> {
    let mut info = ProbeInfo::new(Container::Adts);
    info.codec = match first.profile {
        1 => Codec::AacLc,
        p => Codec::AacOther(p + 1),
    };
    info.sample_rate = Some(first.sample_rate);
    info.channels = (first.channels != 0).then_some(u16::from(first.channels));
    info.timing_case = TimingCase::Adts;
    // Count frames (each holds 1024 samples per raw data block), bounded.
    let mut pos = off;
    let mut frames: u64 = 0;
    let mut complete = false;
    for _ in 0..1_000_000 {
        let mut b = [0u8; 7];
        if src.read_at_most(pos, &mut b)? < 7 {
            complete = true;
            break;
        }
        let Some(h) = parse_adts(&b) else {
            complete = pos >= src.len.saturating_sub(128);
            break;
        };
        frames += 1 + u64::from(b[6] & 3);
        pos += h.frame_len as u64;
    }
    info.frames_hint = Some(frames * 1024);
    info.duration_exact = complete;
    Ok(info)
}

fn probe_mpeg<R: Read + Seek>(src: &mut Src<'_, R>, id3_end: u64, off: u64, h: FrameHeader) -> Result<ProbeInfo> {
    let mut info = ProbeInfo::new(Container::MpegAudio);
    info.codec = match h.layer {
        1 => Codec::Mp1,
        2 => Codec::Mp2,
        _ => Codec::Mp3,
    };
    info.sample_rate = Some(h.sample_rate);
    info.channels = Some(if h.mono { 1 } else { 2 });

    let mut frame = vec![0u8; h.frame_len];
    let got = src.read_at_most(off, &mut frame)?;
    frame.truncate(got);

    let mut d = Mp3Details {
        id3v2_bytes: id3_end,
        audio_offset: off,
        junk_bytes: off - id3_end,
        version: h.version_str(),
        layer: h.layer,
        bitrate_kbps: h.bitrate_kbps,
        samples_per_frame: h.spf,
        info_tag: InfoTag::None,
        xing_frames: None,
        xing_bytes: None,
        lame: None,
        vbri_delay: None,
        vbri_frames: None,
    };

    if h.layer == 3 {
        read_info_tags(&frame, &h, &mut d);
    }

    let spf = u64::from(h.spf);
    info.timing_case = match (&d.info_tag, &d.lame) {
        (InfoTag::Vbri, _) => TimingCase::Mp3Vbri,
        (InfoTag::None, _) => TimingCase::Mp3NoInfo,
        (_, None) => TimingCase::Mp3XingNoLame,
        (_, Some(l)) if !l.crc_valid() => TimingCase::Mp3LameTagBadCrc,
        (_, Some(l)) if l.encoder.starts_with("Lavc") || l.encoder.starts_with("Lavf") => TimingCase::Mp3LavcTag,
        (_, Some(_)) => TimingCase::Mp3LameTag,
    };

    let declared_frames = d.xing_frames.or(d.vbri_frames).filter(|&n| n > 0);
    if let Some(n) = declared_frames {
        info.frames_hint = Some(u64::from(n) * spf);
        info.duration_exact = true;
    } else {
        let end = audio_end(src, off)?;
        // Without a header frame count, the first frame is audio.
        let bytes = end.saturating_sub(off) as f64;
        let bpf = h.bytes_per_frame();
        if bpf > 0.0 {
            info.frames_hint = Some((bytes / bpf).round() as u64 * spf);
        }
    }

    if let Some(lame) = &d.lame {
        info.priming = u64::from(lame.enc_delay) + MP3_DECODER_DELAY;
        info.padding = Some(u64::from(lame.enc_padding));
        info.priming_source = PrimingSource::LameTag;
        if let (Some(total), true) = (info.frames_hint, info.duration_exact) {
            let trim = u64::from(lame.enc_delay) + u64::from(lame.enc_padding);
            info.valid_frames = total.checked_sub(trim);
        }
    }
    info.mp3 = Some(d);
    Ok(info)
}

/// Reads Xing/Info (+ LAME extension) or VBRI from the first frame.
fn read_info_tags(frame: &[u8], h: &FrameHeader, d: &mut Mp3Details) {
    let side_info = match (h.version, h.mono) {
        (1, true) => 17,
        (1, false) => 32,
        (_, true) => 9,
        (_, false) => 17,
    };
    let xing_at = 4 + if h.protected { 2 } else { 0 } + side_info;
    if let Some(id) = frame.get(xing_at..xing_at + 4) {
        if id == b"Xing" || id == b"Info" {
            d.info_tag = if id == b"Xing" { InfoTag::Xing } else { InfoTag::Info };
            let Some(flags) = be_u32(frame, xing_at + 4) else { return };
            let mut p = xing_at + 8;
            if flags & 1 != 0 {
                d.xing_frames = be_u32(frame, p);
                p += 4;
            }
            if flags & 2 != 0 {
                d.xing_bytes = be_u32(frame, p);
                p += 4;
            }
            if flags & 4 != 0 {
                p += 100;
            }
            if flags & 8 != 0 {
                p += 4;
            }
            d.lame = read_lame(frame, p);
            return;
        }
    }
    if frame.get(36..40) == Some(b"VBRI") {
        d.info_tag = InfoTag::Vbri;
        d.vbri_delay = be_u16(frame, 42);
        d.vbri_frames = be_u32(frame, 50);
    }
}

fn read_lame(frame: &[u8], at: usize) -> Option<LameTag> {
    let ext = frame.get(at..at + 36)?;
    let enc = &ext[..9];
    if !enc[..4].iter().all(|b| (0x20..=0x7E).contains(b)) || enc[..4].iter().all(|&b| b == b' ') {
        return None;
    }
    let encoder: String = enc
        .iter()
        .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '\0' })
        .collect::<String>()
        .trim_end_matches(['\0', ' '])
        .to_owned();
    let trim = (u32::from(ext[21]) << 16) | (u32::from(ext[22]) << 8) | u32::from(ext[23]);
    let crc_stored = be_u16(ext, 34)?;
    let crc_computed = crc16_lame(&frame[..at + 34]);
    Some(LameTag {
        encoder,
        revision: ext[9] >> 4,
        vbr_method: ext[9] & 0xF,
        enc_delay: (trim >> 12) as u16,
        enc_padding: (trim & 0xFFF) as u16,
        crc_stored,
        crc_computed,
    })
}
