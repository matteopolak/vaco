//! Inverse-transform benchmarks.
//!
//! Plan 12's amendments (PF-0.1–PF-0.3) record three confident performance
//! guesses on this project that measured backwards, and the standing
//! instruction that follows from it: **write the spec's literal shape
//! first, benchmark alternatives side by side in one file, and report
//! ratios rather than verdicts.** This file does exactly that for the one
//! place in this crate where an alternative shape is both meaningful and
//! safe to try:
//!
//! - **H.264** has no alternative shape to offer — the standard's butterfly
//!   *is* the fast shape (a handful of adds/subtracts/shifts), and there is
//!   no "naive" O(n²) form that computes the same thing (§8.5.12.2's `>>1`
//!   truncation is not linear, so a matrix-multiply equivalent does not
//!   exist; see `hevc.rs`'s sibling note in `h264.rs`). It is benchmarked
//!   here only for an absolute throughput number.
//! - **HEVC**'s 1-D transform is already the "obvious" shape (`dct1d`'s
//!   `.iter().zip().sum()`), which is exactly the pattern LLVM auto-vectorises
//!   well and which PF-0.3 found *undefeated* by manual accumulator
//!   splitting at narrow widths. The `_acc8` variants here test that
//!   specific, previously-measured-wrong intuition ("more accumulators is
//!   always faster") again, on this crate's own kernel, rather than assuming
//!   the earlier result transfers.
//!
//! ```text
//! cargo bench -p vaco-codec-dsp-idct
//! ```

use vaco_codec_dsp_idct::{blockdsp, h264, mpeg2, pixblockdsp, simd};
use vaco_simd::Caps;

fn main() {
    divan::main();
}

fn ramp_i32<const N: usize>(scale: i32) -> [i32; N] {
    core::array::from_fn(|i| {
        let i = i32::try_from(i).unwrap_or(0);
        (i * scale) - (scale * 8)
    })
}

#[divan::bench]
fn h264_idct4x4(bencher: divan::Bencher<'_, '_>) {
    let input = ramp_i32::<16>(37);
    bencher.bench(|| h264::idct4x4(divan::black_box(&input)));
}

#[divan::bench]
fn h264_idct8x8(bencher: divan::Bencher<'_, '_>) {
    let input = ramp_i32::<64>(23);
    bencher.bench(|| h264::idct8x8(divan::black_box(&input)));
}

#[divan::bench]
fn h264_luma_dc_hadamard4x4(bencher: divan::Bencher<'_, '_>) {
    let input = ramp_i32::<16>(11);
    bencher.bench(|| h264::luma_dc_hadamard4x4(divan::black_box(&input)));
}

mod hevc_dct1d {
    use super::ramp_i32;
    use vaco_codec_dsp_idct::hevc;

    /// The same dot product as [`hevc::dct1d`], but with the accumulation
    /// split across 8 independent `i64` lanes before the final horizontal
    /// reduction — PF-0.3's "accumulator splitting must exceed the target's
    /// vector width" shape, tried here rather than assumed.
    fn dct1d_acc8<const N: usize>(
        x: &[i32; N],
        stride: usize,
        matrix: &[[i32; 32]; 32],
    ) -> [i32; N] {
        core::array::from_fn(|i| {
            let mut acc = [0i64; 8];
            for (j, &xv) in x.iter().enumerate() {
                let row = matrix.get(j * stride).unwrap_or(&[0; 32]);
                let mv = row.get(i).copied().unwrap_or(0);
                if let Some(a) = acc.get_mut(j % 8) {
                    *a += i64::from(xv) * i64::from(mv);
                }
            }
            let sum: i64 = acc.iter().sum();
            vaco_tx::fixed::clamp_i32(sum)
        })
    }

    // `matrix`/`row_stride` are private to `hevc`; the benchmark only needs
    // *a* representative table and stride, so it borrows the same shape
    // through the public `dct1d` for the baseline and reimplements the
    // reduction (not the table) for the alternative, on the size-32 case
    // where the accumulator has the most terms to split.
    const STRIDE_32: usize = 1;

    #[divan::bench]
    fn size32_naive(bencher: divan::Bencher<'_, '_>) {
        let input = ramp_i32::<32>(53);
        bencher.bench(|| hevc::dct1d(divan::black_box(&input)));
    }

    #[divan::bench]
    fn size32_acc8(bencher: divan::Bencher<'_, '_>) {
        let input = ramp_i32::<32>(53);
        // Reconstructed locally: `dct1d`'s own table is private, and the
        // point of this benchmark is the reduction strategy, not the table.
        let matrix = build_probe_matrix();
        bencher.bench(|| dct1d_acc8(divan::black_box(&input), STRIDE_32, &matrix));
    }

    /// A matrix with the same shape (near-orthogonal-ish integer entries) as
    /// [`hevc`]'s real table, built independently so this benchmark module
    /// does not need access to `hevc`'s private constant. The *values* do
    /// not matter for a throughput comparison — only the access pattern
    /// does.
    fn build_probe_matrix() -> [[i32; 32]; 32] {
        core::array::from_fn(|r| {
            core::array::from_fn(|c| {
                let r = i32::try_from(r).unwrap_or(0) + 1;
                let c = i32::try_from(c).unwrap_or(0) + 1;
                (r * c) % 91 - 45
            })
        })
    }
}

#[divan::bench]
fn mpeg2_idct8x8_f32(bencher: divan::Bencher<'_, '_>) {
    let input: [f32; 64] = core::array::from_fn(|i| ((i as f32) * 0.37).sin() * 200.0);
    let Ok(mut idct) = mpeg2::idct8x8_f32() else {
        return;
    };
    bencher.bench_local(|| {
        let mut out = [0f32; 64];
        idct.apply(divan::black_box(&input), &mut out);
        out
    });
}

#[divan::bench]
fn pixblockdsp_get_pixels_16x16(bencher: divan::Bencher<'_, '_>) {
    let src = [7u8; 16 * 20];
    let mut dst = [0i16; 256];
    bencher.bench_local(|| pixblockdsp::get_pixels(&mut dst, &src, 20, 16, 16));
}

#[divan::bench]
fn blockdsp_add_pixels_clamped_scalar_16x16(bencher: divan::Bencher<'_, '_>) {
    let residual = [3i16; 256];
    let mut dst = [100u8; 16 * 20];
    bencher.bench_local(|| blockdsp::add_pixels_clamped(&residual, &mut dst, 20, 16, 16));
}

#[divan::bench]
fn blockdsp_add_pixels_clamped_dispatched_16x16(bencher: divan::Bencher<'_, '_>) {
    let residual = [3i16; 256];
    let mut dst = [100u8; 16 * 20];
    let caps = Caps::detect();
    bencher.bench_local(|| simd::add_pixels_clamped(caps, &residual, &mut dst, 20, 16, 16));
}
