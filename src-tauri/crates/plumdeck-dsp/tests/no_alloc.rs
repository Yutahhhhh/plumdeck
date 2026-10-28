//! Processing calls never allocate (the engine's audio thread uses them).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use plumdeck_dsp::biquad::{Biquad, Coefficients, Shape};
use plumdeck_dsp::resample::{Polyphase, Variable};
use plumdeck_dsp::stft::Stft;
use rustfft::num_complex::Complex32;

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations_during(work: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    work();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn processing_does_not_allocate() {
    let input = vec![0.3f32; 2 * 4096];
    let mut output = vec![0.0f32; 2 * 8192];
    let mut polyphase = Polyphase::new(2, 44_100, 48_000);
    let mut variable = Variable::new(2, 48_000.0 / 44_100.0, 0.01);
    let mut biquad = Biquad::new(Coefficients::design(Shape::Peaking { gain_db: 3.0 }, 1000.0, 1.0, 48_000.0));
    let mut stft = Stft::new(2048, 512);
    let mut spectrum = vec![Complex32::new(0.0, 0.0); stft.bins()];
    let mut frame = vec![0.0f32; 2048];
    let mut samples = vec![0.1f32; 512];
    let count = allocations_during(|| {
        for _ in 0..20 {
            polyphase.process(&input, &mut output);
            variable.set_ratio(1.09);
            variable.process(&input, &mut output);
            biquad.process(&mut samples);
            stft.analyze(&input[..2048], &mut spectrum);
            stft.synthesize(&spectrum, &mut frame);
        }
        polyphase.finish(&mut output);
    });
    assert_eq!(count, 0);
}
