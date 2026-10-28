//! Bounds-checked big-endian reads. Every accessor returns `None` instead of
//! panicking when the requested bytes are not there.

pub(crate) fn u8_at(data: &[u8], at: usize) -> Option<u8> {
    data.get(at).copied()
}

pub(crate) fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = slice(data, at, 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = slice(data, at, 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn fourcc_at(data: &[u8], at: usize) -> Option<[u8; 4]> {
    let bytes = slice(data, at, 4)?;
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// `data[at..at + len]`, or `None` when any of it is missing or overflows.
pub(crate) fn slice(data: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    data.get(at..at.checked_add(len)?)
}

/// `count * size` bytes at `at`, or `None` (overflow or out of range).
pub(crate) fn array(data: &[u8], at: usize, count: usize, size: usize) -> Option<&[u8]> {
    slice(data, at, count.checked_mul(size)?)
}

/// UTF-16BE text with trailing NULs removed; an odd trailing byte is ignored
/// and unpaired surrogates become U+FFFD.
pub(crate) fn utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
    let end = units.iter().rposition(|&u| u != 0).map_or(0, |i| i + 1);
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_never_panic_at_the_edges() {
        let data = [1u8, 2, 3];
        assert_eq!(u16_at(&data, 1), Some(0x0203));
        assert_eq!(u16_at(&data, 2), None);
        assert_eq!(u32_at(&data, 0), None);
        assert_eq!(u32_at(&data, usize::MAX), None);
        assert_eq!(slice(&data, usize::MAX, 2), None);
        assert_eq!(array(&data, 0, usize::MAX, 2), None);
        assert_eq!(u8_at(&data, 3), None);
    }

    #[test]
    fn utf16_strips_nul_and_tolerates_garbage() {
        assert_eq!(utf16be(&[0, b'h', 0, b'i', 0, 0]), "hi");
        assert_eq!(utf16be(&[0, b'h', 0]), "h");
        assert_eq!(utf16be(&[0xd8, 0x00]), "\u{fffd}");
        assert_eq!(utf16be(&[]), "");
    }
}
