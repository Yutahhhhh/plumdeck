//! The header facts rekordbox timing needs (backend/infra/rekordbox_timing.py):
//! the MP3 encoder case (Xing / Info / VBRI / LAME tag) and whether an M4A
//! holds AAC. Only headers are read; nothing is decoded.

use std::io::Read;
use std::path::Path;

/// The error Python raises (`RekordboxTimingError`) for an unreadable header.
pub const MP3_UNREADABLE: &str = "音源の時刻情報を読めません。音源の場所とMP3形式を確認してください";
pub const M4A_UNREADABLE: &str = "M4A音源のコーデック情報を読めません";

struct Frame {
    offset: usize,
    version1: bool,
    mono: bool,
    layer3: bool,
}

/// The first MPEG frame after an ID3v2 tag, within 1 MiB (mutagen's bound).
fn first_frame(data: &[u8]) -> Option<Frame> {
    let mut start = 0;
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let size = data[6..10].iter().fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7f));
        start = 10 + size + if data[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let end = data.len().min(start + (1 << 20));
    let mut at = start;
    while at + 4 <= end {
        let h = &data[at..at + 4];
        if h[0] == 0xff && h[1] & 0xe0 == 0xe0 {
            let version = (h[1] >> 3) & 3;
            let layer = (h[1] >> 1) & 3;
            let bitrate = (h[2] >> 4) as usize;
            let rate = (h[2] >> 2) & 3;
            if version != 1 && layer != 0 && bitrate != 0 && bitrate != 15 && rate != 3 {
                let version1 = version == 3;
                let layer3 = layer == 1;
                return Some(Frame { offset: at, version1, mono: (h[3] >> 6) == 3, layer3 });
            }
        }
        at += 1;
    }
    None
}

/// `mp3_timing_case`: `'A'`..=`'D'`, or `Err` when the header is unreadable.
pub fn mp3_timing_case(path: &Path) -> Result<char, &'static str> {
    let mut data = Vec::new();
    std::fs::File::open(path).and_then(|file| file.take(4 << 20).read_to_end(&mut data)).map_err(|_| MP3_UNREADABLE)?;
    let frame = first_frame(&data).ok_or(MP3_UNREADABLE)?;
    if !frame.layer3 {
        return Err(MP3_UNREADABLE);
    }
    let bytes = &data[frame.offset..data.len().min(frame.offset + 2048)];
    if bytes.len() < 40 {
        return Err(MP3_UNREADABLE);
    }
    let xing = 4 + match (frame.version1, frame.mono) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    if &bytes[36..40] == b"VBRI" {
        return Ok('D');
    }
    if bytes.len() < xing + 8 || !(bytes[xing..xing + 4] == *b"Xing" || bytes[xing..xing + 4] == *b"Info") {
        return Ok('A');
    }
    let flags = u32::from_be_bytes(bytes[xing + 4..xing + 8].try_into().expect("4 bytes"));
    let mut position = xing + 8 + if flags & 1 != 0 { 4 } else { 0 } + if flags & 2 != 0 { 4 } else { 0 } + if flags & 4 != 0 { 100 } else { 0 };
    if bytes.get(position..position + 4) == Some(b"GOGO") {
        return Ok('B');
    }
    position += if flags & 8 != 0 { 4 } else { 0 };
    let signature = bytes.get(position..position + 9).ok_or(MP3_UNREADABLE)?;
    if signature.to_ascii_lowercase().starts_with(b"lavc") {
        return Ok('B');
    }
    let digits = |s: &[u8]| s.len() >= 5 && s[0] == b'L' && s[1].is_ascii_digit() && s[2] == b'.' && s[3].is_ascii_digit() && s[4].is_ascii_digit();
    let recognized = signature.starts_with(b"LAME") || digits(signature);
    let old_lame = signature.len() >= 9
        && signature.starts_with(b"LAME3.")
        && (b'0'..=b'8').contains(&signature[6])
        && signature[7].is_ascii_digit()
        && !signature[8].is_ascii_digit();
    if !recognized || old_lame {
        return Ok('B');
    }
    let end = position + 36;
    if end > bytes.len() {
        return Err(MP3_UNREADABLE);
    }
    let stored = u16::from_be_bytes([bytes[end - 2], bytes[end - 1]]);
    Ok(if crc16(&bytes[..end - 2]) == stored { 'D' } else { 'C' })
}

fn crc16(data: &[u8]) -> u16 {
    let mut value: u16 = 0;
    for byte in data {
        value ^= u16::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ if value & 1 != 0 { 0xa001 } else { 0 };
        }
    }
    value
}

/// Whether an M4A's first sample entry is AAC (`mp4a`); `Err` when unreadable.
pub fn m4a_is_aac(path: &Path) -> Result<bool, &'static str> {
    let data = std::fs::read(path).map_err(|_| M4A_UNREADABLE)?;
    fn find(data: &[u8], names: &[&[u8; 4]]) -> Option<usize> {
        let mut at = 0;
        while at + 8 <= data.len() {
            let size = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
            let kind = &data[at + 4..at + 8];
            if size < 8 || at + size > data.len() {
                return None;
            }
            if kind == names[0] {
                return if names.len() == 1 { Some(at + 8) } else { find(&data[at + 8..at + size], &names[1..]).map(|i| i + at + 8) };
            }
            at += size;
        }
        None
    }
    let stsd = find(&data, &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"]).ok_or(M4A_UNREADABLE)?;
    let entry = data.get(stsd + 12..stsd + 16).ok_or(M4A_UNREADABLE)?;
    Ok(entry == b"mp4a")
}

/// `timing_offset_ms(filepath)` on this platform (0 except on macOS and Windows).
pub fn timing_offset_ms(filepath: &str) -> Result<f64, &'static str> {
    let macos = cfg!(target_os = "macos");
    if !macos && !cfg!(windows) {
        return Ok(0.0);
    }
    let extension = Path::new(filepath).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if extension == "mp3" {
        let case = mp3_timing_case(Path::new(filepath))?;
        return Ok(match (macos, case) {
            (true, 'A') => 12.0,
            (true, 'B') => 13.0,
            (true, 'C') => 26.0,
            (true, _) => 50.0,
            (false, 'A') | (false, 'D') => 26.0,
            _ => 0.0,
        });
    }
    if macos && extension == "m4a" {
        return Ok(if m4a_is_aac(Path::new(filepath))? { 48.0 } else { 0.0 });
    }
    Ok(0.0)
}
