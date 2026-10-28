//! WAV (RIFF/RF64) and AIFF/AIFC probing (spec section 5).

use std::io::{Read, Seek};

use super::io::{be_u16, be_u32, le_u16, le_u32, le_u64, Src};
use super::{Codec, Container, ProbeInfo, TimingCase};
use crate::error::{DecodeError, Result};

const MAX_CHUNKS: u32 = 4096;

pub(crate) fn probe_wav<R: Read + Seek>(src: &mut Src<'_, R>) -> Result<ProbeInfo> {
    let mut head = [0u8; 12];
    src.read_exact_at(0, &mut head, "RIFF header")?;
    if &head[8..12] != b"WAVE" {
        return Err(DecodeError::Unsupported(crate::Unsupported::Container));
    }
    let rf64 = &head[..4] == b"RF64";
    let mut info = ProbeInfo::new(Container::Wav);
    info.timing_case = TimingCase::Lossless;
    let mut block_align = 0u16;
    let mut ds64_data: Option<u64> = None;
    let mut data_size: Option<u64> = None;
    let mut pos = 12u64;
    for _ in 0..MAX_CHUNKS {
        let mut ch = [0u8; 8];
        if src.read_at_most(pos, &mut ch)? < 8 {
            break;
        }
        let size = u64::from(le_u32(&ch, 4).unwrap_or(0));
        let body = pos + 8;
        match &ch[..4] {
            b"fmt " => {
                let fmt = src.read_vec(body, size, 64)?;
                let mut tag = le_u16(&fmt, 0).ok_or_else(|| DecodeError::malformed("wav: short fmt"))?;
                info.channels = le_u16(&fmt, 2);
                info.sample_rate = le_u32(&fmt, 4);
                block_align = le_u16(&fmt, 12).unwrap_or(0);
                info.bits_per_sample = le_u16(&fmt, 14);
                if tag == 0xFFFE {
                    tag = le_u16(&fmt, 24).unwrap_or(0);
                }
                info.codec = match tag {
                    1 => Codec::Pcm,
                    3 => Codec::PcmFloat,
                    _ => Codec::PcmOther,
                };
            }
            b"ds64" => {
                let ds = src.read_vec(body, size, 64)?;
                ds64_data = le_u64(&ds, 8);
            }
            b"data" => {
                let declared = if rf64 && size == 0xFFFF_FFFF { ds64_data.unwrap_or(size) } else { size };
                data_size = Some(declared.min(src.len.saturating_sub(body)));
                break;
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    let Some(size) = data_size else {
        return Err(DecodeError::malformed("wav: no data chunk"));
    };
    if info.sample_rate.unwrap_or(0) == 0 || block_align == 0 {
        return Err(DecodeError::malformed("wav: missing or invalid fmt chunk"));
    }
    info.frames_hint = Some(size / u64::from(block_align));
    info.duration_exact = true;
    Ok(info)
}

/// 80-bit IEEE 754 extended to f64.
pub(crate) fn extended_to_f64(b: &[u8; 10]) -> f64 {
    let sign = if b[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    let exp = (i32::from(b[0] & 0x7F) << 8) | i32::from(b[1]);
    let mant = u64::from_be_bytes(b[2..10].try_into().unwrap_or_default());
    if exp == 0 && mant == 0 {
        return 0.0;
    }
    if exp == 0x7FFF {
        return f64::NAN;
    }
    sign * (mant as f64) * 2f64.powi(exp - 16383 - 63)
}

pub(crate) fn probe_aiff<R: Read + Seek>(src: &mut Src<'_, R>) -> Result<ProbeInfo> {
    let mut head = [0u8; 12];
    src.read_exact_at(0, &mut head, "FORM header")?;
    let aifc = match &head[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return Err(DecodeError::Unsupported(crate::Unsupported::Container)),
    };
    let mut info = ProbeInfo::new(Container::Aiff);
    info.timing_case = TimingCase::Lossless;
    let mut pos = 12u64;
    for _ in 0..MAX_CHUNKS {
        let mut ch = [0u8; 8];
        if src.read_at_most(pos, &mut ch)? < 8 {
            break;
        }
        let size = u64::from(be_u32(&ch, 4).unwrap_or(0));
        let body = pos + 8;
        if &ch[..4] == b"COMM" {
            let comm = src.read_vec(body, size, 64)?;
            if comm.len() < 18 {
                return Err(DecodeError::malformed("aiff: short COMM"));
            }
            info.channels = be_u16(&comm, 0);
            let frames = be_u32(&comm, 2).unwrap_or(0);
            info.bits_per_sample = be_u16(&comm, 6);
            let mut ext = [0u8; 10];
            ext.copy_from_slice(&comm[8..18]);
            let rate = extended_to_f64(&ext);
            info.sample_rate = (rate.is_finite() && (1.0..1e7).contains(&rate)).then(|| rate.round() as u32);
            info.codec = if aifc {
                match comm.get(18..22) {
                    Some(b"NONE" | b"twos" | b"sowt" | b"in24" | b"in32" | b"raw ") => Codec::Pcm,
                    Some(b"fl32" | b"FL32" | b"fl64" | b"FL64") => Codec::PcmFloat,
                    _ => Codec::PcmOther,
                }
            } else {
                Codec::Pcm
            };
            info.frames_hint = Some(u64::from(frames));
            info.duration_exact = true;
            break;
        }
        pos = body + size + (size & 1);
    }
    if info.sample_rate.is_none() {
        return Err(DecodeError::malformed("aiff: missing COMM"));
    }
    Ok(info)
}
