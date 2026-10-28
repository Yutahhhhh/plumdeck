//! MP4 / M4A probing (spec section 4).

use std::io::{Read, Seek};

use super::io::{be_u16, be_u32, be_u64, Bits, Src};
use super::{Codec, Container, PrimingSource, ProbeInfo, TimingCase};
use crate::error::{DecodeError, Result};

const MAX_DEPTH: u32 = 8;
const MAX_BOXES: u32 = 4096;
/// Cap for boxes read into memory (`stsd`, `elst`, freeform items).
const SMALL_BOX: usize = 64 * 1024;
const MAX_EDITS: usize = 64;

/// MP4-specific probe details.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mp4Details {
    pub major_brand: String,
    /// Four-character code of the audio sample entry (`mp4a`, `alac`, ...).
    pub sample_entry: String,
    pub object_type_indication: Option<u8>,
    /// MPEG-4 audio object type from the AudioSpecificConfig.
    pub audio_object_type: Option<u8>,
    pub asc_sample_rate: Option<u32>,
    pub entry_sample_rate: Option<u32>,
    pub mvhd_timescale: u32,
    pub mdhd_timescale: u32,
    pub mdhd_duration: u64,
    pub audio_tracks: u32,
    pub elst_entries: u32,
    pub elst_empty_edits: u32,
    /// First non-empty edit: media time (mdhd units).
    pub elst_media_time: Option<i64>,
    /// First non-empty edit: segment duration (mvhd units).
    pub elst_segment_duration: Option<u64>,
    /// iTunSMPB `(priming, padding, original length)`.
    pub itunsmpb: Option<(u64, u64, u64)>,
}

#[derive(Clone, Copy)]
struct BoxHdr {
    typ: [u8; 4],
    body: u64,
    end: u64,
}

fn read_box<R: Read + Seek>(src: &mut Src<'_, R>, pos: u64, limit: u64) -> Result<Option<BoxHdr>> {
    if pos + 8 > limit {
        return Ok(None);
    }
    let mut h = [0u8; 16];
    let n = src.read_at_most(pos, &mut h)?;
    if n < 8 {
        return Ok(None);
    }
    let size32 = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
    let typ = [h[4], h[5], h[6], h[7]];
    let (body, end) = match size32 {
        0 => (pos + 8, limit),
        1 => {
            if n < 16 {
                return Ok(None);
            }
            let large = u64::from_be_bytes(h[8..16].try_into().unwrap_or_default());
            (pos + 16, pos.saturating_add(large))
        }
        s => (pos + 8, pos + s),
    };
    if end < body || end > limit {
        return Ok(None);
    }
    Ok(Some(BoxHdr { typ, body, end }))
}

#[derive(Default)]
struct Trak {
    handler: [u8; 4],
    mdhd_timescale: u32,
    mdhd_duration: u64,
    elst: Vec<(u64, i64)>,
    elst_entries: u32,
    stsd: Option<Vec<u8>>,
}

#[derive(Default)]
struct Walk {
    budget: u32,
    mvhd_timescale: u32,
    traks: Vec<Trak>,
    itunsmpb: Option<(u64, u64, u64)>,
}

impl Walk {
    fn tick(&mut self) -> bool {
        self.budget += 1;
        self.budget <= MAX_BOXES
    }
}

fn full_box_times(b: &[u8]) -> Option<(u32, u64)> {
    // version(1) flags(3), then v0: c u32, m u32, timescale u32, duration u32;
    // v1: c u64, m u64, timescale u32, duration u64.
    match *b.first()? {
        0 => Some((be_u32(b, 12)?, u64::from(be_u32(b, 16)?))),
        1 => Some((be_u32(b, 20)?, be_u64(b, 24)?)),
        _ => None,
    }
}

fn walk<R: Read + Seek>(src: &mut Src<'_, R>, w: &mut Walk, start: u64, end: u64, depth: u32) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let mut pos = start;
    while let Some(b) = read_box(src, pos, end)? {
        if !w.tick() {
            return Ok(());
        }
        match &b.typ {
            b"moov" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"udta" | b"ilst" => {
                walk(src, w, b.body, b.end, depth + 1)?;
            }
            b"trak" => {
                w.traks.push(Trak::default());
                walk(src, w, b.body, b.end, depth + 1)?;
            }
            b"meta" => {
                // ISO meta is a full box; QuickTime's is not (spec 4.1).
                let mut peek = [0u8; 8];
                let n = src.read_at_most(b.body, &mut peek)?;
                let qt = n == 8 && be_u32(&peek, 0).unwrap_or(0) >= 8 && peek[4..8].iter().all(u8::is_ascii_alphanumeric);
                let body = if qt { b.body } else { b.body + 4 };
                walk(src, w, body.min(b.end), b.end, depth + 1)?;
            }
            b"mvhd" => {
                let data = src.read_vec(b.body, b.end - b.body, 64)?;
                if let Some((ts, _)) = full_box_times(&data) {
                    w.mvhd_timescale = ts;
                }
            }
            b"mdhd" => {
                let data = src.read_vec(b.body, b.end - b.body, 64)?;
                if let (Some(t), Some((ts, dur))) = (w.traks.last_mut(), full_box_times(&data)) {
                    t.mdhd_timescale = ts;
                    t.mdhd_duration = dur;
                }
            }
            b"hdlr" => {
                let data = src.read_vec(b.body, b.end - b.body, 64)?;
                if let (Some(t), Some(h)) = (w.traks.last_mut(), data.get(8..12)) {
                    t.handler.copy_from_slice(h);
                }
            }
            b"elst" => {
                let data = src.read_vec(b.body, b.end - b.body, SMALL_BOX)?;
                if let Some(t) = w.traks.last_mut() {
                    parse_elst(&data, t);
                }
            }
            b"stsd" => {
                if let Some(t) = w.traks.last_mut() {
                    if t.stsd.is_none() {
                        t.stsd = Some(src.read_vec(b.body, b.end - b.body, SMALL_BOX)?);
                    }
                }
            }
            b"----" => {
                if b.end - b.body <= SMALL_BOX as u64 {
                    let data = src.read_vec(b.body, b.end - b.body, SMALL_BOX)?;
                    if let Some(v) = parse_freeform_itunsmpb(&data) {
                        w.itunsmpb = Some(v);
                    }
                }
            }
            _ => {}
        }
        if b.end <= pos {
            break;
        }
        pos = b.end;
    }
    Ok(())
}

fn parse_elst(b: &[u8], t: &mut Trak) {
    let Some(&version) = b.first() else { return };
    let Some(count) = be_u32(b, 4) else { return };
    t.elst_entries = count;
    let entry_len = if version == 1 { 20 } else { 12 };
    for i in 0..(count as usize).min(MAX_EDITS) {
        let at = 8 + i * entry_len;
        let entry = if version == 1 {
            be_u64(b, at).zip(be_u64(b, at + 8).map(|v| v as i64))
        } else {
            be_u32(b, at).map(u64::from).zip(be_u32(b, at + 4).map(|v| i64::from(v as i32)))
        };
        let Some(e) = entry else { break };
        t.elst.push(e);
    }
}

/// Children of a `----` item: `mean`, `name`, `data` (spec 4.3).
fn parse_freeform_itunsmpb(b: &[u8]) -> Option<(u64, u64, u64)> {
    let mut pos = 0usize;
    let (mut mean, mut name, mut data): (&[u8], &[u8], &[u8]) = (&[], &[], &[]);
    while pos + 8 <= b.len() {
        let size = be_u32(b, pos)? as usize;
        if size < 8 || pos + size > b.len() {
            break;
        }
        let body = &b[pos + 8..pos + size];
        match &b[pos + 4..pos + 8] {
            b"mean" => mean = body.get(4..).unwrap_or_default(),
            b"name" => name = body.get(4..).unwrap_or_default(),
            b"data" => data = body.get(8..).unwrap_or_default(),
            _ => {}
        }
        pos += size;
    }
    if mean != b"com.apple.iTunes" || name != b"iTunSMPB" {
        return None;
    }
    parse_itunsmpb_text(std::str::from_utf8(data).ok()?)
}

pub(crate) fn parse_itunsmpb_text(text: &str) -> Option<(u64, u64, u64)> {
    let words: Vec<&str> = text.split_whitespace().take(4).collect();
    if words.len() < 4 {
        return None;
    }
    let hex = |s: &str| u64::from_str_radix(s, 16).ok();
    Some((hex(words[1])?, hex(words[2])?, hex(words[3])?))
}

/// Codec details from the first sample entry.
#[derive(Default)]
struct Entry {
    fourcc: [u8; 4],
    channels: Option<u16>,
    bits: Option<u16>,
    entry_rate: Option<u32>,
    oti: Option<u8>,
    asc: Option<Asc>,
    alac_rate: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asc {
    pub aot: u8,
    pub sample_rate: u32,
    pub channel_config: u8,
    pub sbr: bool,
    pub ps: bool,
    /// Output rate when SBR is signalled.
    pub ext_sample_rate: Option<u32>,
}

const AAC_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

fn read_aot(b: &mut Bits<'_>) -> Option<u8> {
    let aot = b.read(5)?;
    Some(if aot == 31 { 32 + b.read(6)? } else { aot } as u8)
}

fn read_rate(b: &mut Bits<'_>) -> Option<u32> {
    let idx = b.read(4)?;
    if idx == 15 {
        b.read(24)
    } else {
        AAC_RATES.get(idx as usize).copied()
    }
}

/// Parses an AudioSpecificConfig (spec 4.2).
pub(crate) fn parse_asc(data: &[u8]) -> Option<Asc> {
    let mut b = Bits::new(data);
    let mut aot = read_aot(&mut b)?;
    let sample_rate = read_rate(&mut b)?;
    let channel_config = b.read(4)? as u8;
    let mut asc = Asc { aot, sample_rate, channel_config, sbr: false, ps: false, ext_sample_rate: None };
    if aot == 5 || aot == 29 {
        asc.sbr = true;
        asc.ps = aot == 29;
        asc.ext_sample_rate = read_rate(&mut b);
        aot = read_aot(&mut b)?;
        asc.aot = aot;
        return Some(asc);
    }
    if !matches!(aot, 1..=4 | 6 | 7 | 17 | 19..=23) || channel_config == 0 {
        return Some(asc);
    }
    // GASpecificConfig.
    let parse_ga = |b: &mut Bits<'_>| -> Option<()> {
        b.skip(1)?; // frameLengthFlag
        if b.read(1)? == 1 {
            b.skip(14)?; // coreCoderDelay
        }
        let extension = b.read(1)?;
        if matches!(aot, 6 | 20) {
            b.skip(3)?;
        }
        if extension == 1 {
            if aot == 22 {
                b.skip(16)?;
            }
            if matches!(aot, 17 | 19 | 20 | 23) {
                b.skip(3)?;
            }
            b.skip(1)?;
        }
        Some(())
    };
    if parse_ga(&mut b).is_none() {
        return Some(asc);
    }
    if b.remaining() >= 16 && b.read(11) == Some(0x2b7) {
        if read_aot(&mut b) == Some(5) && b.read(1) == Some(1) {
            asc.sbr = true;
            asc.ext_sample_rate = read_rate(&mut b);
            if b.remaining() >= 12 && b.read(11) == Some(0x548) {
                asc.ps = b.read(1) == Some(1);
            }
        }
    }
    Some(asc)
}

/// Reads an MPEG-4 descriptor tag and its variable-length size.
fn read_desc(b: &[u8], p: &mut usize) -> Option<(u8, usize)> {
    let tag = *b.get(*p)?;
    *p += 1;
    let mut len = 0usize;
    for _ in 0..4 {
        let byte = *b.get(*p)?;
        *p += 1;
        len = (len << 7) | usize::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            break;
        }
    }
    Some((tag, len))
}

/// Parses an `esds` body (after version/flags): returns (OTI, ASC bytes).
fn parse_esds(b: &[u8]) -> Option<(u8, Option<&[u8]>)> {
    let mut p = 0;
    let (tag, _) = read_desc(b, &mut p)?;
    if tag != 3 {
        return None;
    }
    p += 2; // ES_ID
    let flags = *b.get(p)?;
    p += 1;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        p += 1 + usize::from(*b.get(p)?);
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    let (tag, _) = read_desc(b, &mut p)?;
    if tag != 4 {
        return None;
    }
    let oti = *b.get(p)?;
    p += 13;
    let asc = match read_desc(b, &mut p) {
        Some((5, len)) => b.get(p..p.checked_add(len)?),
        _ => None,
    };
    Some((oti, asc))
}

/// Searches the child boxes of a sample entry for `esds` / `alac`.
fn scan_entry_children(b: &[u8], e: &mut Entry, depth: u32) {
    if depth > 3 {
        return;
    }
    let mut pos = 0usize;
    while pos + 8 <= b.len() {
        let Some(size) = be_u32(b, pos).map(|s| s as usize) else { break };
        if size < 8 || pos + size > b.len() {
            break;
        }
        let body = &b[pos + 8..pos + size];
        match &b[pos + 4..pos + 8] {
            b"esds" => {
                if let Some((oti, asc)) = body.get(4..).and_then(parse_esds) {
                    e.oti = Some(oti);
                    e.asc = asc.and_then(parse_asc);
                }
            }
            b"wave" => scan_entry_children(body, e, depth + 1),
            b"alac" if body.len() >= 28 => {
                // Full box (4) + ALACSpecificConfig.
                let cfg = &body[4..];
                e.bits = Some(u16::from(cfg[5]));
                e.channels = Some(u16::from(cfg[9]));
                e.alac_rate = be_u32(cfg, 20);
            }
            _ => {}
        }
        pos += size;
    }
}

fn parse_stsd(b: &[u8]) -> Option<Entry> {
    // Full box (4) + entry count (4), then the first entry box.
    let size = be_u32(b, 8)? as usize;
    let entry = b.get(8..8usize.checked_add(size)?.min(b.len()))?;
    let mut e = Entry::default();
    e.fourcc.copy_from_slice(entry.get(4..8)?);
    let body = entry.get(8..)?;
    let version = be_u16(body, 8)?;
    e.channels = be_u16(body, 16);
    e.bits = be_u16(body, 18);
    e.entry_rate = be_u32(body, 24).map(|r| r >> 16).filter(|&r| r > 0);
    let children_at = match version {
        1 => 44,
        2 => {
            e.entry_rate = be_u64(body, 32).map(f64::from_bits).filter(|r| r.is_finite() && *r >= 1.0 && *r < 1e7).map(|r| r.round() as u32);
            e.channels = be_u32(body, 40).and_then(|c| u16::try_from(c).ok());
            64
        }
        _ => 28,
    };
    if let Some(children) = body.get(children_at..) {
        scan_entry_children(children, &mut e, 0);
    }
    Some(e)
}

fn scale(value: u64, rate: u32, timescale: u32) -> u64 {
    if timescale == 0 || timescale == rate {
        return value;
    }
    ((u128::from(value) * u128::from(rate) + u128::from(timescale) / 2) / u128::from(timescale)) as u64
}

pub(crate) fn probe<R: Read + Seek>(src: &mut Src<'_, R>) -> Result<ProbeInfo> {
    let mut info = ProbeInfo::new(Container::Mp4);
    let mut d = Mp4Details::default();
    let mut brand = [0u8; 4];
    src.read_exact_at(8, &mut brand, "ftyp")?;
    d.major_brand = String::from_utf8_lossy(&brand).trim_end().to_owned();

    let mut w = Walk::default();
    let len = src.len;
    walk(src, &mut w, 0, len, 0)?;
    d.mvhd_timescale = w.mvhd_timescale;
    d.itunsmpb = w.itunsmpb;
    d.audio_tracks = w.traks.iter().filter(|t| &t.handler == b"soun").count() as u32;

    let Some(trak) = w.traks.iter().find(|t| &t.handler == b"soun") else {
        return Err(DecodeError::NoAudioTrack);
    };
    let entry = trak.stsd.as_deref().and_then(parse_stsd).ok_or_else(|| DecodeError::malformed("mp4: missing sample description"))?;
    d.sample_entry = String::from_utf8_lossy(&entry.fourcc).into_owned();
    d.object_type_indication = entry.oti;
    d.entry_sample_rate = entry.entry_rate;
    d.mdhd_timescale = trak.mdhd_timescale;
    d.mdhd_duration = trak.mdhd_duration;
    d.elst_entries = trak.elst_entries;
    d.elst_empty_edits = trak.elst.iter().filter(|e| e.1 == -1).count() as u32;
    if let Some(&(seg, mt)) = trak.elst.iter().find(|e| e.1 >= 0) {
        d.elst_media_time = Some(mt);
        d.elst_segment_duration = Some(seg);
    }

    info.channels = entry.channels;
    info.sample_rate = entry.entry_rate;
    match &entry.fourcc {
        b"mp4a" => match entry.oti {
            Some(0x69 | 0x6B) => info.codec = Codec::Mp3,
            _ => {
                if let Some(asc) = entry.asc {
                    d.audio_object_type = Some(asc.aot);
                    d.asc_sample_rate = Some(asc.sample_rate);
                    if asc.channel_config > 0 && asc.channel_config < 8 {
                        info.channels = Some(match asc.channel_config {
                            7 => 8,
                            c => u16::from(c),
                        });
                    }
                    let implicit = !asc.sbr && asc.aot == 2 && entry.entry_rate == Some(asc.sample_rate * 2);
                    info.codec = if asc.sbr || implicit {
                        Codec::HeAac { ps: asc.ps, implicit }
                    } else if asc.aot == 2 {
                        Codec::AacLc
                    } else {
                        Codec::AacOther(asc.aot)
                    };
                    info.sample_rate = if asc.sbr {
                        asc.ext_sample_rate.or(entry.entry_rate)
                    } else if implicit {
                        entry.entry_rate
                    } else {
                        Some(asc.sample_rate)
                    };
                } else {
                    info.codec = Codec::AacLc;
                }
            }
        },
        b"alac" => {
            info.codec = Codec::Alac;
            info.bits_per_sample = entry.bits;
            info.sample_rate = entry.alac_rate.filter(|&r| r > 0).or(entry.entry_rate);
        }
        b"fLaC" => info.codec = Codec::Flac,
        b"Opus" => info.codec = Codec::Opus,
        b".mp3" => info.codec = Codec::Mp3,
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"raw " => {
            info.codec = Codec::Pcm;
            info.bits_per_sample = entry.bits;
        }
        b"fl32" | b"fl64" => info.codec = Codec::PcmFloat,
        _ => info.codec = Codec::Unknown,
    }

    let rate = info.sample_rate.unwrap_or(0);
    if rate > 0 && trak.mdhd_duration > 0 {
        info.frames_hint = Some(scale(trak.mdhd_duration, rate, trak.mdhd_timescale));
        info.duration_exact = true;
    }

    // Priming precedence (spec 4.3).
    let elst_priming = d.elst_media_time.filter(|&mt| mt > 0).map(|mt| scale(mt as u64, rate, trak.mdhd_timescale));
    if let Some(p) = elst_priming {
        info.priming = p;
        info.priming_source = PrimingSource::EditList;
        info.valid_frames = d.elst_segment_duration.filter(|&s| s > 0).map(|s| scale(s, rate, w.mvhd_timescale));
        if let (Some(total), Some(valid)) = (info.frames_hint, info.valid_frames) {
            info.padding = total.checked_sub(p + valid);
        }
    } else if let Some((p, pad, n)) = d.itunsmpb {
        info.priming = p;
        info.padding = Some(pad);
        info.valid_frames = (n > 0).then_some(n);
        info.priming_source = PrimingSource::ITunSmpb;
    } else if let (Some(seg), Some(0)) = (d.elst_segment_duration, d.elst_media_time) {
        // An edit starting at 0 still declares the presentation length.
        info.valid_frames = (seg > 0).then(|| scale(seg, rate, w.mvhd_timescale));
    }
    info.timing_case = if info.priming > 0 { TimingCase::Mp4Priming } else { TimingCase::Mp4NoPriming };
    info.mp4 = Some(d);
    Ok(info)
}
