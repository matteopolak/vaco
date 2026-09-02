//! Fuzzing `vaco-codec-dsp-dwt`'s two 2D wavelet transforms (VC-2's
//! seven integer filters, and JPEG 2000's irreversible float CDF 9/7)
//! for panics on arbitrary shapes and data, and for the one property
//! that must hold over any well-formed shape:
//! an integer VC-2 filter's forward-then-inverse round trip is **exact**,
//! not merely close, on any input the shape check accepts. `width`,
//! `height` and `levels` are drawn from small bounded ranges rather than
//! raw `usize` because most arbitrary combinations are simply rejected by
//! `check_shape` (not a multiple of `2^levels`) -- narrowing the range
//! keeps the fuzzer actually exercising the transform instead of only
//! its own input-validation path.
//! fuzz-crate: vaco-codec-dsp-dwt
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_dwt::{cdf97, vc2};
use vaco_limits::{Budget, Limits};

const ALL_KINDS: [vc2::WaveletKind; 7] = [
    vc2::WaveletKind::DeslauriersDubuc9_7,
    vc2::WaveletKind::LeGall5_3,
    vc2::WaveletKind::DeslauriersDubuc13_7,
    vc2::WaveletKind::HaarNoShift,
    vc2::WaveletKind::HaarSingleShift,
    vc2::WaveletKind::Fidelity,
    vc2::WaveletKind::Daubechies9_7Integer,
];

#[derive(Arbitrary, Debug)]
struct Input {
    kind_idx: u8,
    /// `1..=4`, via `% 4 + 1` below -- levels beyond that just multiply
    /// runtime for no additional code path on a small fuzz-sized image.
    level_raw: u8,
    /// `1..=8`, via `% 8 + 1` -- the per-level multiplier on `2^levels`
    /// for both axes.
    w_mult_raw: u8,
    h_mult_raw: u8,
    int_seed: Vec<i32>,
    float_seed: Vec<i16>,
}

fuzz_target!(|input: Input| {
    let kind = ALL_KINDS[usize::from(input.kind_idx) % ALL_KINDS.len()];
    let levels = u32::from(input.level_raw % 4) + 1;
    let w_mult = usize::from(input.w_mult_raw % 8) + 1;
    let h_mult = usize::from(input.h_mult_raw % 8) + 1;
    let divisor = 1usize << levels;
    let width = w_mult * divisor;
    let height = h_mult * divisor;
    let n = width * height;

    // -- vc2: integer path, exact round trip.
    if !input.int_seed.is_empty() && n <= 4096 {
        let original: Vec<i32> = (0..n)
            .map(|i| input.int_seed[i % input.int_seed.len()].clamp(-30_000, 30_000))
            .collect();
        let mut a = original.clone();
        let mut budget = Budget::new(Limits::default());
        if vc2::dwt_2d(&mut a, width, height, kind, levels, &mut budget).is_ok() {
            let _ = vc2::idwt_2d(&mut a, width, height, kind, levels, &mut budget);
            assert_eq!(
                a, original,
                "vc2 round trip must be exact: {kind:?} levels={levels} w={width} h={height}"
            );
        }
    }

    // -- cdf97: float path, no exactness claim -- panic-freedom and
    // finiteness only (a genuinely floating-point transform, per this
    // crate's own documented tolerance policy).
    if !input.float_seed.is_empty() && n <= 4096 {
        let original: Vec<f64> = (0..n)
            .map(|i| f64::from(input.float_seed[i % input.float_seed.len()]))
            .collect();
        let mut a = original.clone();
        let mut budget = Budget::new(Limits::default());
        if cdf97::dwt_2d(&mut a, width, height, levels, &mut budget).is_ok() {
            let _ = cdf97::idwt_2d(&mut a, width, height, levels, &mut budget);
            for v in &a {
                assert!(v.is_finite(), "cdf97 round trip produced a non-finite value");
            }
        }
    }
});
