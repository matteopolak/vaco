//! H.264 motion-compensation kernels and their resolved dispatch table.
//!
//! The decoder owns the scheduling and edge extension; this module owns the
//! arithmetic over already-gathered rows or blocks. Function-pointer entries
//! operate on whole rows or blocks, so dispatch is never paid per pixel and
//! narrow 4x4/2x2 work can be gathered by the decoder before a call.

use vaco_simd::{KernelSet, Tier};

#[cfg(test)]
use crate::fir::{self, taps};

/// Raw horizontal H.264 six-tap passes over one gathered partition window.
pub type LumaHalfRawFn = fn(&[[u8; 21]; 21], usize, usize, &mut [[i32; 16]; 21]);

/// One gathered 3x3 chroma window and its eighth-sample position.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChromaJob {
    /// The four outputs' shared, edge-extended source samples.
    pub src: [[u8; 3]; 3],
    /// Horizontal fractional position in `0..=7`.
    pub frac_x: u8,
    /// Vertical fractional position in `0..=7`.
    pub frac_y: u8,
}

/// Bilinear interpolation of a batch of gathered chroma windows.
pub type ChromaBatchFn = fn(&[ChromaJob], &mut [[[u8; 2]; 2]]);

/// Apply one reference's prediction weight to a whole strided sample block.
pub type WeightUniFn = fn(&[u8], usize, &mut [u8], usize, usize, usize, UniWeight);

/// Combine two references with explicit, implicit, or default bi-prediction.
pub type WeightBiFn = fn(&[u8], usize, &[u8], usize, &mut [u8], usize, usize, usize, BiWeight);

/// Single-list H.264 weighted-prediction parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniWeight {
    /// `w0` from clause 8.4.2.3.2.
    pub weight: i32,
    /// The already bit-depth-scaled offset (`o0` at eight-bit depth).
    pub offset: i32,
    /// `logWD`; zero means no rounding shift.
    pub log2_denom: u8,
}

impl UniWeight {
    /// The unweighted identity transform.
    pub const IDENTITY: Self = Self {
        weight: 1,
        offset: 0,
        log2_denom: 0,
    };
}

/// Two-list H.264 weighted-prediction parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiWeight {
    /// List-0 multiplier.
    pub weight0: i32,
    /// List-1 multiplier.
    pub weight1: i32,
    /// The combined offset `(o0 + o1 + 1) >> 1`.
    pub offset: i32,
    /// `logWD`; the final shift is `logWD + 1`.
    pub log2_denom: u8,
}

impl BiWeight {
    /// Clause 8.4.2.3.1's rounded unweighted average.
    pub const AVERAGE: Self = Self {
        weight0: 1,
        weight1: 1,
        offset: 0,
        log2_denom: 0,
    };
}

/// H.264 MC entries resolved once per decoder/picture, not per sample.
#[derive(Debug, Clone, Copy)]
pub struct H264McKernels {
    /// Raw six-tap luma row used by every non-integer qpel partition.
    pub luma_half_raw: LumaHalfRawFn,
    /// Four chroma samples sharing a 3x3 bilinear input window.
    pub chroma_batch: ChromaBatchFn,
    /// Whole-row single-list weighted prediction.
    pub weight_uni: WeightUniFn,
    /// Whole-row two-list weighted prediction.
    pub weight_bi: WeightBiFn,
}

impl KernelSet for H264McKernels {
    fn for_tier(_tier: Tier) -> Self {
        // The row/block shapes are deliberately identical at every tier for
        // now. LLVM specialises their straight-line fixed-size loops for the
        // selected build target, while this stable table is the seam for a
        // future explicit SIMD body without another decoder API change.
        Self {
            luma_half_raw,
            chroma_batch,
            weight_uni,
            weight_bi,
        }
    }

    fn kernel_names() -> &'static [&'static str] {
        &[
            "h264_luma_half_raw",
            "h264_chroma_2x2",
            "h264_weight_uni",
            "h264_weight_bi",
        ]
    }
}

impl Default for H264McKernels {
    fn default() -> Self {
        Self::select()
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "width is clamped to 16, so x+5 fits the fixed 21-sample source row and x fits the 16-output row"
)]
fn luma_half_raw(src: &[[u8; 21]; 21], width: usize, height: usize, dst: &mut [[i32; 16]; 21]) {
    let width = width.min(16);
    let height = height.min(21);
    for (source, dest) in src.iter().zip(dst.iter_mut()).take(height) {
        for x in 0..width {
            dest[x] = i32::from(source[x]) - 5 * i32::from(source[x + 1])
                + 20 * i32::from(source[x + 2])
                + 20 * i32::from(source[x + 3])
                - 5 * i32::from(source[x + 4])
                + i32::from(source[x + 5]);
        }
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "dy/dx are fixed 0..2 output coordinates and therefore address only the fixed 3x3 input"
)]
fn chroma_2x2(job: &ChromaJob) -> [[u8; 2]; 2] {
    let fx = i32::from(job.frac_x.min(7));
    let fy = i32::from(job.frac_y.min(7));
    let weights = [(8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy];
    core::array::from_fn(|dy| {
        core::array::from_fn(|dx| {
            let sum = weights[0] * i32::from(job.src[dy][dx])
                + weights[1] * i32::from(job.src[dy][dx + 1])
                + weights[2] * i32::from(job.src[dy + 1][dx])
                + weights[3] * i32::from(job.src[dy + 1][dx + 1]);
            clip_u8((sum + 32) >> 6)
        })
    })
}

fn chroma_batch(jobs: &[ChromaJob], out: &mut [[[u8; 2]; 2]]) {
    for (job, block) in jobs.iter().zip(out) {
        *block = chroma_2x2(job);
    }
}

fn weight_uni(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    params: UniWeight,
) {
    let weight = params.weight.clamp(-128, 128);
    let offset = params.offset.clamp(-128, 127);
    let log2_denom = params.log2_denom.min(7);
    for y in 0..height {
        let src_row = src
            .get(y.saturating_mul(src_stride)..)
            .and_then(|row| row.get(..width))
            .unwrap_or(&[]);
        let dst_row = dst
            .get_mut(y.saturating_mul(dst_stride)..)
            .and_then(|row| row.get_mut(..width))
            .unwrap_or(&mut []);
        for (&sample, out) in src_row.iter().zip(dst_row) {
            let value = i32::from(sample) * weight;
            let value = if log2_denom == 0 {
                value
            } else {
                (value + (1i32 << (log2_denom - 1))) >> log2_denom
            };
            *out = clip_u8(value + offset);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn weight_bi(
    src0: &[u8],
    src0_stride: usize,
    src1: &[u8],
    src1_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    params: BiWeight,
) {
    let weight0 = params.weight0.clamp(-128, 128);
    let weight1 = params.weight1.clamp(-128, 128);
    let offset = params.offset.clamp(-128, 127);
    let log2_denom = params.log2_denom.min(7);
    let round = 1i32 << log2_denom;
    let shift = u32::from(log2_denom) + 1;
    for y in 0..height {
        let row0 = src0
            .get(y.saturating_mul(src0_stride)..)
            .and_then(|row| row.get(..width))
            .unwrap_or(&[]);
        let row1 = src1
            .get(y.saturating_mul(src1_stride)..)
            .and_then(|row| row.get(..width))
            .unwrap_or(&[]);
        let dst_row = dst
            .get_mut(y.saturating_mul(dst_stride)..)
            .and_then(|row| row.get_mut(..width))
            .unwrap_or(&mut []);
        for ((&sample0, &sample1), out) in row0.iter().zip(row1).zip(dst_row) {
            let sum = i32::from(sample0) * weight0 + i32::from(sample1) * weight1 + round;
            *out = clip_u8((sum >> shift) + offset);
        }
    }
}

const fn clip_u8(value: i32) -> u8 {
    if value < 0 {
        0
    } else if value > 255 {
        255
    } else {
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "value was range-checked immediately above"
        )]
        {
            value as u8
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "tests use fixed-size arrays and statically bounded loops"
)]
mod tests {
    use super::*;

    #[test]
    fn kernel_set_names_every_entry() {
        assert_eq!(H264McKernels::kernel_names().len(), 4);
    }

    #[test]
    fn luma_raw_matches_the_generic_fir_reference() {
        let src: [u8; 21] = core::array::from_fn(|i| ((i * 53) & 255) as u8);
        let mut window = [[0u8; 21]; 21];
        window[0] = src;
        let want = fir::fir_pass_i32(&src, &taps::H264_LUMA_HALFPEL.coeffs, 16);
        let mut outputs = [[0i32; 16]; 21];
        (H264McKernels::select().luma_half_raw)(&window, 16, 1, &mut outputs);
        assert_eq!(outputs[0].as_slice(), want.as_slice());
    }

    #[test]
    fn chroma_block_matches_four_direct_bilinear_samples() {
        let src = [[3u8, 29, 101], [47, 89, 137], [173, 211, 251]];
        let kernels = H264McKernels::select();
        for fy in 0..8u8 {
            for fx in 0..8u8 {
                let jobs = [ChromaJob {
                    src,
                    frac_x: fx,
                    frac_y: fy,
                }];
                let mut outputs = [[[0u8; 2]; 2]; 1];
                (kernels.chroma_batch)(&jobs, &mut outputs);
                let got = outputs[0];
                for dy in 0..2usize {
                    for dx in 0..2usize {
                        let wx = i32::from(fx);
                        let wy = i32::from(fy);
                        let want = ((8 - wx) * (8 - wy) * i32::from(src[dy][dx])
                            + wx * (8 - wy) * i32::from(src[dy][dx + 1])
                            + (8 - wx) * wy * i32::from(src[dy + 1][dx])
                            + wx * wy * i32::from(src[dy + 1][dx + 1])
                            + 32)
                            >> 6;
                        assert_eq!(got[dy][dx], clip_u8(want), "fx={fx} fy={fy}");
                    }
                }
            }
        }
    }

    #[test]
    fn weighted_rows_cover_identity_average_explicit_and_implicit_shapes() {
        let kernels = H264McKernels::select();
        let a = [0u8, 1, 17, 128, 250, 255];
        let b = [255u8, 250, 129, 17, 1, 0];
        let mut out = [0u8; 6];
        (kernels.weight_uni)(&a, 6, &mut out, 6, 6, 1, UniWeight::IDENTITY);
        assert_eq!(out, a);

        (kernels.weight_bi)(&a, 6, &b, 6, &mut out, 6, 6, 1, BiWeight::AVERAGE);
        let average = core::array::from_fn::<_, 6, _>(|i| {
            ((u16::from(a[i]) + u16::from(b[i]) + 1) >> 1) as u8
        });
        assert_eq!(out, average);

        (kernels.weight_uni)(
            &a,
            6,
            &mut out,
            6,
            6,
            1,
            UniWeight {
                weight: 15,
                offset: -3,
                log2_denom: 4,
            },
        );
        let expected = a.map(|v| clip_u8(((i32::from(v) * 15 + 8) >> 4) - 3));
        assert_eq!(out, expected);

        (kernels.weight_bi)(
            &a,
            6,
            &b,
            6,
            &mut out,
            6,
            6,
            1,
            BiWeight {
                weight0: 48,
                weight1: 16,
                offset: 0,
                log2_denom: 5,
            },
        );
        let expected = core::array::from_fn::<_, 6, _>(|i| {
            clip_u8((i32::from(a[i]) * 48 + i32::from(b[i]) * 16 + 32) >> 6)
        });
        assert_eq!(out, expected);
    }
}
