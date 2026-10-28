//! Page-based PCM storage (W1-05).
//!
//! Long tracks are kept as fixed-size pages of interleaved `f32` frames
//! instead of one large allocation. The page table is allocated once, sized
//! for the maximum length, so the decoding thread never reallocates it and a
//! reader on another thread can follow the decode while it runs:
//!
//! - each page is published once, complete (the last one may be short), via
//!   a [`OnceLock`], so a published page is immutable;
//! - `frames_available` is stored with `Release` after the page is
//!   published and loaded with `Acquire` by readers, so every frame below it
//!   is visible.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Default frames per page.
pub const DEFAULT_PAGE_FRAMES: usize = 65_536;

/// Upper bound on page-table slots, whatever the requested length
/// (65,536-frame pages: about 6 hours at 192 kHz).
const MAX_PAGES: usize = 1 << 16;

/// Shared, append-only PCM pages.
#[derive(Debug)]
pub struct PcmPages {
    channels: usize,
    sample_rate: u32,
    page_frames: usize,
    pages: Box<[OnceLock<Box<[f32]>>]>,
    frames_available: AtomicU64,
    finished: AtomicBool,
    truncated: AtomicBool,
}

impl PcmPages {
    /// Creates an empty page set and its writer. `max_frames` is clamped so
    /// the table never exceeds a fixed number of slots.
    pub fn new(channels: usize, sample_rate: u32, page_frames: usize, max_frames: u64) -> (Arc<Self>, PageWriter) {
        let channels = channels.max(1);
        let page_frames = page_frames.clamp(256, 1 << 22);
        let slots = max_frames.div_ceil(page_frames as u64).clamp(1, MAX_PAGES as u64) as usize;
        let pages: Box<[OnceLock<Box<[f32]>>]> = (0..slots).map(|_| OnceLock::new()).collect();
        let shared = Arc::new(Self {
            channels,
            sample_rate,
            page_frames,
            pages,
            frames_available: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            truncated: AtomicBool::new(false),
        });
        let limit = max_frames.min(shared.capacity_frames());
        let writer = PageWriter { shared: Arc::clone(&shared), current: Vec::with_capacity(page_frames * channels), next_page: 0, pushed: 0, limit };
        (shared, writer)
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn page_frames(&self) -> usize {
        self.page_frames
    }

    /// Largest number of frames the table can hold.
    pub fn capacity_frames(&self) -> u64 {
        (self.pages.len() * self.page_frames) as u64
    }

    /// Frames that are published and readable.
    pub fn frames_available(&self) -> u64 {
        self.frames_available.load(Ordering::Acquire)
    }

    /// True once the writer finished (normally or by truncation).
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// True when input was dropped because the table was full.
    pub fn is_truncated(&self) -> bool {
        self.truncated.load(Ordering::Acquire)
    }

    /// A published page (interleaved), if any.
    pub fn page(&self, index: usize) -> Option<&[f32]> {
        self.pages.get(index)?.get().map(|p| &p[..])
    }

    /// Copies interleaved frames starting at `start` into `out`; returns the
    /// number of frames copied (fewer at the end of the available data).
    pub fn copy_interleaved(&self, start: u64, out: &mut [f32]) -> usize {
        let ch = self.channels;
        let want = out.len() / ch;
        let avail = self.frames_available().saturating_sub(start);
        let n = (want as u64).min(avail) as usize;
        let mut done = 0usize;
        while done < n {
            let frame = start + done as u64;
            let page_idx = (frame / self.page_frames as u64) as usize;
            let in_page = (frame % self.page_frames as u64) as usize;
            let Some(page) = self.page(page_idx) else { break };
            let page_len = page.len() / ch;
            let take = (n - done).min(page_len.saturating_sub(in_page));
            if take == 0 {
                break;
            }
            out[done * ch..(done + take) * ch].copy_from_slice(&page[in_page * ch..(in_page + take) * ch]);
            done += take;
        }
        done
    }

    /// All available frames as one interleaved vector.
    pub fn to_interleaved(&self) -> Vec<f32> {
        let n = self.frames_available() as usize;
        let mut out = vec![0.0; n * self.channels];
        let got = self.copy_interleaved(0, &mut out);
        out.truncate(got * self.channels);
        out
    }

    /// All available frames, one vector per channel.
    pub fn to_planar(&self) -> Vec<Vec<f32>> {
        let ch = self.channels;
        let n = self.frames_available() as usize;
        let mut planes = vec![Vec::with_capacity(n); ch];
        let mut i = 0;
        while let Some(page) = self.page(i) {
            for frame in page.chunks_exact(ch) {
                for (plane, &s) in planes.iter_mut().zip(frame) {
                    plane.push(s);
                }
            }
            i += 1;
        }
        for plane in &mut planes {
            plane.truncate(n);
        }
        planes
    }
}

/// The single writer of a [`PcmPages`].
#[derive(Debug)]
pub struct PageWriter {
    shared: Arc<PcmPages>,
    current: Vec<f32>,
    next_page: usize,
    pushed: u64,
    limit: u64,
}

impl PageWriter {
    pub fn pages(&self) -> &Arc<PcmPages> {
        &self.shared
    }

    /// Appends interleaved frames. Returns `false` once the frame limit is
    /// reached (the rest is dropped and the set is marked truncated).
    pub fn push_interleaved(&mut self, mut samples: &[f32]) -> bool {
        let ch = self.shared.channels;
        let page_len = self.shared.page_frames * ch;
        samples = &samples[..samples.len() - samples.len() % ch];
        let room = self.limit - self.pushed;
        let frames = (samples.len() / ch) as u64;
        let accepted = frames.min(room);
        let truncated = accepted < frames;
        samples = &samples[..accepted as usize * ch];
        self.pushed += accepted;
        while !samples.is_empty() {
            let take = (page_len - self.current.len()).min(samples.len());
            self.current.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.current.len() == page_len {
                self.publish();
            }
        }
        if truncated {
            self.shared.truncated.store(true, Ordering::Release);
        }
        !truncated
    }

    fn publish(&mut self) {
        let ch = self.shared.channels;
        let frames = (self.current.len() / ch) as u64;
        if frames == 0 || self.next_page >= self.shared.pages.len() {
            return;
        }
        let page: Box<[f32]> = std::mem::replace(&mut self.current, Vec::with_capacity(self.shared.page_frames * ch)).into_boxed_slice();
        // The writer is unique, so the slot is always empty here.
        let _ = self.shared.pages[self.next_page].set(page);
        self.next_page += 1;
        self.shared.frames_available.fetch_add(frames, Ordering::Release);
    }

    /// Publishes the last partial page and marks the set finished.
    pub fn finish(mut self) -> Arc<PcmPages> {
        self.publish();
        self.shared.finished.store(true, Ordering::Release);
        Arc::clone(&self.shared)
    }
}

impl Drop for PageWriter {
    fn drop(&mut self) {
        // A writer dropped without `finish` (error or panic upstream) still
        // releases readers waiting for completion.
        self.shared.finished.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_round_trip_and_truncate() {
        let (pages, mut w) = PcmPages::new(2, 48_000, 256, 1000);
        assert_eq!(pages.capacity_frames(), 1024);
        let data: Vec<f32> = (0..2 * 700).map(|i| i as f32).collect();
        assert!(w.push_interleaved(&data[..2 * 300]));
        assert_eq!(pages.frames_available(), 256);
        assert!(w.push_interleaved(&data[2 * 300..]));
        let pages = w.finish();
        assert_eq!(pages.frames_available(), 700);
        assert_eq!(pages.to_interleaved(), data);
        let planar = pages.to_planar();
        assert_eq!(planar[1][699], data[2 * 699 + 1]);
        let mut out = vec![0.0; 2 * 10];
        assert_eq!(pages.copy_interleaved(250, &mut out), 10);
        assert_eq!(out[0], data[500]);
        assert_eq!(pages.copy_interleaved(695, &mut out), 5);

        let (pages, mut w) = PcmPages::new(1, 48_000, 256, 300);
        assert!(w.push_interleaved(&[0.0; 300]));
        assert!(!pages.is_truncated());
        assert!(!w.push_interleaved(&[0.0; 1]));
        let pages2 = w.finish();
        assert!(pages2.is_truncated());
        assert_eq!(pages.frames_available(), 300);
    }
}
