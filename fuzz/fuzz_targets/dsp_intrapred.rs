//! Fuzzing `vaco-codec-dsp-intrapred`'s three primitives for panics on
//! arbitrary geometry and reference data.
//!
//! `size`/`bit_depth`/`angle`/`log2_size` all ultimately trace back to
//! bitstream-signalled block parameters in any real caller, so this
//! exercises the same "attacker picks the shape" surface `dsp_idct`'s own
//! target does for transform sizes: no property beyond panic-freedom is
//! checked here (the crate's own unit/property tests already pin the
//! exact-value properties over a tamer, hand-chosen domain), matching the
//! project's standard reasoning for out-of-conformance shape input.
//! fuzz-crate: vaco-codec-dsp-intrapred
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_intrapred::{angular_project, dc_predict, planar_predict};

#[derive(Arbitrary, Debug)]
struct Input {
    top: Vec<u16>,
    left: Vec<u16>,
    size: u8,
    bit_depth: u8,
    top_right: u16,
    bottom_left: u16,
    log2_size: u8,
    refs: Vec<u16>,
    pos: u32,
    angle: i32,
    out_len: u8,
}

fuzz_target!(|input: Input| {
    let size = usize::from(input.size);
    let _ = dc_predict(&input.top, &input.left, size, u32::from(input.bit_depth));

    let mut dst = vec![0u16; size.saturating_mul(size).min(4096)];
    planar_predict(
        &mut dst,
        &input.top,
        &input.left,
        input.top_right,
        input.bottom_left,
        size,
        u32::from(input.log2_size),
    );

    let mut row = vec![0u16; usize::from(input.out_len)];
    angular_project(&mut row, &input.refs, input.pos as usize, input.angle);
});
