//! Probe unit tests on hand-built headers, plus a deterministic fuzz pass.

use std::io::Cursor;

use super::mp4::{parse_asc, parse_itunsmpb_text};
use super::mpeg::{crc16_lame, parse_header, MP3_DECODER_DELAY};
use super::riff::extended_to_f64;
use super::*;

// MPEG-1 layer III, 128 kbps, 44.1 kHz, stereo, no CRC: 417-byte frames.
const HDR: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];
const FRAME_LEN: usize = 417;

fn plain_frame() -> Vec<u8> {
    let mut f = vec![0u8; FRAME_LEN];
    f[..4].copy_from_slice(&HDR);
    f
}

fn info_frame(tag: &[u8; 4], frames: u32, encoder: Option<&[u8; 9]>, delay: u16, padding: u16) -> Vec<u8> {
    let mut f = plain_frame();
    let x = 36;
    f[x..x + 4].copy_from_slice(tag);
    f[x + 4..x + 8].copy_from_slice(&0x0Fu32.to_be_bytes());
    f[x + 8..x + 12].copy_from_slice(&frames.to_be_bytes());
    f[x + 12..x + 16].copy_from_slice(&(frames * FRAME_LEN as u32).to_be_bytes());
    if let Some(enc) = encoder {
        let l = x + 120;
        f[l..l + 9].copy_from_slice(enc);
        let trim = (u32::from(delay) << 12) | u32::from(padding);
        f[l + 21] = (trim >> 16) as u8;
        f[l + 22] = (trim >> 8) as u8;
        f[l + 23] = trim as u8;
        let crc = crc16_lame(&f[..l + 34]);
        f[l + 34..l + 36].copy_from_slice(&crc.to_be_bytes());
    }
    f
}

fn mp3_file(first: Vec<u8>, audio_frames: usize, prefix: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(&first);
    for _ in 0..audio_frames {
        out.extend_from_slice(&plain_frame());
    }
    out
}

fn probe_bytes(b: &[u8]) -> crate::Result<ProbeInfo> {
    probe_reader(&mut Cursor::new(b))
}

#[test]
fn frame_header_fields() {
    let h = parse_header(u32::from_be_bytes(HDR)).unwrap();
    assert!(format!("{h:?}").contains("frame_len: 417"));
    // MPEG-2 layer III 64 kbps 22.05 kHz: 72 * 64000 / 22050 = 208.
    let h2 = parse_header(0xFFF3_8000).unwrap();
    assert!(format!("{h2:?}").contains("frame_len: 208") && format!("{h2:?}").contains("spf: 576"));
    assert!(parse_header(0xFFFB_F000).is_none(), "bitrate index 15");
    assert!(parse_header(0xFFFB_0000).is_none(), "free format");
    assert!(parse_header(0xFFFB_9C00).is_none(), "reserved rate");
    assert!(parse_header(0xFFF9_9000).is_none(), "reserved layer");
}

#[test]
fn crc16_known_vector() {
    // CRC-16/ARC of "123456789" is 0xBB3D.
    assert_eq!(crc16_lame(b"123456789"), 0xBB3D);
}

#[test]
fn mp3_cases() {
    let lame = probe_bytes(&mp3_file(info_frame(b"Info", 100, Some(b"LAME3.100"), 576, 1000), 8, &[])).unwrap();
    assert_eq!(lame.timing_case, TimingCase::Mp3LameTag);
    assert_eq!(lame.timing_case.label(), "mp3-D");
    assert_eq!(lame.priming, 576 + MP3_DECODER_DELAY);
    assert_eq!(lame.frames_hint, Some(100 * 1152));
    assert_eq!(lame.valid_frames, Some(100 * 1152 - 1576));
    assert_eq!(lame.padding, Some(1000));
    let d = lame.mp3.as_ref().unwrap();
    assert_eq!((d.info_tag, d.xing_frames, d.audio_offset), (InfoTag::Info, Some(100), 0));

    let lavc = probe_bytes(&mp3_file(info_frame(b"Info", 100, Some(b"Lavc61.19"), 576, 1000), 8, &[])).unwrap();
    assert_eq!(lavc.timing_case, TimingCase::Mp3LavcTag);

    let mut bad = info_frame(b"Xing", 100, Some(b"LAME3.100"), 576, 1000);
    bad[36 + 120 + 35] ^= 0xFF;
    let bad = probe_bytes(&mp3_file(bad, 8, &[])).unwrap();
    assert_eq!(bad.timing_case, TimingCase::Mp3LameTagBadCrc);
    assert_eq!(bad.priming, 576 + MP3_DECODER_DELAY);

    let xing = probe_bytes(&mp3_file(info_frame(b"Xing", 100, None, 0, 0), 8, &[])).unwrap();
    assert_eq!(xing.timing_case, TimingCase::Mp3XingNoLame);
    assert_eq!((xing.priming, xing.valid_frames, xing.frames_hint), (0, None, Some(100 * 1152)));

    let none = probe_bytes(&mp3_file(plain_frame(), 9, &[])).unwrap();
    assert_eq!(none.timing_case, TimingCase::Mp3NoInfo);
    assert_eq!(none.frames_hint, Some(10 * 1152));
    assert!(!none.duration_exact);

    let mut vbri = plain_frame();
    vbri[36..40].copy_from_slice(b"VBRI");
    vbri[42..44].copy_from_slice(&1105u16.to_be_bytes());
    vbri[50..54].copy_from_slice(&77u32.to_be_bytes());
    let vbri = probe_bytes(&mp3_file(vbri, 8, &[])).unwrap();
    assert_eq!(vbri.timing_case, TimingCase::Mp3Vbri);
    assert_eq!(vbri.priming, 0);
    assert_eq!(vbri.mp3.as_ref().unwrap().vbri_delay, Some(1105));
    assert_eq!(vbri.frames_hint, Some(77 * 1152));
}

#[test]
fn mp3_after_id3v2_and_junk() {
    // ID3v2.4 with footer, 300-byte body (synchsafe 0x00 0x00 0x02 0x2C).
    let mut prefix = b"ID3\x04\x00\x10\x00\x00\x02\x2C".to_vec();
    prefix.resize(10 + 300 + 10, 0);
    prefix.extend_from_slice(&[0u8; 37]); // junk
    prefix.extend_from_slice(&[0xFF, 0xE0, 0x12]); // false sync
    let info = probe_bytes(&mp3_file(info_frame(b"Info", 50, Some(b"LAME3.100"), 576, 600), 8, &prefix)).unwrap();
    let d = info.mp3.unwrap();
    assert_eq!(d.id3v2_bytes, 320);
    assert_eq!(d.junk_bytes, 40);
    assert_eq!(d.audio_offset, 360);
    assert_eq!(info.timing_case, TimingCase::Mp3LameTag);
}

#[test]
fn mp3_trailing_tags_are_not_audio() {
    let mut file = mp3_file(plain_frame(), 9, &[]);
    let mut ape = vec![0u8; 100];
    let mut footer = b"APETAGEX".to_vec();
    footer.extend_from_slice(&2000u32.to_le_bytes());
    footer.extend_from_slice(&132u32.to_le_bytes()); // items + footer
    footer.extend_from_slice(&1u32.to_le_bytes());
    footer.extend_from_slice(&0u32.to_le_bytes());
    footer.extend_from_slice(&[0u8; 8]);
    ape.extend_from_slice(&footer);
    file.extend_from_slice(&ape);
    let mut v1 = b"TAG".to_vec();
    v1.resize(128, b' ');
    file.extend_from_slice(&v1);
    let info = probe_bytes(&file).unwrap();
    assert_eq!(info.frames_hint, Some(10 * 1152));
}

struct BitWriter {
    bytes: Vec<u8>,
    n: usize,
}

impl BitWriter {
    fn new() -> Self {
        Self { bytes: Vec::new(), n: 0 }
    }
    fn put(&mut self, v: u32, bits: u32) -> &mut Self {
        for i in (0..bits).rev() {
            if self.n % 8 == 0 {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.n % 8);
            }
            self.n += 1;
        }
        self
    }
}

#[test]
fn audio_specific_config() {
    let lc = parse_asc(&[0x12, 0x10]).unwrap();
    assert_eq!((lc.aot, lc.sample_rate, lc.channel_config, lc.sbr), (2, 44_100, 2, false));

    // Explicit hierarchical: AOT 5, 22.05 kHz core, 44.1 kHz output, core LC.
    let mut w = BitWriter::new();
    w.put(5, 5).put(7, 4).put(2, 4).put(4, 4).put(2, 5).put(0, 3);
    let he = parse_asc(&w.bytes).unwrap();
    assert_eq!((he.aot, he.sbr, he.ps, he.ext_sample_rate), (2, true, false, Some(44_100)));

    let mut w = BitWriter::new();
    w.put(29, 5).put(7, 4).put(1, 4).put(4, 4).put(2, 5).put(0, 3);
    assert!(parse_asc(&w.bytes).unwrap().ps);

    // Backward-compatible explicit: LC + 0x2b7 + AOT 5 + sbr + rate + 0x548 + ps.
    let mut w = BitWriter::new();
    w.put(2, 5).put(7, 4).put(2, 4).put(0, 3).put(0x2b7, 11).put(5, 5).put(1, 1).put(4, 4).put(0x548, 11).put(1, 1);
    let bc = parse_asc(&w.bytes).unwrap();
    assert_eq!((bc.aot, bc.sbr, bc.ps, bc.ext_sample_rate), (2, true, true, Some(44_100)));

    // Escaped rate and object type, and truncated input.
    let mut w = BitWriter::new();
    w.put(31, 5).put(10, 6).put(15, 4).put(44_100, 24).put(1, 4);
    let esc = parse_asc(&w.bytes).unwrap();
    assert_eq!((esc.aot, esc.sample_rate), (42, 44_100));
    assert!(parse_asc(&[0x12]).is_none());
}

#[test]
fn itunsmpb_and_extended() {
    let t = " 00000000 00000840 000001CA 00000000001CC1F6 00000000 00000000 00000000 00000000";
    assert_eq!(parse_itunsmpb_text(t), Some((2112, 458, 0x1C_C1F6)));
    assert_eq!(parse_itunsmpb_text("00000000 zz 0 0"), None);
    assert_eq!(parse_itunsmpb_text(""), None);
    let ext = [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0];
    assert_eq!(extended_to_f64(&ext), 44_100.0);
}

fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(typ);
    out.extend_from_slice(body);
    out
}

fn full(typ: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version, 0, 0, 0];
    b.extend_from_slice(body);
    bx(typ, &b)
}

/// A minimal AAC-LC M4A header (no samples needed for probing).
fn m4a(elst: Option<(u32, i32)>, itunsmpb: Option<&str>, asc: &[u8], entry_rate: u32) -> Vec<u8> {
    let mut esds = vec![3u8, 0x80 | 0, 25, 0, 1, 0];
    esds.extend_from_slice(&[4, 17, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    esds.extend_from_slice(&[5, asc.len() as u8]);
    esds.extend_from_slice(asc);
    let mut entry = vec![0u8; 6];
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0u8; 8]);
    entry.extend_from_slice(&2u16.to_be_bytes());
    entry.extend_from_slice(&16u16.to_be_bytes());
    entry.extend_from_slice(&[0u8; 4]);
    entry.extend_from_slice(&(entry_rate << 16).to_be_bytes());
    entry.extend_from_slice(&full(b"esds", 0, &esds));
    let mut stsd_body = 1u32.to_be_bytes().to_vec();
    stsd_body.extend_from_slice(&bx(b"mp4a", &entry));
    let stbl = bx(b"stbl", &full(b"stsd", 0, &stsd_body));
    let minf = bx(b"minf", &stbl);
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&44_100u32.to_be_bytes());
    mdhd.extend_from_slice(&134_144u32.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 4]);
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(b"soun");
    hdlr.extend_from_slice(&[0u8; 13]);
    let mut mdia = full(b"mdhd", 0, &mdhd);
    mdia.extend_from_slice(&full(b"hdlr", 0, &hdlr));
    mdia.extend_from_slice(&minf);
    let mut trak = Vec::new();
    if let Some((seg, mt)) = elst {
        let mut e = 1u32.to_be_bytes().to_vec();
        e.extend_from_slice(&seg.to_be_bytes());
        e.extend_from_slice(&mt.to_be_bytes());
        e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        trak.extend_from_slice(&bx(b"edts", &full(b"elst", 0, &e)));
    }
    trak.extend_from_slice(&bx(b"mdia", &mdia));
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&3000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let mut moov = full(b"mvhd", 0, &mvhd);
    moov.extend_from_slice(&bx(b"trak", &trak));
    if let Some(text) = itunsmpb {
        let mut item = full(b"mean", 0, b"com.apple.iTunes");
        item.extend_from_slice(&full(b"name", 0, b"iTunSMPB"));
        let mut data = 1u32.to_be_bytes().to_vec();
        data.extend_from_slice(&[0u8; 4]);
        data.extend_from_slice(text.as_bytes());
        item.extend_from_slice(&bx(b"data", &data));
        let ilst = bx(b"ilst", &bx(b"----", &item));
        moov.extend_from_slice(&bx(b"udta", &full(b"meta", 0, &ilst)));
    }
    let mut file = bx(b"ftyp", b"M4A \0\0\0\0M4A mp42");
    file.extend_from_slice(&bx(b"moov", &moov));
    file.extend_from_slice(&bx(b"mdat", &[0u8; 64]));
    file
}

const SMPB: &str = " 00000000 00000840 000001CA 0000000000020000 00000000";

#[test]
fn mp4_priming_precedence() {
    // Edit list with priming wins.
    let a = probe_bytes(&m4a(Some((3000, 1024)), Some(SMPB), &[0x12, 0x10], 44_100)).unwrap();
    assert_eq!((a.codec, a.timing_case, a.priming_source), (Codec::AacLc, TimingCase::Mp4Priming, PrimingSource::EditList));
    assert_eq!(a.priming, 1024);
    assert_eq!(a.valid_frames, Some(132_300));
    assert_eq!(a.frames_hint, Some(134_144));
    assert_eq!(a.padding, Some(134_144 - 1024 - 132_300));
    assert_eq!(a.mp4.as_ref().unwrap().itunsmpb, Some((2112, 458, 0x20000)));

    // Edit list at 0: iTunSMPB is used.
    let b = probe_bytes(&m4a(Some((3000, 0)), Some(SMPB), &[0x12, 0x10], 44_100)).unwrap();
    assert_eq!((b.priming, b.priming_source, b.valid_frames), (2112, PrimingSource::ITunSmpb, Some(0x20000)));

    let c = probe_bytes(&m4a(None, None, &[0x12, 0x10], 44_100)).unwrap();
    assert_eq!((c.priming, c.timing_case), (0, TimingCase::Mp4NoPriming));
}

#[test]
fn mp4_he_aac_detection() {
    // Implicit: LC at 22.05 kHz in a 44.1 kHz sample entry.
    let mut w = BitWriter::new();
    w.put(2, 5).put(7, 4).put(2, 4).put(0, 3);
    let implicit = probe_bytes(&m4a(None, None, &w.bytes, 44_100)).unwrap();
    assert_eq!(implicit.codec, Codec::HeAac { ps: false, implicit: true });
    assert_eq!(implicit.sample_rate, Some(44_100));

    let mut w = BitWriter::new();
    w.put(5, 5).put(7, 4).put(2, 4).put(4, 4).put(2, 5).put(0, 3);
    let explicit = probe_bytes(&m4a(None, None, &w.bytes, 44_100)).unwrap();
    assert_eq!(explicit.codec, Codec::HeAac { ps: false, implicit: false });
}

/// Deterministic fuzzing: mutated and truncated headers must never panic.
#[test]
fn fuzz_never_panics() {
    let mut seeds: Vec<Vec<u8>> = vec![
        mp3_file(info_frame(b"Info", 100, Some(b"LAME3.100"), 576, 1000), 4, &[]),
        mp3_file(plain_frame(), 4, b"ID3\x03\x00\x00\x00\x00\x00\x20"),
        m4a(Some((3000, 1024)), Some(SMPB), &[0x12, 0x10], 44_100),
        b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\x10\xb1\x02\0\x04\0\x10\0data\x08\0\0\0\0\0\0\0\0\0\0\0".to_vec(),
        b"FORM\0\0\0\x2eAIFFCOMM\0\0\0\x12\0\x02\0\0\0\x01\0\x10\x40\x0e\xac\x44\0\0\0\0\0\0SSND\0\0\0\x0c\0\0\0\0\0\0\0\0\0\0\0\0".to_vec(),
        {
            let mut f = b"fLaC\x80\0\0\x22".to_vec();
            f.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0, 0x0A, 0xC4, 0x42, 0xF0, 0, 0, 0x10, 0]);
            f.resize(8 + 34, 0);
            f
        },
        {
            let mut p = b"OggS\0\x02\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\x01\x1e".to_vec();
            p.extend_from_slice(b"\x01vorbis\0\0\0\0\x02\x44\xac\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\xb8\x01");
            p
        },
        b"caff\0\x01\0\0desc\0\0\0\0\0\0\0\x20\x40\xe5\x88\x80\0\0\0\0lpcm\0\0\0\x0c\0\0\0\x04\0\0\0\x01\0\0\0\x02\0\0\0\x10".to_vec(),
    ];
    for magic in [&b"ftyp"[..], b"OggS", b"caff", b"fLaC", b"ID3\x04"] {
        let mut v = vec![0u8; 4];
        v.extend_from_slice(magic);
        seeds.push(v);
    }
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut probed = 0;
    for (i, seed) in seeds.iter().enumerate() {
        if let Err(e) = probe_bytes(seed) {
            assert!(i >= 3, "seed {i} must probe: {e}");
            eprintln!("seed {i} does not probe: {e}");
        }
        for _ in 0..2000 {
            let mut data = seed.clone();
            let edits = 1 + rnd() % 8;
            for _ in 0..edits {
                if data.is_empty() {
                    break;
                }
                let at = (rnd() % data.len() as u64) as usize;
                match rnd() % 4 {
                    0 => data[at] = rnd() as u8,
                    1 => data[at] = 0xFF,
                    2 => data.truncate(at),
                    _ => data.splice(at..at, (0..rnd() % 16).map(|_| rnd() as u8)).for_each(drop),
                }
            }
            let _ = probe_bytes(&data);
            probed += 1;
        }
    }
    assert!(probed >= 2000);
}
