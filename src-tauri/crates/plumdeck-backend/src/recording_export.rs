//! Recording export without FFmpeg (deviation D-04): the recorder's file is
//! decoded with plumdeck-decode and written as 16/24-bit PCM WAV or FLAC
//! (flacenc, Apache-2.0). MP3 export is dropped (owner decision: WAV/FLAC).
//!
//! Both writers stream: a block is decoded, quantized and written before the
//! next is read, so multi-hour mixes never sit in memory. Header fields that
//! depend on the length (WAV sizes, FLAC STREAMINFO) are patched at the end.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, Stream};
use flacenc::error::Verify;
use flacenc::source::{Context, Fill, FrameBuf};

/// Frames per FLAC block (the reference encoder's default for 44.1/48 kHz).
const FLAC_BLOCK: usize = 4096;

/// Output bit depth: 24 when the source carries more than 16 bits (24/32-bit
/// PCM, float WAV), else 16 (16-bit PCM and lossy sources).
fn output_bits(source_bits: Option<u16>) -> u16 {
    match source_bits {
        Some(bits) if bits > 16 => 24,
        _ => 16,
    }
}

fn quantize(sample: f32, bits: u16) -> i32 {
    let scale = (1i64 << (bits - 1)) as f64;
    let value = (f64::from(sample) * scale).round();
    value.clamp(-scale, scale - 1.0) as i32
}

/// Writes `source` to `target` as `format` ("wav" or "flac").
pub fn export(source: &Path, target: &Path, format: &str) -> Result<(), String> {
    let mut decoder = plumdeck_decode::open(source, plumdeck_decode::Basis::Canonical).map_err(|e| e.to_string())?;
    let channels = decoder.channels();
    let rate = decoder.sample_rate();
    let bits = output_bits(decoder.info().bits_per_sample);
    if channels == 0 || rate == 0 {
        return Err("音声トラックがありません".into());
    }
    let file = File::create(target).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    let mut block = Vec::new();
    let mut ints: Vec<i32> = Vec::new();
    let mut next = |ints: &mut Vec<i32>| -> Result<bool, String> {
        let frames = decoder.read_block(&mut block).map_err(|e| e.to_string())?;
        ints.clear();
        ints.extend(block[..frames * channels].iter().map(|&s| quantize(s, bits)));
        Ok(frames > 0)
    };
    let frames = match format {
        "wav" => write_wav(&mut out, channels, rate, bits, &mut ints, &mut next)?,
        "flac" => write_flac(&mut out, channels, rate, bits, &mut ints, &mut next)?,
        other => return Err(format!("unsupported format {other}")),
    };
    out.flush().map_err(|e| e.to_string())?;
    if frames == 0 {
        return Err("変換結果が空です".into());
    }
    Ok(())
}

type Next<'a> = dyn FnMut(&mut Vec<i32>) -> Result<bool, String> + 'a;

fn write_wav<W: Write + Seek>(out: &mut W, channels: usize, rate: u32, bits: u16, ints: &mut Vec<i32>, next: &mut Next<'_>) -> Result<u64, String> {
    let io = |e: std::io::Error| e.to_string();
    let bytes = usize::from(bits / 8);
    let block_align = (channels * bytes) as u16;
    let header = |data_len: u32| {
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&(channels as u16).to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
        h.extend_from_slice(&block_align.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data_len.to_le_bytes());
        h
    };
    out.write_all(&header(0)).map_err(io)?;
    let mut data_len: u64 = 0;
    let mut buffer = Vec::new();
    while next(ints)? {
        buffer.clear();
        for &v in ints.iter() {
            buffer.extend_from_slice(&v.to_le_bytes()[..bytes]);
        }
        out.write_all(&buffer).map_err(io)?;
        data_len += buffer.len() as u64;
    }
    if data_len > u64::from(u32::MAX - 36) {
        return Err("WAV の上限（4GB）を超えています。FLAC で書き出してください".into());
    }
    out.seek(SeekFrom::Start(0)).map_err(io)?;
    out.write_all(&header(data_len as u32)).map_err(io)?;
    Ok(data_len / u64::from(block_align))
}

fn write_flac<W: Write + Seek>(out: &mut W, channels: usize, rate: u32, bits: u16, ints: &mut Vec<i32>, next: &mut Next<'_>) -> Result<u64, String> {
    let err = |e: &dyn std::fmt::Debug| format!("{e:?}");
    let config = flacenc::config::Encoder::default().into_verified().map_err(|e| err(&e))?;
    let bits = usize::from(bits);
    let mut stream = Stream::new(rate as usize, channels, bits).map_err(|e| err(&e))?;
    // 0 = unknown frame sizes; fixed block size like the reference encoder.
    stream.stream_info_mut().set_frame_sizes(0, 0).map_err(|e| err(&e))?;
    stream.stream_info_mut().set_block_sizes(FLAC_BLOCK, FLAC_BLOCK).map_err(|e| err(&e))?;
    let header = |stream: &Stream| -> Result<Vec<u8>, String> {
        let mut sink = MemSink::<u8>::new();
        stream.write(&mut sink).map_err(|e| err(&e))?;
        Ok(sink.into_inner())
    };
    // STREAMINFO has a fixed size, so the final header overwrites this one.
    let placeholder = header(&stream)?;
    out.write_all(&placeholder).map_err(|e| e.to_string())?;
    let mut framebuf = FrameBuf::with_size(channels, FLAC_BLOCK).map_err(|e| err(&e))?;
    let mut context = Context::new(bits, channels);
    let mut pending: Vec<i32> = Vec::with_capacity(FLAC_BLOCK * channels * 2);
    let mut sink = MemSink::<u8>::new();
    let mut total: u64 = 0;
    let mut done = false;
    while !done {
        while pending.len() < FLAC_BLOCK * channels && !done {
            if next(ints)? {
                pending.extend_from_slice(ints);
            } else {
                done = true;
            }
        }
        let take = pending.len().min(FLAC_BLOCK * channels);
        if take == 0 {
            break;
        }
        let mut fill = (&mut framebuf, &mut context);
        fill.fill_interleaved(&pending[..take]).map_err(|e| err(&e))?;
        pending.drain(..take);
        let number = context.current_frame_number().unwrap_or(0);
        let frame = flacenc::encode_fixed_size_frame(&config, &framebuf, number, stream.stream_info()).map_err(|e| err(&e))?;
        stream.stream_info_mut().update_frame_info(&frame);
        sink.clear();
        frame.write(&mut sink).map_err(|e| err(&e))?;
        out.write_all(sink.as_slice()).map_err(|e| e.to_string())?;
        total += (take / channels) as u64;
    }
    stream.stream_info_mut().set_block_sizes(FLAC_BLOCK, FLAC_BLOCK).map_err(|e| err(&e))?;
    stream.stream_info_mut().set_md5_digest(&context.md5_digest());
    stream.stream_info_mut().set_total_samples(total as usize);
    let finished = header(&stream)?;
    if finished.len() != placeholder.len() {
        return Err("FLAC header size changed".into());
    }
    out.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    out.write_all(&finished).map_err(|e| e.to_string())?;
    out.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_source(path: &Path, rate: u32, frames: usize) -> Vec<i16> {
        let samples: Vec<i16> = (0..frames * 2).map(|i| ((i as f64 * 0.01).sin() * 12000.0) as i16).collect();
        let mut out = File::create(path).unwrap();
        let data_len = (samples.len() * 2) as u32;
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + data_len).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(rate * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data_len.to_le_bytes());
        out.write_all(&h).unwrap();
        for s in &samples {
            out.write_all(&s.to_le_bytes()).unwrap();
        }
        samples
    }

    fn decode(path: &Path) -> (Vec<i16>, u32, usize) {
        let mut d = plumdeck_decode::open(path, plumdeck_decode::Basis::Canonical).unwrap();
        let (rate, ch) = (d.sample_rate(), d.channels());
        let mut all = Vec::new();
        let mut block = Vec::new();
        while d.read_block(&mut block).unwrap() > 0 {
            all.extend(block.iter().map(|&s| quantize(s, 16) as i16));
        }
        (all, rate, ch)
    }

    #[test]
    fn wav_to_flac_is_lossless() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("a.wav"), dir.path().join("a.flac"));
        let original = write_source(&src, 44_100, 10_000);
        export(&src, &dst, "flac").unwrap();
        let bytes = std::fs::read(&dst).unwrap();
        assert_eq!(&bytes[..4], b"fLaC");
        let (decoded, rate, ch) = decode(&dst);
        assert_eq!((rate, ch), (44_100, 2));
        assert_eq!(decoded, original);
    }

    #[test]
    fn flac_back_to_wav_is_lossless() {
        let dir = tempfile::tempdir().unwrap();
        let (src, mid, dst) = (dir.path().join("a.wav"), dir.path().join("a.flac"), dir.path().join("b.wav"));
        let original = write_source(&src, 48_000, 5_000);
        export(&src, &mid, "flac").unwrap();
        export(&mid, &dst, "wav").unwrap();
        assert_eq!(decode(&dst).0, original);
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 44 + original.len() as u64 * 2);
    }

    #[test]
    fn unreadable_source_fails() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.wav");
        std::fs::write(&src, b"not audio").unwrap();
        assert!(export(&src, &dir.path().join("x.flac"), "flac").is_err());
    }
}
