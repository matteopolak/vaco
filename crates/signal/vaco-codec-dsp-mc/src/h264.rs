//! H.264 motion-compensation kernels and their resolved dispatch table.
//!
//! The decoder owns the scheduling and edge extension; this module owns the
//! arithmetic over already-gathered rows or blocks. Function-pointer entries
//! operate on whole rows or blocks, so dispatch is never paid per pixel and
//! narrow 4x4/2x2 work can be gathered by the decoder before a call.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, KernelSet, Tier, dispatch_kernel, ops};

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
    fn for_tier(tier: Tier) -> Self {
        if tier.is_scalar() {
            return Self {
                luma_half_raw,
                chroma_batch,
                weight_uni,
                weight_bi,
            };
        }
        match tier {
            Tier::Scalar => unreachable!("handled above"),
            Tier::Sse2 => tier_kernels::<1>(),
            Tier::Sse42 => tier_kernels::<2>(),
            Tier::Avx2 => tier_kernels::<3>(),
            Tier::Avx512 => tier_kernels::<4>(),
            Tier::Neon => tier_kernels::<5>(),
            _ => Self {
                luma_half_raw,
                chroma_batch,
                weight_uni,
                weight_bi,
            },
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

fn tier_kernels<const TIER: u8>() -> H264McKernels {
    H264McKernels {
        luma_half_raw: luma_half_raw_tier::<TIER>,
        chroma_batch: chroma_batch_tier::<TIER>,
        weight_uni: weight_uni_tier::<TIER>,
        weight_bi: weight_bi_tier::<TIER>,
    }
}

const fn tier_from_code<const TIER: u8>() -> Tier {
    match TIER {
        1 => Tier::Sse2,
        2 => Tier::Sse42,
        3 => Tier::Avx2,
        4 => Tier::Avx512,
        5 => Tier::Neon,
        _ => Tier::Scalar,
    }
}

fn capped_tier<const TIER: u8>() -> Option<Caps> {
    Caps::detect().capped_at(tier_from_code::<TIER>())
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

fn luma_half_raw_tier<const TIER: u8>(
    src: &[[u8; 21]; 21],
    width: usize,
    height: usize,
    dst: &mut [[i32; 16]; 21],
) {
    let Some(caps) = capped_tier::<TIER>() else {
        return luma_half_raw(src, width, height, dst);
    };
    dispatch_kernel!(caps, s => luma_half_raw_simd(s, src, width, height, dst));
}

#[inline(always)]
#[allow(
    clippy::indexing_slicing,
    reason = "the fixed 21-sample source row covers all six taps for sixteen outputs"
)]
fn luma_half_raw_simd<S: Lanes>(
    simd: S,
    src: &[[u8; 21]; 21],
    width: usize,
    height: usize,
    dst: &mut [[i32; 16]; 21],
) {
    let width = width.min(16);
    let height = height.min(21);
    if width != 16 {
        return luma_half_raw(src, width, height, dst);
    }

    for (source, dest) in src.iter().zip(dst.iter_mut()).take(height) {
        let zero = i16x8::splat(simd, 0);
        let mut lo = zero;
        let mut hi = zero;
        for (tap, coefficient) in [1i16, -5, 20, 20, -5, 1].into_iter().enumerate() {
            let samples = u8x16::from_slice(simd, &source[tap..tap + 16]);
            let (samples_lo, samples_hi) = samples.widen();
            lo = ops::simd::wmla_i16::<S, i16x8<S>>(
                lo,
                samples_lo.bitcast::<i16x8<S>>(),
                coefficient,
            );
            hi = ops::simd::wmla_i16::<S, i16x8<S>>(
                hi,
                samples_hi.bitcast::<i16x8<S>>(),
                coefficient,
            );
        }
        let (a, b) = lo.widen();
        let (c, d) = hi.widen();
        let [d0, d1, d2, d3] = dest.as_chunks_mut::<4>().0 else {
            continue;
        };
        a.store_slice(d0);
        b.store_slice(d1);
        c.store_slice(d2);
        d.store_slice(d3);
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "dy/dx are fixed 0..2 output coordinates and therefore address only the fixed 3x3 input"
)]
#[inline(always)]
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

fn chroma_batch_tier<const TIER: u8>(jobs: &[ChromaJob], out: &mut [[[u8; 2]; 2]]) {
    let Some(caps) = capped_tier::<TIER>() else {
        return chroma_batch(jobs, out);
    };
    dispatch_kernel!(caps, s => chroma_batch_simd(s, jobs, out));
}

#[inline(always)]
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "four fixed 2x2 jobs map exactly to sixteen lanes; division rounds down to complete groups"
)]
fn chroma_batch_simd<S: Lanes>(simd: S, jobs: &[ChromaJob], out: &mut [[[u8; 2]; 2]]) {
    let count = jobs.len().min(out.len());
    let full = (count / 4) * 4;
    let (Some(job_head), Some(out_head)) = (jobs.get(..full), out.get_mut(..full)) else {
        return chroma_batch(jobs, out);
    };

    for (job_group, out_group) in job_head.chunks_exact(4).zip(out_head.chunks_exact_mut(4)) {
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        let mut c = [0u8; 16];
        let mut d = [0u8; 16];
        let mut fx = [0i16; 16];
        let mut fy = [0i16; 16];
        for (job_index, job) in job_group.iter().enumerate() {
            for dy in 0..2usize {
                for dx in 0..2usize {
                    let lane = job_index * 4 + dy * 2 + dx;
                    a[lane] = job.src[dy][dx];
                    b[lane] = job.src[dy][dx + 1];
                    c[lane] = job.src[dy + 1][dx];
                    d[lane] = job.src[dy + 1][dx + 1];
                    fx[lane] = i16::from(job.frac_x.min(7));
                    fy[lane] = i16::from(job.frac_y.min(7));
                }
            }
        }

        let (a0, a1) = u8x16::from_slice(simd, &a).widen();
        let (b0, b1) = u8x16::from_slice(simd, &b).widen();
        let (c0, c1) = u8x16::from_slice(simd, &c).widen();
        let (d0, d1) = u8x16::from_slice(simd, &d).widen();
        let [fx0, fx1] = fx.as_chunks::<8>().0 else {
            continue;
        };
        let [fy0, fy1] = fy.as_chunks::<8>().0 else {
            continue;
        };
        let fx0 = i16x8::from_slice(simd, fx0);
        let fx1 = i16x8::from_slice(simd, fx1);
        let fy0 = i16x8::from_slice(simd, fy0);
        let fy1 = i16x8::from_slice(simd, fy1);
        let eight = i16x8::splat(simd, 8);
        let round = i16x8::splat(simd, 32);
        let interpolate =
            |a: u16x8<S>, b: u16x8<S>, c: u16x8<S>, d: u16x8<S>, wx: i16x8<S>, wy: i16x8<S>| {
                let a = a.bitcast::<i16x8<S>>();
                let b = b.bitcast::<i16x8<S>>();
                let c = c.bitcast::<i16x8<S>>();
                let d = d.bitcast::<i16x8<S>>();
                let top = a * (eight - wx) + b * wx;
                let bottom = c * (eight - wx) + d * wx;
                (top * (eight - wy) + bottom * wy + round) >> 6u32
            };
        let lo = interpolate(a0, b0, c0, d0, fx0, fy0);
        let hi = interpolate(a1, b1, c1, d1, fx1, fy1);
        ops::simd::pack_u8_from_i16x8::<S>(lo, hi)
            .store_slice(out_group.as_flattened_mut().as_flattened_mut());
    }

    chroma_batch(
        jobs.get(full..count).unwrap_or(&[]),
        out.get_mut(full..count).unwrap_or(&mut []),
    );
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

fn weight_uni_tier<const TIER: u8>(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    params: UniWeight,
) {
    let Some(caps) = capped_tier::<TIER>() else {
        return weight_uni(src, src_stride, dst, dst_stride, width, height, params);
    };
    dispatch_kernel!(caps, s => weight_uni_simd(s, src, src_stride, dst, dst_stride, width, height, params));
}

#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    clippy::integer_division,
    reason = "the public kernel contract is strided; division rounds down to complete sixteen-sample groups"
)]
fn weight_uni_simd<S: Lanes>(
    simd: S,
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    params: UniWeight,
) {
    let weight = i16::try_from(params.weight.clamp(-128, 128)).unwrap_or(0);
    let offset = i16::try_from(params.offset.clamp(-128, 127)).unwrap_or(0);
    let log2_denom = params.log2_denom.min(7);
    let round = if log2_denom == 0 {
        0
    } else {
        1i16 << (log2_denom - 1)
    };
    let weight_v = i16x8::splat(simd, weight);
    let offset_v = i16x8::splat(simd, offset);
    let round_v = i16x8::splat(simd, round);

    for y in 0..height {
        let src_row = src
            .get(y.saturating_mul(src_stride)..)
            .and_then(|row| row.get(..width))
            .unwrap_or(&[]);
        let dst_row = dst
            .get_mut(y.saturating_mul(dst_stride)..)
            .and_then(|row| row.get_mut(..width))
            .unwrap_or(&mut []);
        let len = src_row.len().min(dst_row.len());
        let full = (len / 16) * 16;
        let (Some(src_head), Some(dst_head)) = (src_row.get(..full), dst_row.get_mut(..full))
        else {
            continue;
        };
        for (source, dest) in src_head.chunks_exact(16).zip(dst_head.chunks_exact_mut(16)) {
            let (lo, hi) = u8x16::from_slice(simd, source).widen();
            let lo = ((lo.bitcast::<i16x8<S>>() * weight_v + round_v) >> u32::from(log2_denom))
                + offset_v;
            let hi = ((hi.bitcast::<i16x8<S>>() * weight_v + round_v) >> u32::from(log2_denom))
                + offset_v;
            ops::simd::pack_u8_from_i16x8::<S>(lo, hi).store_slice(dest);
        }
        for (&sample, out) in src_row
            .get(full..len)
            .unwrap_or(&[])
            .iter()
            .zip(dst_row.get_mut(full..len).unwrap_or(&mut []).iter_mut())
        {
            let value = i32::from(sample) * i32::from(weight);
            let value = if log2_denom == 0 {
                value
            } else {
                (value + i32::from(round)) >> log2_denom
            };
            *out = clip_u8(value + i32::from(offset));
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

#[allow(clippy::too_many_arguments)]
fn weight_bi_tier<const TIER: u8>(
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
    if !bi_weights_fit_i16(params) {
        return weight_bi(
            src0,
            src0_stride,
            src1,
            src1_stride,
            dst,
            dst_stride,
            width,
            height,
            params,
        );
    }
    let Some(caps) = capped_tier::<TIER>() else {
        return weight_bi(
            src0,
            src0_stride,
            src1,
            src1_stride,
            dst,
            dst_stride,
            width,
            height,
            params,
        );
    };
    dispatch_kernel!(caps, s => weight_bi_simd(s, src0, src0_stride, src1, src1_stride, dst, dst_stride, width, height, params));
}

fn bi_weights_fit_i16(params: BiWeight) -> bool {
    let weight0 = i64::from(params.weight0.clamp(-128, 128)).abs();
    let weight1 = i64::from(params.weight1.clamp(-128, 128)).abs();
    let round = 1i64 << params.log2_denom.min(7);
    255 * (weight0 + weight1) + round <= i64::from(i16::MAX)
}

#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    clippy::integer_division,
    reason = "the public kernel contract is strided; division rounds down to complete sixteen-sample groups"
)]
fn weight_bi_simd<S: Lanes>(
    simd: S,
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
    let weight0 = i16::try_from(params.weight0.clamp(-128, 128)).unwrap_or(0);
    let weight1 = i16::try_from(params.weight1.clamp(-128, 128)).unwrap_or(0);
    let offset = i16::try_from(params.offset.clamp(-128, 127)).unwrap_or(0);
    let log2_denom = params.log2_denom.min(7);
    let round = 1i16 << log2_denom;
    let shift = u32::from(log2_denom) + 1;
    let is_average = weight0 == 1 && weight1 == 1 && offset == 0 && log2_denom == 0;
    let weight0_v = i16x8::splat(simd, weight0);
    let weight1_v = i16x8::splat(simd, weight1);
    let offset_v = i16x8::splat(simd, offset);
    let round_v = i16x8::splat(simd, round);

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
        let len = row0.len().min(row1.len()).min(dst_row.len());
        let full = (len / 16) * 16;
        let (Some(head0), Some(head1), Some(dst_head)) =
            (row0.get(..full), row1.get(..full), dst_row.get_mut(..full))
        else {
            continue;
        };
        for ((a, b), dest) in head0
            .chunks_exact(16)
            .zip(head1.chunks_exact(16))
            .zip(dst_head.chunks_exact_mut(16))
        {
            let av = u8x16::from_slice(simd, a);
            let bv = u8x16::from_slice(simd, b);
            if is_average {
                ops::simd::rounded_avg_u8::<S, u8x16<S>>(av, bv).store_slice(dest);
                continue;
            }
            let (a0, a1) = av.widen();
            let (b0, b1) = bv.widen();
            let lo = ((a0.bitcast::<i16x8<S>>() * weight0_v
                + b0.bitcast::<i16x8<S>>() * weight1_v
                + round_v)
                >> shift)
                + offset_v;
            let hi = ((a1.bitcast::<i16x8<S>>() * weight0_v
                + b1.bitcast::<i16x8<S>>() * weight1_v
                + round_v)
                >> shift)
                + offset_v;
            ops::simd::pack_u8_from_i16x8::<S>(lo, hi).store_slice(dest);
        }
        for ((&sample0, &sample1), out) in row0
            .get(full..len)
            .unwrap_or(&[])
            .iter()
            .zip(row1.get(full..len).unwrap_or(&[]))
            .zip(dst_row.get_mut(full..len).unwrap_or(&mut []))
        {
            let sum = i32::from(sample0) * i32::from(weight0)
                + i32::from(sample1) * i32::from(weight1)
                + i32::from(round);
            *out = clip_u8((sum >> shift) + i32::from(offset));
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
