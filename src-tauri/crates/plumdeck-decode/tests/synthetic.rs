//! Fixtures synthesised in the test (no external tools).

mod common;

use common::*;
use plumdeck_decode::{decode_to_pages, open, probe_path, Basis, Codec, Container, DecodeOptions, TimingCase};

fn opts(basis: Basis) -> DecodeOptions {
    DecodeOptions { basis, ..DecodeOptions::default() }
}

#[test]
fn wav16_is_exact_on_both_bases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.wav");
    write_source_wav(&path);
    let info = probe_path(&path).unwrap();
    assert_eq!(info.container, Container::Wav);
    assert_eq!(info.codec, Codec::Pcm);
    assert_eq!((info.sample_rate, info.channels, info.bits_per_sample), (Some(RATE), Some(2), Some(16)));
    assert_eq!(info.frames_hint, Some(LEN as u64));
    assert!(info.duration_exact);
    assert_eq!(info.timing_case, TimingCase::Lossless);
    assert_eq!(info.priming, 0);

    let expected = to_f32_interleaved(&source());
    for basis in [Basis::DecoderOutput, Basis::Canonical] {
        let d = decode_to_pages(&path, &opts(basis)).unwrap();
        assert_eq!(d.frames(), LEN as u64);
        assert_eq!(d.pages.to_interleaved(), expected, "{basis:?}");
        let mono = mono_of(&d.pages.to_interleaved(), 2);
        assert_eq!(peak_index(&mono, 0..CLICK + NOISE_GAP), CLICK);
    }
}

#[test]
fn wav24_and_float_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let src = to_f32_mono(&source());
    for (bits, format, tol) in [(24u16, 1u16, 1.0 / 8_388_608.0), (32, 3, 0.0)] {
        let path = dir.path().join(format!("m{bits}.wav"));
        write_wav(&path, RATE, 1, bits, format, &src);
        let info = probe_path(&path).unwrap();
        assert_eq!(info.codec, if format == 3 { Codec::PcmFloat } else { Codec::Pcm });
        assert_eq!(info.frames_hint, Some(LEN as u64));
        let d = decode_to_pages(&path, &opts(Basis::Canonical)).unwrap();
        let out = d.pages.to_interleaved();
        assert_eq!(out.len(), LEN);
        let (max_err, _) = compare(&out, &src);
        assert!(max_err <= tol, "{bits}-bit max err {max_err}");
    }
}

fn write_aiff(path: &std::path::Path, src: &[[i16; 2]]) {
    let frames = src.len() as u32;
    let mut ssnd = vec![0u8; 8];
    for f in src {
        ssnd.extend_from_slice(&f[0].to_be_bytes());
        ssnd.extend_from_slice(&f[1].to_be_bytes());
    }
    let mut comm = Vec::new();
    comm.extend_from_slice(&2i16.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&16i16.to_be_bytes());
    // 44100 as 80-bit extended: exponent 16383 + 15, mantissa 44100 << 48.
    comm.extend_from_slice(&(16383u16 + 15).to_be_bytes());
    comm.extend_from_slice(&(44_100u64 << 48).to_be_bytes());
    let mut body = b"AIFF".to_vec();
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&(ssnd.len() as u32).to_be_bytes());
    body.extend_from_slice(&ssnd);
    let mut out = b"FORM".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    std::fs::write(path, out).unwrap();
}

#[test]
fn aiff16_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.aiff");
    let src = source();
    write_aiff(&path, &src);
    let info = probe_path(&path).unwrap();
    assert_eq!(info.container, Container::Aiff);
    assert_eq!(info.sample_rate, Some(RATE));
    assert_eq!(info.frames_hint, Some(LEN as u64));
    let d = decode_to_pages(&path, &opts(Basis::Canonical)).unwrap();
    assert_eq!(d.pages.to_interleaved(), to_f32_interleaved(&src));
}

#[test]
fn resampled_mono_keeps_click_position() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.wav");
    write_source_wav(&path);
    let d = decode_to_pages(&path, &DecodeOptions { target_rate: Some(22_050), mono: true, ..DecodeOptions::default() }).unwrap();
    assert_eq!(d.sample_rate(), 22_050);
    assert_eq!(d.channels(), 1);
    assert_eq!(d.frames(), (LEN as u64).div_ceil(2));
    let out = d.pages.to_interleaved();
    assert_eq!(peak_index(&out, 0..(CLICK + NOISE_GAP) / 2), CLICK / 2);

    let m = plumdeck_decode::decode_mono(&path, Basis::Canonical, Some(48_000), 1800.0).unwrap();
    assert_eq!(m.samples.len() as u64, (LEN as u64 * 48_000).div_ceil(44_100));
    let expect = (CLICK as f64 * 48_000.0 / 44_100.0).round() as usize;
    let got = peak_index(&m.samples, 0..expect + 100);
    assert!(got.abs_diff(expect) <= 1, "click at {got}, expected {expect}");
}

#[test]
fn small_pages_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.wav");
    write_source_wav(&path);
    let d = decode_to_pages(&path, &DecodeOptions { page_frames: 1000, ..DecodeOptions::default() }).unwrap();
    assert!(d.pages.page(0).is_some_and(|p| p.len() == 2000));
    assert_eq!(d.pages.to_interleaved(), to_f32_interleaved(&source()));
    assert!(!d.truncated());

    for rate in [0u32, 999, 1_000_000] {
        let o = DecodeOptions { target_rate: Some(rate), ..DecodeOptions::default() };
        assert_eq!(decode_to_pages(&path, &o).is_err(), rate != 0, "target rate {rate}");
    }

    let d = decode_to_pages(&path, &DecodeOptions { max_seconds: 1.0, ..DecodeOptions::default() }).unwrap();
    assert_eq!(d.frames(), u64::from(RATE));
    assert!(d.truncated());
}

#[test]
fn streaming_reader_matches_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.wav");
    write_source_wav(&path);
    let mut dec = open(&path, Basis::Canonical).unwrap();
    let mut block = Vec::new();
    let mut total = 0usize;
    loop {
        let n = dec.read_block(&mut block).unwrap();
        if n == 0 {
            break;
        }
        assert_eq!(block.len(), n * 2);
        total += n;
    }
    assert_eq!(total, LEN);
    assert_eq!(dec.position(), LEN as u64);
}

#[test]
fn truncated_and_garbage_inputs_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src.wav");
    write_source_wav(&path);
    let bytes = std::fs::read(&path).unwrap();

    // Cut mid-data: the probe clamps the data size to the file.
    let cut = dir.path().join("cut.wav");
    std::fs::write(&cut, &bytes[..bytes.len() / 2 + 1]).unwrap();
    let info = probe_path(&cut).unwrap();
    let d = decode_to_pages(&cut, &DecodeOptions::default()).unwrap();
    assert_eq!(Some(d.frames()), info.frames_hint);

    for (name, data) in [("empty.mp3", Vec::new()), ("zeros.mp3", vec![0u8; 5000]), ("ff.mp3", vec![0xFFu8; 5000]), ("head.wav", bytes[..30].to_vec())] {
        let p = dir.path().join(name);
        std::fs::write(&p, &data).unwrap();
        assert!(probe_path(&p).is_err(), "{name} should not probe");
        assert!(decode_to_pages(&p, &DecodeOptions::default()).is_err(), "{name} should not decode");
    }
}
