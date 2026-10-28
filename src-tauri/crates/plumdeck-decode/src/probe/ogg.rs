//! Ogg probing (spec section 7).

use std::io::{Read, Seek};

use super::io::{le_u16, le_u32, le_u64, Src};
use super::{flac, Codec, Container, PrimingSource, ProbeInfo, TimingCase};
use crate::error::{DecodeError, Result};

const TAIL: u64 = 64 * 1024;
const MAX_PAGES: usize = 64;

struct Page {
    granule: u64,
    serial: u32,
    /// Offset of the page body and its length.
    body: u64,
    body_len: u64,
}

fn read_page<R: Read + Seek>(src: &mut Src<'_, R>, pos: u64) -> Result<Option<Page>> {
    let mut h = [0u8; 27];
    if src.read_at_most(pos, &mut h)? < 27 || &h[..4] != b"OggS" {
        return Ok(None);
    }
    let nsegs = usize::from(h[26]);
    let mut segs = [0u8; 255];
    if src.read_at_most(pos + 27, &mut segs[..nsegs])? < nsegs {
        return Ok(None);
    }
    let body_len: u64 = segs[..nsegs].iter().map(|&s| u64::from(s)).sum();
    Ok(Some(Page {
        granule: le_u64(&h, 6).unwrap_or(u64::MAX),
        serial: le_u32(&h, 14).unwrap_or(0),
        body: pos + 27 + nsegs as u64,
        body_len,
    }))
}

pub(crate) fn probe<R: Read + Seek>(src: &mut Src<'_, R>) -> Result<ProbeInfo> {
    let first = read_page(src, 0)?.ok_or_else(|| DecodeError::malformed("ogg: first page"))?;
    let packet = src.read_vec(first.body, first.body_len, 512)?;
    let mut info = ProbeInfo::new(Container::Ogg);
    if packet.starts_with(b"\x01vorbis") {
        info.codec = Codec::Vorbis;
        info.timing_case = TimingCase::OggVorbis;
        info.channels = packet.get(11).map(|&c| u16::from(c));
        info.sample_rate = le_u32(&packet, 12).filter(|&r| r > 0);
    } else if packet.starts_with(b"OpusHead") {
        info.codec = Codec::Opus;
        info.timing_case = TimingCase::OggOpus;
        info.channels = packet.get(9).map(|&c| u16::from(c));
        info.sample_rate = Some(48_000);
        info.priming = u64::from(le_u16(&packet, 10).unwrap_or(0));
        info.priming_source = PrimingSource::OpusHead;
    } else if packet.starts_with(b"\x7FFLAC") && packet.get(9..13) == Some(b"fLaC") {
        info.timing_case = TimingCase::Lossless;
        if let Some(si) = packet.get(17..).and_then(flac::parse_streaminfo) {
            flac::fill(&mut info, &si);
        }
        info.frames_hint = None;
        info.duration_exact = false;
    } else {
        info.codec = Codec::Unknown;
    }

    // First page past the headers with a positive granule (reported only).
    let mut pos = first.body + first.body_len;
    for _ in 0..MAX_PAGES {
        let Some(p) = read_page(src, pos)? else { break };
        if p.serial == first.serial && p.granule != u64::MAX && p.granule > 0 {
            info.ogg_first_granule = Some(p.granule);
            break;
        }
        pos = p.body + p.body_len;
    }

    // Last granule position of this logical stream.
    let tail_start = src.len.saturating_sub(TAIL);
    let tail = src.read_vec(tail_start, TAIL, TAIL as usize)?;
    let mut last: Option<u64> = None;
    let mut i = tail.len().saturating_sub(27);
    loop {
        if tail.get(i..i + 4) == Some(b"OggS") {
            let g = le_u64(&tail, i + 6);
            let s = le_u32(&tail, i + 14);
            if let (Some(g), Some(s)) = (g, s) {
                if s == first.serial && g != u64::MAX {
                    last = Some(g);
                    break;
                }
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    if let Some(g) = last {
        info.frames_hint = Some(g);
        info.duration_exact = true;
        info.valid_frames = Some(g.saturating_sub(info.priming));
    }
    Ok(info)
}
