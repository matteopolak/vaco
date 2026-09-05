//! H.264 motion-compensation kernels and their resolved dispatch table.
//!
//! The decoder owns the scheduling and edge extension; this module owns the
//! arithmetic over already-gathered rows or blocks. Function-pointer entries
//! operate on whole rows or blocks, so dispatch is never paid per pixel and
//! narrow 4x4/2x2 work can be gathered by the decoder before a call.

use vaco_simd::{KernelSet, Tier};

use crate::fir::{self, taps};

/// Raw horizontal H.264 six-tap pass over one gathered source row.
pub type LumaHalfRawFn = fn(&[u8], &mut [i32]);

/// Bilinear interpolation of one 2x2 chroma output from its shared 3x3 input.
pub type Chroma2x2Fn = fn(&[[u8; 3]; 3], u8, u8) -> [[u8; 2]; 2];

/// Apply one reference's prediction weight to a whole contiguous sample row.
pub type WeightUniFn = fn(&[u8], &mut [u8], UniWeight);

/// Combine two references with explicit, implicit, or default bi-prediction.
pub type WeightBiFn = fn(&[u8], &[u8], &mut [u8], BiWeight);

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
    pub chroma_2x2: Chroma2x2Fn,
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
            chroma_2x2,
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

fn luma_half_raw(src: &[u8], dst: &mut [i32]) {
    fir::fir_pass_i32_into(src, &taps::H264_LUMA_HALFPEL.coeffs, dst);
}

#[allow(
    clippy::indexing_slicing,
    reason = "dy/dx are fixed 0..2 output coordinates and therefore address only the fixed 3x3 input"
)]
fn chroma_2x2(src: &[[u8; 3]; 3], frac_x: u8, frac_y: u8) -> [[u8; 2]; 2] {
    let fx = i32::from(frac_x.min(7));
    let fy = i32::from(frac_y.min(7));
    let weights = [(8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy];
    core::array::from_fn(|dy| {
        core::array::from_fn(|dx| {
            let sum = weights[0] * i32::from(src[dy][dx])
                + weights[1] * i32::from(src[dy][dx + 1])
                + weights[2] * i32::from(src[dy + 1][dx])
                + weights[3] * i32::from(src[dy + 1][dx + 1]);
            clip_u8((sum + 32) >> 6)
        })
    })
}

fn weight_uni(src: &[u8], dst: &mut [u8], params: UniWeight) {
    for (&sample, out) in src.iter().zip(dst.iter_mut()) {
        let value = i32::from(sample).saturating_mul(params.weight);
        let value = if params.log2_denom == 0 {
            value
        } else {
            value.saturating_add(1i32 << (params.log2_denom - 1)) >> params.log2_denom
        };
        *out = clip_u8(value.saturating_add(params.offset));
    }
}

fn weight_bi(src0: &[u8], src1: &[u8], dst: &mut [u8], params: BiWeight) {
    let round = 1i32 << params.log2_denom;
    let shift = u32::from(params.log2_denom) + 1;
    for ((&sample0, &sample1), out) in src0.iter().zip(src1).zip(dst.iter_mut()) {
        let sum = i32::from(sample0)
            .saturating_mul(params.weight0)
            .saturating_add(i32::from(sample1).saturating_mul(params.weight1))
            .saturating_add(round);
        *out = clip_u8((sum >> shift).saturating_add(params.offset));
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
mod tests {
    use super::*;

    #[test]
    fn kernel_set_names_every_entry() {
        assert_eq!(H264McKernels::kernel_names().len(), 4);
    }

    #[test]
    fn luma_raw_matches_the_generic_fir_reference() {
        let src: Vec<u8> = (0..21).map(|i| ((i * 53) & 255) as u8).collect();
        let mut got = [0i32; 16];
        (H264McKernels::select().luma_half_raw)(&src, &mut got);
        let want = fir::fir_pass_i32(&src, &taps::H264_LUMA_HALFPEL.coeffs, 16);
        assert_eq!(got.as_slice(), want.as_slice());
    }

    #[test]
    fn chroma_block_matches_four_direct_bilinear_samples() {
        let src = [[3u8, 29, 101], [47, 89, 137], [173, 211, 251]];
        let kernels = H264McKernels::select();
        for fy in 0..8u8 {
            for fx in 0..8u8 {
                let got = (kernels.chroma_2x2)(&src, fx, fy);
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
        (kernels.weight_uni)(&a, &mut out, UniWeight::IDENTITY);
        assert_eq!(out, a);

        (kernels.weight_bi)(&a, &b, &mut out, BiWeight::AVERAGE);
        let average = core::array::from_fn::<_, 6, _>(|i| {
            ((u16::from(a[i]) + u16::from(b[i]) + 1) >> 1) as u8
        });
        assert_eq!(out, average);

        (kernels.weight_uni)(
            &a,
            &mut out,
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
            &b,
            &mut out,
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
