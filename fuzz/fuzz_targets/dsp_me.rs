//! Fuzzing `vaco-codec-dsp-mecmp`'s comparison functions and
//! `vaco-codec-dsp-me`'s search patterns for panics on arbitrary pixel
//! data, block geometry and search parameters.
//!
//! Every quantity here is attacker-reachable in a real encoder: pixel
//! content ultimately traces back to a decoded (possibly hostile) frame
//! being re-encoded, and block/search geometry traces back to encoder
//! options. `Plane::sub`/`row` are documented to degrade rather than panic
//! on any out-of-bounds or overflowing offset, and every search pattern is
//! documented to skip an out-of-bounds candidate rather than fail — this
//! target is the adversarial check that both of those properties actually
//! hold, across the full space of malformed and mismatched geometry (a
//! stride shorter than a claimed width, a block origin past the plane's
//! own bounds, a search range wide enough to walk off the edge) rather
//! than only the well-formed shapes the crates' own unit tests construct.
//! fuzz-crate: vaco-codec-dsp-me
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_me::{BlockOrigin, Displacement, Metric, SearchConfig, Searcher};
use vaco_codec_dsp_mecmp::{MecmpKernels, Plane};
use vaco_simd::KernelSet;

#[derive(Arbitrary, Debug)]
struct Input {
    cur: Vec<u8>,
    refb: Vec<u8>,
    stride: u16,
    width: u16,
    height: u16,
    block_x: u16,
    block_y: u16,
    block_w: u8,
    block_h: u8,
    start_x: i16,
    start_y: i16,
    range: u16,
    use_satd: bool,
}

fuzz_target!(|input: Input| {
    // Cap buffer sizes so a single input cannot dominate the fuzzer's time
    // budget; the property under test is panic-freedom over malformed
    // *shapes*, not throughput over huge buffers.
    let cur: Vec<u8> = input.cur.into_iter().take(4096).collect();
    let refb: Vec<u8> = input.refb.into_iter().take(4096).collect();
    let stride = usize::from(input.stride);
    let width = usize::from(input.width);
    let height = usize::from(input.height);

    let cur_plane = Plane::new(&cur, stride, width, height);
    let ref_plane = Plane::new(&refb, stride, width, height);

    let kernels = MecmpKernels::reference();
    let _ = (kernels.sad)(cur_plane, ref_plane);
    let _ = (kernels.ssd)(cur_plane, ref_plane);
    let _ = (kernels.variance)(cur_plane, ref_plane);
    let _ = (kernels.satd)(cur_plane, ref_plane);

    let dispatched = MecmpKernels::select();
    let _ = (dispatched.sad)(cur_plane, ref_plane);
    let _ = (dispatched.ssd)(cur_plane, ref_plane);
    let _ = (dispatched.variance)(cur_plane, ref_plane);

    let block = BlockOrigin {
        x: usize::from(input.block_x),
        y: usize::from(input.block_y),
        width: usize::from(input.block_w),
        height: usize::from(input.block_h),
    };
    let cfg = SearchConfig {
        metric: if input.use_satd {
            Metric::Satd
        } else {
            Metric::Sad
        },
        range: u32::from(input.range),
    };
    let start = Displacement {
        x: i32::from(input.start_x),
        y: i32::from(input.start_y),
    };

    let searcher = Searcher::with_kernels(kernels);
    let _ = searcher.diamond_search(cur_plane, ref_plane, block, &cfg, start);
    let _ = searcher.three_step_search(cur_plane, ref_plane, block, &cfg, start);
    // Full search is O(range^2); cap the range actually used for it so one
    // fuzz input cannot itself become a timeout.
    let full_cfg = SearchConfig {
        range: cfg.range.min(24),
        ..cfg
    };
    let _ = searcher.full_search(cur_plane, ref_plane, block, &full_cfg, start);
});
