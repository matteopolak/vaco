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
//! **The pack that is not there.** Every pixel-producing kernel ends by clamping
//! a signed intermediate to `0..=255` and packing it into bytes — `packuswb` on
//! x86, `sqxtun` on NEON, one instruction. The substrate has no such operation:
//! `SimdNarrow` takes `i16` to `i8` and only `u16` to `u8`. So the last step of
//! the kernel goes through [`ops::simd::pack_u8_from_i16x8`], which costs a
//! `max(0)` and a bitcast on top. See the measurement report.
//!
//! **Why this runs at 128-bit block granularity.** The rgb24 store is a 3-way
//! byte interleave: 16 pixels produce 48 bytes, and the shuffle pattern is
//! inherently 16-byte-block-shaped. `swizzle_dyn_within_blocks` and the fixed
//! `u8x16` type are the right tools, and plan 11 §5.6 already says
//! cost-sensitive shuffles should work at block granularity. A production
//! `vaco-scale` should do the *arithmetic* at native width and drop to blocks
//! only for the store; that is a real optimisation this example skips in favour
//! of being readable. On aarch64 it costs nothing, because 128 bits is native.

use crate::{KernelSet, Lanes, Tier, ops};
use fearless_simd::{Bytes, SimdBase, SimdNarrow, SimdWiden, i16x8, i32x4, u8x16};

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
    for (x, (&py, out)) in y.iter().zip(rgb.chunks_exact_mut(3)).enumerate() {
        let c = x >> 1;
        let pu = u.get(c).copied().unwrap_or(128);
        let pv = v.get(c).copied().unwrap_or(128);

        let yy = bt601::Y_SCALE * (i32::from(py) - bt601::Y_OFF);
        let du = i32::from(pu) - bt601::C_OFF;
        let dv = i32::from(pv) - bt601::C_OFF;

        let r = (yy + bt601::R_V * dv + bt601::ROUND) >> bt601::SHIFT;
        let g = (yy - bt601::G_U * du - bt601::G_V * dv + bt601::ROUND) >> bt601::SHIFT;
        let b = (yy + bt601::B_U * du + bt601::ROUND) >> bt601::SHIFT;

        if let [or, og, ob] = out {
            *or = ops::clip_u8(r);
            *og = ops::clip_u8(g);
            *ob = ops::clip_u8(b);
        }
    }
}

/// How many luma pixels one iteration of the vector body consumes.
const BLOCK: usize = 16;
/// Output bytes produced per [`BLOCK`] pixels.
const RGB_BLOCK: usize = BLOCK * 3;

/// One generic body, monomorphised once per CPU level by `dispatch_kernel!`.
///
/// `#[inline(always)]` is MANDATORY and is not a performance suggestion: it is
/// how the target-feature context of the dispatched level reaches this body. A
/// kernel that fails to inline is compiled at the baseline and silently loses
/// its dispatch.
#[inline(always)]
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

    let (y_head, y_tail) = y.split_at(head_px);
    let (rgb_head, rgb_tail) = rgb.split_at_mut(head_px * 3);
    // `head_px` is a multiple of 16, so the chroma split is exact and the tail's
    // `x >> 1` indexing lines up without an offset.
    let (u_head, u_tail) = u.split_at(head_px >> 1);
    let (v_head, v_tail) = v.split_at(head_px >> 1);

    let chroma = u_head
        .chunks_exact(CHROMA_BLOCK)
        .zip(v_head.chunks_exact(CHROMA_BLOCK));
    for ((yc, (uc, vc)), oc) in y_head
        .chunks_exact(BLOCK)
        .zip(chroma)
        .zip(rgb_head.chunks_exact_mut(RGB_BLOCK))
    {
        block16(simd, yc, uc, vc, oc);
    }

    yuv420p_to_rgb24_row_scalar(y_tail, u_tail, v_tail, rgb_tail);
}

/// Chroma samples consumed per [`BLOCK`] luma pixels.
const CHROMA_BLOCK: usize = BLOCK >> 1;

/// 16 luma pixels, 8 chroma pairs, 48 output bytes.
#[inline(always)]
fn block16<S: Lanes>(simd: S, yc: &[u8], uc: &[u8], vc: &[u8], oc: &mut [u8]) {
    // --- load -------------------------------------------------------------
    let yv = u8x16::from_slice(simd, yc);
    // Nearest-neighbour chroma upsample. `zip_low(a, a)` is [a0,a0,a1,a1,…]:
    // one instruction for what a scalar loop spends an index shift on.
    let uh = load_half(simd, uc);
    let vh = load_half(simd, vc);
    let uv = uh.zip_low(uh);
    let vv = vh.zip_low(vh);

    // --- widen u8 -> i32 --------------------------------------------------
    // Four i32x4 groups cover one u8x16. This chain is the substrate's whole
    // widening story: u8 -> u16 -> u32, then a free bitcast to signed.
    let (y0, y1, y2, y3) = widen_u8_i32(yv);
    let (u0, u1, u2, u3) = widen_u8_i32(uv);
    let (v0, v1, v2, v3) = widen_u8_i32(vv);

    // --- the colour matrix ------------------------------------------------
    let (r0, g0, b0) = matrix(y0, u0, v0);
    let (r1, g1, b1) = matrix(y1, u1, v1);
    let (r2, g2, b2) = matrix(y2, u2, v2);
    let (r3, g3, b3) = matrix(y3, u3, v3);

    // --- clip and pack ----------------------------------------------------
    // `relaxed_narrow` is safe here: the post-shift range is about -290..=550,
    // comfortably inside i16. The clip to 0..=255 happens in `pack_u8_from_i16x8`.
    let rp = pack4(r0, r1, r2, r3);
    let gp = pack4(g0, g1, g2, g3);
    let bp = pack4(b0, b1, b2, b3);

    // --- 3-way interleaved store ------------------------------------------
    // 48 output bytes as three 16-byte blocks. For each block, one
    // `swizzle_dyn_precise` per channel — out-of-range indices produce zero, so
    // the three results OR together cleanly.
    for (dst, idx) in oc.chunks_exact_mut(16).zip(INTERLEAVE) {
        let [ir, ig, ib] = idx;
        let out =
            rp.swizzle_dyn_precise(ir) | gp.swizzle_dyn_precise(ig) | bp.swizzle_dyn_precise(ib);
        out.store_slice(dst);
    }
}

/// The BT.601 colour matrix on one group of four pixels, returning `(r, g, b)`
/// as post-shift `i32` lanes still to be clipped.
#[inline(always)]
fn matrix<S: Lanes>(yg: i32x4<S>, ug: i32x4<S>, vg: i32x4<S>) -> (i32x4<S>, i32x4<S>, i32x4<S>) {
    let yy = (yg - bt601::Y_OFF) * bt601::Y_SCALE;
    let du = ug - bt601::C_OFF;
    let dv = vg - bt601::C_OFF;
    (
        (yy + dv * bt601::R_V + bt601::ROUND) >> bt601::SHIFT,
        (yy - du * bt601::G_U - dv * bt601::G_V + bt601::ROUND) >> bt601::SHIFT,
        (yy + du * bt601::B_U + bt601::ROUND) >> bt601::SHIFT,
    )
}

/// Load 8 chroma bytes into the low half of a `u8x16`.
///
/// `from_slice` demands exactly `N` elements, and a chroma chunk is `N / 2`, so
/// this goes through a stack array. The high half is never read. A narrower
/// load (`u8x8`) does not exist in the substrate — 128 bits is its floor.
#[inline(always)]
fn load_half<S: Lanes>(simd: S, half: &[u8]) -> u8x16<S> {
    let mut tmp = [128u8; 16];
    let (lo, _) = tmp.split_at_mut(CHROMA_BLOCK);
    lo.copy_from_slice(half);
    u8x16::from_slice(simd, &tmp)
}

/// `u8x16` → four `i32x4`, in lane order.
#[inline(always)]
#[allow(
    clippy::many_single_char_names,
    reason = "four anonymous quarter-vectors; names would add nothing"
)]
fn widen_u8_i32<S: Lanes>(v: u8x16<S>) -> (i32x4<S>, i32x4<S>, i32x4<S>, i32x4<S>) {
    let (lo, hi) = v.widen();
    let (a, b) = lo.widen();
    let (c, d) = hi.widen();
    (
        a.bitcast::<i32x4<S>>(),
        b.bitcast::<i32x4<S>>(),
        c.bitcast::<i32x4<S>>(),
        d.bitcast::<i32x4<S>>(),
    )
}

/// Four `i32x4` → one `u8x16`, clipped to `0..=255`.
#[inline(always)]
fn pack4<S: Lanes>(a: i32x4<S>, b: i32x4<S>, c: i32x4<S>, d: i32x4<S>) -> u8x16<S> {
    let lo: i16x8<S> = a.relaxed_narrow(b);
    let hi: i16x8<S> = c.relaxed_narrow(d);
    ops::simd::pack_u8_from_i16x8(lo, hi)
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
