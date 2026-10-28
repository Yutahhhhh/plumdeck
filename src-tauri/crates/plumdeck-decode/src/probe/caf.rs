//! CAF probing (spec section 6).

use std::io::{Read, Seek};

use super::io::{be_u32, be_u64, Src};
use super::{Codec, Container, PrimingSource, ProbeInfo, TimingCase};
use crate::error::Result;

const MAX_CHUNKS: u32 = 4096;

pub(crate) fn probe<R: Read + Seek>(src: &mut Src<'_, R>) -> Result<ProbeInfo> {
    let mut info = ProbeInfo::new(Container::Caf);
    info.timing_case = TimingCase::Lossless;
    let mut bytes_per_packet = 0u32;
    let mut frames_per_packet = 0u32;
    let mut pakt: Option<(u64, u64, u64)> = None;
    let mut data_bytes: Option<u64> = None;
    let mut pos = 8u64;
    for _ in 0..MAX_CHUNKS {
        let mut ch = [0u8; 12];
        if src.read_at_most(pos, &mut ch)? < 12 {
            break;
        }
        let raw = be_u64(&ch, 4).unwrap_or(0);
        let body = pos + 12;
        let size = if raw == u64::MAX { src.len.saturating_sub(body) } else { raw };
        match &ch[..4] {
            b"desc" => {
                let d = src.read_vec(body, size, 64)?;
                let rate = be_u64(&d, 0).map(f64::from_bits).unwrap_or(0.0);
                info.sample_rate = (rate.is_finite() && (1.0..1e7).contains(&rate)).then(|| rate.round() as u32);
                info.codec = match d.get(8..12) {
                    Some(b"lpcm") => Codec::Pcm,
                    Some(b"alac") => Codec::Alac,
                    Some(b"aac ") => Codec::AacLc,
                    Some(b"aach") => Codec::HeAac { ps: false, implicit: false },
                    Some(b"aacp") => Codec::HeAac { ps: true, implicit: false },
                    Some(b".mp3") => Codec::Mp3,
                    Some(b"flac") => Codec::Flac,
                    Some(b"opus") => Codec::Opus,
                    _ => Codec::Unknown,
                };
                if info.codec == Codec::Pcm && be_u32(&d, 12).unwrap_or(0) & 1 != 0 {
                    info.codec = Codec::PcmFloat;
                }
                bytes_per_packet = be_u32(&d, 16).unwrap_or(0);
                frames_per_packet = be_u32(&d, 20).unwrap_or(0);
                info.channels = be_u32(&d, 24).and_then(|c| u16::try_from(c).ok());
                info.bits_per_sample = be_u32(&d, 28).and_then(|c| u16::try_from(c).ok()).filter(|&b| b > 0);
            }
            b"pakt" => {
                let d = src.read_vec(body, size, 64)?;
                if let (Some(valid), Some(prime), Some(rem)) = (be_u64(&d, 8), be_u32(&d, 16), be_u32(&d, 20)) {
                    pakt = Some((valid, u64::from(prime), u64::from(rem)));
                }
            }
            b"data" => data_bytes = Some(size.saturating_sub(4).min(src.len.saturating_sub(body + 4))),
            _ => {}
        }
        let Some(next) = body.checked_add(size) else { break };
        pos = next;
    }
    if let Some((valid, prime, rem)) = pakt {
        if valid <= i64::MAX as u64 {
            info.frames_hint = Some(valid + prime + rem);
            info.duration_exact = true;
            info.valid_frames = Some(valid);
            info.padding = Some(rem);
            if prime > 0 {
                info.priming = prime;
                info.priming_source = PrimingSource::CafPakt;
                info.timing_case = TimingCase::CafPakt;
            }
        }
    } else if let (Some(bytes), true) = (data_bytes, bytes_per_packet > 0 && frames_per_packet > 0) {
        info.frames_hint = Some((bytes / u64::from(bytes_per_packet)).saturating_mul(u64::from(frames_per_packet)));
        info.duration_exact = true;
    }
    Ok(info)
}
