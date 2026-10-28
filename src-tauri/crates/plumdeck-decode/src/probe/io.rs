//! Bounds-checked positioned reads over a seekable source.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

use crate::error::{DecodeError, Result};

/// Hard cap for any buffer read into memory by the probe.
pub(crate) const MAX_READ: usize = 1 << 20;

pub(crate) struct Src<'a, R: Read + Seek> {
    inner: &'a mut R,
    pub len: u64,
}

impl<'a, R: Read + Seek> Src<'a, R> {
    pub fn new(inner: &'a mut R) -> Result<Self> {
        let len = inner.seek(SeekFrom::End(0))?;
        Ok(Self { inner, len })
    }

    /// Reads as many bytes as available at `offset` into `buf`, returns the
    /// count (short only at end of file).
    pub fn read_at_most(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        self.inner.seek(SeekFrom::Start(offset))?;
        let mut done = 0;
        while done < buf.len() {
            match self.inner.read(&mut buf[done..]) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(done)
    }

    /// Reads exactly `buf.len()` bytes or fails with `Malformed`.
    pub fn read_exact_at(&mut self, offset: u64, buf: &mut [u8], what: &str) -> Result<()> {
        if self.read_at_most(offset, buf)? != buf.len() {
            return Err(DecodeError::malformed(format!("truncated {what}")));
        }
        Ok(())
    }

    /// Reads up to `len` bytes (capped at `cap` and [`MAX_READ`]) into a new
    /// vector.
    pub fn read_vec(&mut self, offset: u64, len: u64, cap: usize) -> Result<Vec<u8>> {
        let n = len.min(cap.min(MAX_READ) as u64).min(self.len.saturating_sub(offset)) as usize;
        let mut buf = vec![0u8; n];
        let got = self.read_at_most(offset, &mut buf)?;
        buf.truncate(got);
        Ok(buf)
    }
}

pub(crate) fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub(crate) fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn be_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

pub(crate) fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub(crate) fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// MSB-first bit reader over a byte slice.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// Reads `n <= 32` bits.
    pub fn read(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        if self.remaining() < n as usize {
            return None;
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(v)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.remaining() < n {
            return None;
        }
        self.pos += n;
        Some(())
    }
}
