//! Synthetic audio files for the S3 media scenarios (`/api/stream`,
//! `/api/metadata`). Every byte is generated here (no owner data, nothing
//! downloaded): silent MPEG frames, FLAC/MP4/WAV containers with tags, and
//! names that exercise NFC/NFD resolution. The files only need to be
//! readable by tag readers; they do not have to decode to audio.
//!
//! Both sides read the same directory, `<scenario>/media`, which scenarios
//! reach through `{media}` in their SQL, paths and query values.

use std::path::Path;

use anyhow::Result;

/// A 1x1 PNG (cover art).
const PNG: [u8; 67] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
    0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d,
    0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, joint stereo, no CRC: a valid
/// header followed by a zero payload (417 bytes per frame).
fn mpeg_frames(count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..count {
        out.extend_from_slice(&[0xff, 0xfb, 0x90, 0x44]);
        out.extend(std::iter::repeat_n(0u8, 413));
    }
    out
}

fn syncsafe(size: usize) -> [u8; 4] {
    [((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8]
}

/// One ID3v2 frame; v2.3 sizes are plain big-endian, v2.4 syncsafe.
fn id3_frame(id: &str, body: &[u8], version: u8) -> Vec<u8> {
    let mut out = id.as_bytes().to_vec();
    if version == 4 {
        out.extend_from_slice(&syncsafe(body.len()));
    } else {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(body);
    out
}

/// A text frame body: encoding 1 (UTF-16 with BOM) for v2.3, 3 (UTF-8) for v2.4.
fn id3_text(text: &str, version: u8) -> Vec<u8> {
    if version == 4 {
        let mut out = vec![3];
        out.extend_from_slice(text.as_bytes());
        return out;
    }
    let mut out = vec![1, 0xff, 0xfe];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn id3_lyrics(text: &str) -> Vec<u8> {
    // Latin-1, language "eng", empty description.
    let mut out = vec![0, b'e', b'n', b'g', 0];
    out.extend(text.chars().map(|c| c as u8));
    out
}

fn id3_picture(kind: u8) -> Vec<u8> {
    let mut out = vec![0];
    out.extend_from_slice(b"image/png\0");
    out.push(kind);
    out.extend_from_slice(b"\0");
    out.extend_from_slice(&PNG);
    out
}

fn id3_tag(frames: &[Vec<u8>], version: u8) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let mut out = b"ID3".to_vec();
    out.extend_from_slice(&[version, 0, 0]);
    out.extend_from_slice(&syncsafe(body.len()));
    out.extend_from_slice(&body);
    out
}

fn id3v1(title: &str, artist: &str, album: &str) -> Vec<u8> {
    let field = |text: &str, len: usize| {
        let mut bytes = text.as_bytes().to_vec();
        bytes.resize(len, 0);
        bytes
    };
    let mut out = b"TAG".to_vec();
    out.extend(field(title, 30));
    out.extend(field(artist, 30));
    out.extend(field(album, 30));
    out.extend(field("1999", 4));
    out.extend(field("comment", 30));
    out.push(13);
    out
}

fn flac_block(kind: u8, last: bool, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind | if last { 0x80 } else { 0 }];
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

fn flac(comments: &[(&str, &str)], picture: bool) -> Vec<u8> {
    let mut out = b"fLaC".to_vec();
    // STREAMINFO: 4096-sample blocks, 44.1 kHz, 2 channels, 16 bit, 441000 samples.
    let mut info = vec![0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0];
    let packed: u64 = (44_100u64 << 44) | (1u64 << 41) | (15u64 << 36) | 441_000;
    info.extend_from_slice(&packed.to_be_bytes());
    info.extend_from_slice(&[0u8; 16]);
    out.extend(flac_block(0, false, &info));
    let mut vorbis = Vec::new();
    let vendor = b"plumdeck-parity";
    vorbis.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vorbis.extend_from_slice(vendor);
    vorbis.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (key, value) in comments {
        let entry = format!("{key}={value}");
        vorbis.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        vorbis.extend_from_slice(entry.as_bytes());
    }
    out.extend(flac_block(4, !picture, &vorbis));
    if picture {
        let mut body = Vec::new();
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(&9u32.to_be_bytes());
        body.extend_from_slice(b"image/png");
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&[0u8; 16]);
        body.extend_from_slice(&(PNG.len() as u32).to_be_bytes());
        body.extend_from_slice(&PNG);
        out.extend(flac_block(6, true, &body));
    }
    out
}

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn mp4_data(kind: u32, value: &[u8]) -> Vec<u8> {
    let mut body = kind.to_be_bytes().to_vec();
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend_from_slice(value);
    atom(b"data", &body)
}

/// An AAC .m4a with an iTunes `ilst` (title, artist, album, lyrics, cover).
fn m4a() -> Vec<u8> {
    let mut ftyp = b"M4A ".to_vec();
    ftyp.extend_from_slice(&[0, 0, 0, 0]);
    ftyp.extend_from_slice(b"M4A isommp42");
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&10_000u32.to_be_bytes());
    mvhd[20..24].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    mvhd[24] = 0x01;
    let ilst = [
        atom(b"\xa9nam", &mp4_data(1, "M4A Title".as_bytes())),
        atom(b"\xa9ART", &mp4_data(1, "M4A Artist".as_bytes())),
        atom(b"\xa9alb", &mp4_data(1, "M4A Album".as_bytes())),
        atom(b"\xa9lyr", &mp4_data(1, "m4a line one\nm4a line two".as_bytes())),
        atom(b"covr", &mp4_data(14, &PNG)),
    ]
    .concat();
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"mdirappl");
    hdlr.extend_from_slice(&[0u8; 9]);
    let mut meta = vec![0, 0, 0, 0];
    meta.extend(atom(b"hdlr", &hdlr));
    meta.extend(atom(b"ilst", &ilst));
    let udta = atom(b"udta", &atom(b"meta", &meta));
    let moov = atom(b"moov", &[atom(b"mvhd", &mvhd), udta].concat());
    [atom(b"ftyp", &ftyp), moov, atom(b"mdat", &[0u8; 64])].concat()
}

fn wav() -> Vec<u8> {
    let samples = 4410u32;
    let data_len = samples * 4;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend((0..data_len).map(|i| (i % 251) as u8));
    out
}

/// Deterministic noise (xorshift), for a file no reader understands.
fn noise(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9e37_79b9;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// The files, by name. Names in NFD stay NFD on disk (Linux keeps bytes).
pub fn files() -> Vec<(String, Vec<u8>)> {
    let v23 = id3_tag(
        &[
            id3_frame("TIT2", &id3_text("Alpha Song", 3), 3),
            id3_frame("TPE1", &id3_text("DJ Alpha", 3), 3),
            id3_frame("TALB", &id3_text("First Album", 3), 3),
            id3_frame("USLT", &id3_lyrics("line one\nline two"), 3),
            id3_frame("APIC", &id3_picture(3), 3),
        ],
        3,
    );
    let v24 = id3_tag(
        &[
            id3_frame("TIT2", &id3_text("日本語のタイトル", 4), 4),
            id3_frame("TPE1", &id3_text("アーティスト ß", 4), 4),
            id3_frame("APIC", &id3_picture(0), 4),
        ],
        4,
    );
    let v24_empty = id3_tag(&[id3_frame("TIT2", &id3_text("", 4), 4)], 4);
    vec![
        ("id3v23.mp3".into(), [v23, mpeg_frames(40)].concat()),
        ("日本語 ß.mp3".into(), [v24, mpeg_frames(40)].concat()),
        ("cafe\u{301} nfd.mp3".into(), [v24_empty, mpeg_frames(20)].concat()),
        ("id3v1.mp3".into(), [mpeg_frames(30), id3v1("Old Title", "Old Artist", "")].concat()),
        ("untagged.mp3".into(), mpeg_frames(30)),
        (
            "vorbis.flac".into(),
            flac(&[("TITLE", "Flac Title"), ("ARTIST", "Flac Artist"), ("ALBUM", "Flac Album"), ("LYRICS", "flac lyric")], true),
        ),
        ("untagged.flac".into(), flac(&[], false)),
        ("tagged.m4a".into(), m4a()),
        ("plain.wav".into(), wav()),
        ("noise.mp3".into(), noise(3000)),
        ("empty.mp3".into(), Vec::new()),
        ("notes.txt".into(), b"not audio\n".to_vec()),
        ("UPPER.MP3".into(), mpeg_frames(5)),
        ("archive.mp3.gz".into(), noise(64)),
        ("no-extension".into(), noise(32)),
    ]
}

/// Write [`files`] into `dir`, plus a subdirectory `folder.mp3/`.
pub fn write(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir.join("folder.mp3"))?;
    for (name, bytes) in files() {
        std::fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_deterministic() {
        let a = files();
        let b = files();
        assert_eq!(a, b);
        let m4a = &a.iter().find(|(name, _)| name == "tagged.m4a").unwrap().1;
        assert_eq!(&m4a[4..8], b"ftyp");
        let flac = &a.iter().find(|(name, _)| name == "vorbis.flac").unwrap().1;
        assert_eq!(&flac[..4], b"fLaC");
    }
}
