//! FLAC probing (spec section 5).

use std::io::{Read, Seek};

use super::io::{be_u32, Src};
use super::{Codec, Container, ProbeInfo, TimingCase};
use crate::error::{DecodeError, Result};

/// Parsed STREAMINFO fields.
pub(crate) struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub total: u64,
}

/// Parses a 34-byte STREAMINFO body.
pub(crate) fn parse_streaminfo(b: &[u8]) -> Option<StreamInfo> {
    if b.len() < 18 {
        return None;
    }
    let sample_rate = (u32::from(b[10]) << 12) | (u32::from(b[11]) << 4) | (u32::from(b[12]) >> 4);
    let channels = u16::from((b[12] >> 1) & 7) + 1;
    let bits = ((u16::from(b[12] & 1) << 4) | u16::from(b[13] >> 4)) + 1;
    let total = (u64::from(b[13] & 0xF) << 32) | u64::from(be_u32(b, 14)?);
    Some(StreamInfo { sample_rate, channels, bits, total })
}

pub(crate) fn fill(info: &mut ProbeInfo, si: &StreamInfo) {
    info.codec = Codec::Flac;
    info.sample_rate = (si.sample_rate > 0).then_some(si.sample_rate);
    info.channels = Some(si.channels);
    info.bits_per_sample = Some(si.bits);
    if si.total > 0 {
        info.frames_hint = Some(si.total);
        info.duration_exact = true;
    }
}

/// Probes native FLAC whose `fLaC` marker is at `start`.
pub(crate) fn probe<R: Read + Seek>(src: &mut Src<'_, R>, start: u64) -> Result<ProbeInfo> {
    let mut b = [0u8; 4 + 4 + 34];
    src.read_exact_at(start, &mut b, "FLAC STREAMINFO")?;
    if b[4] & 0x7F != 0 {
        return Err(DecodeError::malformed("flac: first metadata block is not STREAMINFO"));
    }
    let si = parse_streaminfo(&b[8..]).ok_or_else(|| DecodeError::malformed("flac: STREAMINFO"))?;
    let mut info = ProbeInfo::new(Container::Flac);
    info.timing_case = TimingCase::Lossless;
    fill(&mut info, &si);
    Ok(info)
}
