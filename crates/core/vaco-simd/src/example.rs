//! A worked kernel, end to end, for contributors to copy.
//!
//! yuv420p → rgb24 for one output row, BT.601 limited-range, 8-bit fixed point.
//! The real one belongs in `vaco-scale`; this copy exists so the authoring
//! pattern has a home that is compiled, tested and benchmarked next to the
//! substrate it demonstrates.
//!
//! # The five parts, in order
//!
//! 1. [`yuv420p_to_rgb24_row_scalar`] — the reference. Always compiled, never
//!    conditional, definitionally correct. Also the tail handler: the SIMD body
//!    calls it for the leftover pixels so there is no second edge implementation
//!    that could disagree.
//! 2. [`yuv420p_to_rgb24_row_simd`] — one `#[inline(always)]` body generic over
//!    `S: Lanes`, monomorphised once per level by the dispatcher.
//! 3. [`yuv420p_to_rgb24_row_dispatched`] — the wrapper that goes in the table.
//! 4. [`ColorKernels`] — the [`KernelSet`] table. Selection happens
//!    once, in the consumer's constructor; the indirect call is per *row*.
//! 5. A proptest in `tests/example_agrees.rs` proving 2 equals 1, bit for bit.
//!
//! # Two things this example deliberately shows
//!
//! **The signed-to-unsigned pack that is not there.** Every pixel-producing
//! kernel ends by clamping a signed intermediate to `0..=255` and packing it
//! into bytes — `packuswb` on x86, `sqxtun` on NEON. The substrate has no direct
//! signed-to-unsigned narrow. On x86 this example subtracts 128 before the
//! fixed-point shift, uses signed saturation, then flips the output sign bit;
//! other targets use `max(0)`, a safe bitcast and unsigned saturation so NEON
//! retains its native `sqxtun` lowering.
//!
//! **Why this runs at 128-bit block granularity.** The rgb24 store is a 3-way
//! byte interleave: 16 pixels produce 48 bytes, and the shuffle pattern is
//! inherently 16-byte-block-shaped. `swizzle_dyn_within_blocks` and the fixed
//! `u8x16` type are the right tools, and plan 11 §5.6 already says
//! cost-sensitive shuffles should work at block granularity. The matrix combines
//! those lanes into native-width `i32x8` groups on AVX2, then splits only for
//! narrowing and the fixed 16-pixel store. On aarch64, 128 bits is native and
//! the same portable source stays at that width.

use crate::{KernelSet, Lanes, Tier, ops};
use fearless_simd::{
    Bytes, SimdBase, SimdCombine, SimdNarrow, SimdSplit, SimdWiden, i16x8, i32x4, i32x8, u8x16,
    u8x32,
};

/// BT.601 limited-range coefficients, 8-bit fixed point.
///
/// Derived from ITU-R BT.601-7 §2.5.1's luma equation and the standard
/// limited-range scaling (`Y` 16..235, `C` 16..240), rounded to 8 fractional
/// bits. These are spec-dictated, not authorial (D15).
mod bt601 {
    pub(super) const Y_SCALE: i32 = 298; // 255 / 219 · 256
    pub(super) const R_V: i32 = 409;
    pub(super) const G_U: i32 = 100;
    pub(super) const G_V: i32 = 208;
    pub(super) const B_U: i32 = 516;
    pub(super) const ROUND: i32 = 128; // 0.5 at 8 fractional bits
    pub(super) const SHIFT: u32 = 8;
    pub(super) const Y_OFF: i32 = 16;
    pub(super) const C_OFF: i32 = 128;
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    const OUTPUT_BIAS: i32 = 128 << SHIFT;
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    const OUTPUT_BIAS: i32 = 0;
    pub(super) const R_OFFSET: i32 = ROUND - Y_SCALE * Y_OFF - R_V * C_OFF - OUTPUT_BIAS;
    pub(super) const G_OFFSET: i32 =
        ROUND - Y_SCALE * Y_OFF + G_U * C_OFF + G_V * C_OFF - OUTPUT_BIAS;
    pub(super) const B_OFFSET: i32 = ROUND - Y_SCALE * Y_OFF - B_U * C_OFF - OUTPUT_BIAS;
}

/// The signature stored in the kernel table.
///
/// `y` is one luma row of `width` samples; `u` and `v` are the *corresponding*
/// chroma rows, `width.div_ceil(2)` samples each, already selected for this
/// output line. `rgb` is `width * 3` bytes. Chroma is upsampled nearest —
/// sample `c` covers luma `2c` and `2c + 1`.
///
/// Samples beyond the end of a chroma row are treated as neutral (128), so a
/// short `u`/`v` degrades rather than panicking.
pub type Yuv420ToRgb24Row = fn(y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]);

/// Reference. Always compiled, never conditionally. Definitionally correct.
///
/// Also the tail handler for the vector body, per plan 11 §5.4 rule 1.
#[allow(
    clippy::many_single_char_names,
    reason = "y/u/v/r/g/b are the domain's own names; anything longer reads worse"
)]
pub fn yuv420p_to_rgb24_row_scalar(y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
    for (x, (&py, out)) in y
        .iter()
        .zip(rgb.as_chunks_mut::<3>().0.iter_mut())
        .enumerate()
    {
        let c = x >> 1;
        let pu = u.get(c).copied().unwrap_or(128);
        let pv = v.get(c).copied().unwrap_or(128);

        let yy = bt601::Y_SCALE * (i32::from(py) - bt601::Y_OFF);
        let du = i32::from(pu) - bt601::C_OFF;
        let dv = i32::from(pv) - bt601::C_OFF;

        let r = (yy + bt601::R_V * dv + bt601::ROUND) >> bt601::SHIFT;
        let g = (yy - bt601::G_U * du - bt601::G_V * dv + bt601::ROUND) >> bt601::SHIFT;
        let b = (yy + bt601::B_U * du + bt601::ROUND) >> bt601::SHIFT;

        // `out` is now `&mut [u8; 3]`: the destructure is irrefutable, so the
        // old `if let` (a runtime check against a slice-length invariant
        // `as_chunks_mut` already enforces at the type level) is gone.
        let [or, og, ob] = out;
        *or = ops::clip_u8(r);
        *og = ops::clip_u8(g);
        *ob = ops::clip_u8(b);
    }
}

/// How many luma pixels one RGB interleave block consumes.
const BLOCK: usize = 16;
/// Output bytes produced per [`BLOCK`] pixels.
const RGB_BLOCK: usize = BLOCK * 3;
/// Chroma samples consumed per [`BLOCK`] luma pixels.
const CHROMA_BLOCK: usize = BLOCK >> 1;

const CHROMA_LO_DUPLICATE: [u8; 32] = [
    0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 6, 7, 4, 5, 6, 7, 8, 9, 10, 11, 8, 9, 10, 11, 12, 13, 14, 15, 12,
    13, 14, 15,
];
const CHROMA_HI_DUPLICATE: [u8; 32] = [
    16, 17, 18, 19, 16, 17, 18, 19, 20, 21, 22, 23, 20, 21, 22, 23, 24, 25, 26, 27, 24, 25, 26, 27,
    28, 29, 30, 31, 28, 29, 30, 31,
];

/// One generic body, monomorphised once per CPU level by `dispatch_kernel!`.
///
/// `#[inline(always)]` is MANDATORY and is not a performance suggestion: it is
/// how the target-feature context of the dispatched level reaches this body. A
/// kernel that fails to inline is compiled at the baseline and silently loses
/// its dispatch.
#[inline(always)]
#[crate::vaco::must_vectorize]
#[expect(
    clippy::integer_division,
    reason = "every divisor here is a compile-time constant; there is no untrusted denominator"
)]
pub fn yuv420p_to_rgb24_row_simd<S: Lanes>(simd: S, y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
    // The vector body needs a full block of EVERY plane. A short chroma row is
    // therefore not a special case to handle inside the block — it just shortens
    // the head, and the scalar reference picks up from there with its own
    // per-sample neutral substitution. One place decides what a missing chroma
    // sample means, so the two implementations cannot disagree about it.
    let pixels = y.len().min(rgb.len() / 3);
    let usable = pixels.min(u.len() * 2).min(v.len() * 2);
    let head_px = (usable / BLOCK) * BLOCK;

    // The `min` chain above proves every boundary is in range. The checked
    // splits keep a future violation on a non-panicking error path instead of
    // emitting `split_at`'s panic calls into the dispatched kernel.
    let Some((y_head, y_tail)) = y.split_at_checked(head_px) else {
        return;
    };
    let Some((rgb_head, rgb_tail)) = rgb.split_at_mut_checked(head_px * 3) else {
        return;
    };
    // `head_px` is a multiple of 16, so the chroma split is exact and the tail's
    // `x >> 1` indexing lines up without an offset.
    let Some((u_head, u_tail)) = u.split_at_checked(head_px >> 1) else {
        return;
    };
    let Some((v_head, v_tail)) = v.split_at_checked(head_px >> 1) else {
        return;
    };

    let chroma = u_head
        .as_chunks::<CHROMA_BLOCK>()
        .0
        .iter()
        .zip(v_head.as_chunks::<CHROMA_BLOCK>().0.iter());
    for ((yc, (uc, vc)), oc) in y_head
        .as_chunks::<BLOCK>()
        .0
        .iter()
        .zip(chroma)
        .zip(rgb_head.as_chunks_mut::<RGB_BLOCK>().0.iter_mut())
    {
        block16(simd, yc, uc, vc, oc);
    }

    yuv420p_to_rgb24_row_scalar(y_tail, u_tail, v_tail, rgb_tail);
}

/// One 16-pixel block with its matrix arithmetic combined at AVX2 width.
#[inline(always)]
fn block16<S: Lanes>(simd: S, yc: &[u8], uc: &[u8], vc: &[u8], oc: &mut [u8]) {
    // --- load -------------------------------------------------------------
    let yv = u8x16::from_slice(simd, yc);

    // --- widen u8 -> i32 --------------------------------------------------
    // Two i32x8 groups cover one u8x16. Combining the four 128-bit widening
    // results before the matrix is what makes AVX2 execute the arithmetic at
    // its native 256-bit width while preserving the 16-pixel store shape.
    let (y0, y1) = widen_u8_i32x8(yv);
    let (u0, u1) = widen_and_upsample_chroma(simd, uc);
    let (v0, v1) = widen_and_upsample_chroma(simd, vc);

    // --- the colour matrix ------------------------------------------------
    // Finish and pack one channel before starting the next. This keeps the
    // live-vector set small enough that AVX2 constants remain in registers.
    let yy0 = y0 * bt601::Y_SCALE;
    let yy1 = y1 * bt601::Y_SCALE;
    let rp = pack2(red(yy0, v0), red(yy1, v1));
    let gp = pack2(green(yy0, u0, v0), green(yy1, u1, v1));
    let bp = pack2(blue(yy0, u0), blue(yy1, u1));

    store_rgb_block(rp, gp, bp, oc);
}

/// Interleave one fixed 16-pixel planar block.
#[inline(always)]
fn store_rgb_block<S: Lanes>(rp: u8x16<S>, gp: u8x16<S>, bp: u8x16<S>, oc: &mut [u8]) {
    // --- 3-way interleaved store ------------------------------------------
    // 48 output bytes as three 16-byte blocks. For each block, one
    // `swizzle_dyn_precise` per channel — out-of-range indices produce zero, so
    // the three results OR together cleanly.
    for (dst, idx) in oc.as_chunks_mut::<16>().0.iter_mut().zip(INTERLEAVE) {
        let [ir, ig, ib] = idx;
        let out =
            rp.swizzle_dyn_precise(ir) | gp.swizzle_dyn_precise(ig) | bp.swizzle_dyn_precise(ib);
        out.store_slice(dst);
    }
}

/// The red row of the BT.601 matrix on one native i32 group.
#[inline(always)]
fn red<S: Lanes>(yy: i32x8<S>, vg: i32x8<S>) -> i32x8<S> {
    (yy + vg * bt601::R_V + bt601::R_OFFSET) >> bt601::SHIFT
}

/// The green row of the BT.601 matrix on one native i32 group.
#[inline(always)]
fn green<S: Lanes>(yy: i32x8<S>, ug: i32x8<S>, vg: i32x8<S>) -> i32x8<S> {
    (yy - ug * bt601::G_U - vg * bt601::G_V + bt601::G_OFFSET) >> bt601::SHIFT
}

/// The blue row of the BT.601 matrix on one native i32 group.
#[inline(always)]
fn blue<S: Lanes>(yy: i32x8<S>, ug: i32x8<S>) -> i32x8<S> {
    (yy + ug * bt601::B_U + bt601::B_OFFSET) >> bt601::SHIFT
}

/// Load 8 chroma bytes, widen once, then duplicate the i32 lanes.
#[inline(always)]
fn widen_and_upsample_chroma<S: Lanes>(simd: S, half: &[u8]) -> (i32x8<S>, i32x8<S>) {
    let mut tmp = [128u8; BLOCK];
    let (lo, _) = tmp.split_at_mut(CHROMA_BLOCK);
    lo.copy_from_slice(half);
    let source = u8x16::from_slice(simd, &tmp);
    let (source, _) = source.widen();
    let (lo, hi) = source.widen();
    let source = lo.combine(hi).bitcast::<u8x32<S>>();
    let lo_indices = u8x32::from_slice(simd, &CHROMA_LO_DUPLICATE);
    let hi_indices = u8x32::from_slice(simd, &CHROMA_HI_DUPLICATE);
    (
        source.swizzle_dyn_precise(lo_indices).bitcast::<i32x8<S>>(),
        source.swizzle_dyn_precise(hi_indices).bitcast::<i32x8<S>>(),
    )
}

/// One `u8x16` to two AVX2-width `i32x8` vectors, in lane order.
#[inline(always)]
#[allow(
    clippy::many_single_char_names,
    reason = "four anonymous quarter-vectors; names would add nothing"
)]
fn widen_u8_i32x8<S: Lanes>(v: u8x16<S>) -> (i32x8<S>, i32x8<S>) {
    let (lo, hi) = v.widen();
    let (a, b) = lo.widen();
    let (c, d) = hi.widen();
    (
        a.bitcast::<i32x4<S>>().combine(b.bitcast::<i32x4<S>>()),
        c.bitcast::<i32x4<S>>().combine(d.bitcast::<i32x4<S>>()),
    )
}

/// Two sequential `i32x8` groups to one clipped 16-pixel channel.
#[inline(always)]
fn pack2<S: Lanes>(a: i32x8<S>, b: i32x8<S>) -> u8x16<S> {
    let (a0, a1) = a.split();
    let (b0, b1) = b.split();
    pack4_128(a0, a1, b0, b1)
}

/// Four sequential `i32x4` groups to one clipped 16-pixel channel.
#[inline(always)]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn pack4_128<S: Lanes>(a: i32x4<S>, b: i32x4<S>, c: i32x4<S>, d: i32x4<S>) -> u8x16<S> {
    // Matrix outputs are biased by -128 on x86. Signed saturation therefore
    // clamps to -128..=127; flipping the sign bit maps that exactly to 0..=255.
    let lo: i16x8<S> = a.relaxed_narrow(b);
    let hi: i16x8<S> = c.relaxed_narrow(d);
    let packed: fearless_simd::i8x16<S> = lo.saturating_narrow(hi);
    packed.bitcast::<u8x16<S>>() ^ 0x80u8
}

/// Non-x86 fallback preserving the native signed-to-unsigned narrow on NEON.
#[inline(always)]
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn pack4_128<S: Lanes>(a: i32x4<S>, b: i32x4<S>, c: i32x4<S>, d: i32x4<S>) -> u8x16<S> {
    let lo: i16x8<S> = a.relaxed_narrow(b);
    let hi: i16x8<S> = c.relaxed_narrow(d);
    let zero = i16x8::splat(a.witness(), 0);
    let lo = lo.max(zero).bitcast::<fearless_simd::u16x8<S>>();
    let hi = hi.max(zero).bitcast::<fearless_simd::u16x8<S>>();
    lo.saturating_narrow(hi)
}

/// `INTERLEAVE[block][channel]` selects the bytes of one channel that land in
/// one 16-byte output block. `0xFF` means "not from this channel"; with
/// `swizzle_dyn_precise` that reads as zero, so the three channels OR together.
const INTERLEAVE: [[[u8; 16]; 3]; 3] = [
    [
        interleave_indices(0, 0),
        interleave_indices(0, 1),
        interleave_indices(0, 2),
    ],
    [
        interleave_indices(1, 0),
        interleave_indices(1, 1),
        interleave_indices(1, 2),
    ],
    [
        interleave_indices(2, 0),
        interleave_indices(2, 1),
        interleave_indices(2, 2),
    ],
];

#[allow(
    clippy::integer_division,
    clippy::indexing_slicing,
    reason = "const evaluation over a fixed 16-element array; any error is a compile-time panic"
)]
const fn interleave_indices(block: usize, channel: usize) -> [u8; 16] {
    let mut out = [0xFFu8; 16];
    let mut j = 0;
    while j < 16 {
        let k = block * 16 + j;
        if k % 3 == channel {
            out[j] = (k / 3) as u8;
        }
        j += 1;
    }
    out
}

/// The dispatching wrapper. One per kernel; this is what goes in the table.
pub fn yuv420p_to_rgb24_row_dispatched(y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
    let caps = crate::Caps::detect();
    crate::dispatch_kernel!(caps, simd => yuv420p_to_rgb24_row_simd(simd, y, u, v, rgb));
}

/// The kernel table. Selection happens once, in the consumer's constructor.
///
/// ```
/// use vaco_simd::{KernelSet, example::ColorKernels};
///
/// struct RgbWriter { k: ColorKernels }
///
/// impl RgbWriter {
///     fn new() -> Self { Self { k: ColorKernels::select() } }
///     fn row(&self, y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
///         // One indirect call per ROW. Never per pixel.
///         (self.k.yuv420p_to_rgb24_row)(y, u, v, rgb);
///     }
/// }
///
/// let w = RgbWriter::new();
/// let mut rgb = [0u8; 48];
/// w.row(&[128; 16], &[128; 8], &[128; 8], &mut rgb);
/// assert_eq!(rgb[0], 130);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct ColorKernels {
    /// yuv420p → rgb24, one row.
    pub yuv420p_to_rgb24_row: Yuv420ToRgb24Row,
}

impl KernelSet for ColorKernels {
    fn for_tier(tier: Tier) -> Self {
        Self {
            yuv420p_to_rgb24_row: if tier.is_scalar() {
                yuv420p_to_rgb24_row_scalar
            } else {
                yuv420p_to_rgb24_row_dispatched
            },
        }
    }

    fn kernel_names() -> &'static [&'static str] {
        &["yuv420p_to_rgb24_row"]
    }
}
