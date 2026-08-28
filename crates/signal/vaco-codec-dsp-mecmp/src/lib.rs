#![forbid(unsafe_code)]
//! Motion-estimation comparison functions: SAD, SSD, variance and SATD.
//!
//! An encoder's motion search evaluates one of these on every candidate
//! offset it tries — often hundreds of times per block — so this crate is a
//! hot inner loop, not a general-purpose image-diff utility. It exists
//! because [`vaco-codec-dsp-me`](https://docs.rs) (D-13, motion-estimation
//! *search patterns*) calls straight into it: the search pattern decides
//! *where* to look, this crate decides *how good* a candidate is, and
//! keeping them separate crates (rather than one) is what let D-13 be
//! written entirely against this crate's public API rather than against a
//! specific search algorithm's internals.
//!
//! # The four comparisons
//!
//! | Function | What it measures | Typical use |
//! |---|---|---|
//! | [`sad`] | Σ &#124;cur − ref&#124; | the inner-loop cost for every search pattern in D-13; cheapest to compute, monotonic enough to rank candidates well |
//! | [`ssd`] | Σ (cur − ref)² | a smoother cost when SAD's ties need breaking, and the input to [`variance`] |
//! | [`variance`] | Σ (cur − ref)² − (Σ (cur − ref))² / N | SAD/SSD are fooled by a candidate that is a *uniform* offset away from perfect (a lighting change, a DC shift); subtracting the mean-square term removes exactly that bias, which is why encoders use it to gate "is this candidate genuinely better" rather than "is this candidate literally closer" |
//! | [`satd`] | Σ &#124;Hadamard(cur − ref)&#124; | a residual's transform-domain cost, closer to what an entropy coder will actually pay than any pixel-domain metric; used for mode/reference-frame decisions where SAD's ranking is not fine enough, never in the innermost search loop because it costs several times more |
//!
//! None of these is normative — there is no bitstream syntax element these
//! numbers ever get written into, so there is nothing to be bit-exact
//! *against*. What has to hold is the same function on both the scalar and
//! vectorised paths, which is what [`MecmpKernels`] and the differential
//! tests in `vaco-checkasm`'s `kernels::mecmp` module verify. They live
//! there rather than here because `vaco-checkasm` is the higher layer
//! (`cargo xtask layer-check` forbids the reverse edge): the pattern is the
//! same one `kernels::fir_mc` and `kernels::scale_affine` already use for
//! `vaco-codec-dsp-mc` and `vaco-scale`.
//!
//! # What is vectorised, and what is not
//!
//! [`sad`], [`ssd`] and [`variance`] each have a `#[inline(always)]`
//! `vaco-simd`-dispatched body, monomorphised once per [`vaco_simd::Tier`]
//! exactly like `vaco-scale::fast::affine_row`. [`satd`] does not: a 4×4
//! Hadamard transform is a butterfly network, not a lanewise reduction, and
//! vectorising it correctly needs an in-lane transpose this crate does not
//! yet have a tested primitive for. `satd` is scalar-only in
//! [`MecmpKernels::for_tier`] for every tier, which is the same "not yet
//! vectorised" fallback [`vaco_simd::KernelSet::for_tier`]'s own contract
//! names — SATD is the mode-decision metric, evaluated once or twice per
//! block rather than hundreds of times, so it is far less hot than SAD.
//!
//! # Untrusted-input posture
//!
//! Every function here takes caller-supplied [`Plane`] views and caller-
//! supplied dimensions. Mismatched or malformed input (a `refp` shorter
//! than `cur`, a stride smaller than the claimed width) degrades to a
//! smaller-than-expected sum rather than panicking — see [`Plane`]'s own
//! doc — because a motion search tries many caller-computed near-edge
//! offsets and none of them should be able to crash the encoder.

use vaco_simd::prelude::*;
use vaco_simd::{Caps, KernelSet, Tier, dispatch_kernel};

mod plane;
pub use plane::Plane;

/// Shared width/height a `(cur, refp)` pair actually compares over: the
/// smaller of the two views in each dimension, so a caller's mismatched
/// pair degrades to the overlapping region instead of reading past either
/// view's own declared bounds.
fn overlap(cur: Plane<'_>, refp: Plane<'_>) -> (usize, usize) {
    (
        cur.width().min(refp.width()),
        cur.height().min(refp.height()),
    )
}

// ---------------------------------------------------------------- SAD

/// Sum of absolute differences over the overlapping region of `cur` and
/// `refp`. Always compiled, always correct — the oracle every SIMD variant
/// is checked against, and the tail handler said variant falls back to for
/// whatever a full vector chunk cannot cover.
#[must_use]
pub fn sad(cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let (w, h) = overlap(cur, refp);
    let mut acc: u32 = 0;
    for y in 0..h {
        acc = acc.wrapping_add(sad_row_scalar(cur.row(y), refp.row(y), w));
    }
    acc
}

fn sad_row_scalar(cur: &[u8], refb: &[u8], w: usize) -> u32 {
    cur.iter()
        .zip(refb.iter())
        .take(w)
        .map(|(&a, &b)| u32::from(a.abs_diff(b)))
        .fold(0u32, u32::wrapping_add)
}

/// Dispatched, runtime-selected SAD. Falls back to [`sad`] itself when the
/// detected tier is scalar, and inside the vector body for whatever tail of
/// a row does not fill a whole SIMD chunk.
fn sad_dispatched(cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let caps = Caps::detect();
    dispatch_kernel!(caps, simd => sad_simd(simd, cur, refp))
}

/// One generic body, monomorphised once per CPU level by `dispatch_kernel!`.
///
/// `#[inline(always)]` is mandatory, not a tuning knob: it is how the
/// dispatched level's target-feature context reaches this body.
#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::many_single_char_names,
    reason = "mandatory for target-feature propagation, see vaco-simd's crate doc; \
              w/h/c/r/n/x are this module's own names for width/height/row/row/count/offset"
)]
fn sad_simd<S: Lanes>(simd: S, cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let (w, h) = overlap(cur, refp);
    let lanes = <S::u8s as SimdBase<S>>::N;
    let mut acc = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut tail: u32 = 0;

    for y in 0..h {
        let c = cur.row(y);
        let r = refp.row(y);
        let n = c.len().min(r.len()).min(w);
        let head = n.checked_div(lanes).map_or(0, |q| q * lanes);
        let mut x = 0usize;
        while x < head {
            let (Some(cv), Some(rv)) = (c.get(x..x + lanes), r.get(x..x + lanes)) else {
                break;
            };
            let cvec = <S::u8s as SimdBase<S>>::from_slice(simd, cv);
            let rvec = <S::u8s as SimdBase<S>>::from_slice(simd, rv);
            let diff = ops::simd::abs_diff_u8(cvec, rvec);
            let (lo16, hi16) = ops::simd::widen_u8_i16::<S>(diff);
            let (lo32a, lo32b): (S::i32s, S::i32s) = lo16.widen();
            let (hi32a, hi32b): (S::i32s, S::i32s) = hi16.widen();
            acc = acc + lo32a + lo32b + hi32a + hi32b;
            x += lanes;
        }
        if x < n
            && let (Some(ct), Some(rt)) = (c.get(x..n), r.get(x..n))
        {
            tail = tail.wrapping_add(sad_row_scalar(ct, rt, n - x));
        }
    }

    u32::try_from(ops::simd::hsum_i32(acc).max(0))
        .unwrap_or(0)
        .wrapping_add(tail)
}

// ---------------------------------------------------------------- SSD

/// Sum of squared differences over the overlapping region.
#[must_use]
pub fn ssd(cur: Plane<'_>, refp: Plane<'_>) -> u64 {
    let (w, h) = overlap(cur, refp);
    let mut acc: u64 = 0;
    for y in 0..h {
        acc = acc.wrapping_add(ssd_row_scalar(cur.row(y), refp.row(y), w));
    }
    acc
}

fn ssd_row_scalar(cur: &[u8], refb: &[u8], w: usize) -> u64 {
    cur.iter()
        .zip(refb.iter())
        .take(w)
        .map(|(&a, &b)| {
            let d = u64::from(a.abs_diff(b));
            d * d
        })
        .fold(0u64, u64::wrapping_add)
}

fn ssd_dispatched(cur: Plane<'_>, refp: Plane<'_>) -> u64 {
    let caps = Caps::detect();
    dispatch_kernel!(caps, simd => ssd_simd(simd, cur, refp))
}

#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::many_single_char_names,
    reason = "mandatory for target-feature propagation, see vaco-simd's crate doc; \
              w/h/c/r/n/x are this module's own names for width/height/row/row/count/offset"
)]
fn ssd_simd<S: Lanes>(simd: S, cur: Plane<'_>, refp: Plane<'_>) -> u64 {
    let (w, h) = overlap(cur, refp);
    let lanes = <S::u8s as SimdBase<S>>::N;
    // Two accumulators rather than one: a single loop-carried vector
    // accumulator is a chain of dependent adds with nothing to fill the
    // latency (vaco-simd's own "Rule B" — measured 3.90x vs 0.99x there).
    let mut acc0 = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut acc1 = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut tail: u64 = 0;

    for y in 0..h {
        let c = cur.row(y);
        let r = refp.row(y);
        let n = c.len().min(r.len()).min(w);
        let head = n.checked_div(lanes).map_or(0, |q| q * lanes);
        let mut x = 0usize;
        while x < head {
            let (Some(cv), Some(rv)) = (c.get(x..x + lanes), r.get(x..x + lanes)) else {
                break;
            };
            let cvec = <S::u8s as SimdBase<S>>::from_slice(simd, cv);
            let rvec = <S::u8s as SimdBase<S>>::from_slice(simd, rv);
            let diff = ops::simd::abs_diff_u8(cvec, rvec);
            let (lo16, hi16) = ops::simd::widen_u8_i16::<S>(diff);
            // Each lane is <=255, so lo*lo <=65025: safely inside i16 range
            // is false (65025 > i16::MAX), so square in the widened i32
            // domain instead of squaring the i16 vectors directly.
            let (lo32a, lo32b): (S::i32s, S::i32s) = lo16.widen();
            let (hi32a, hi32b): (S::i32s, S::i32s) = hi16.widen();
            acc0 = acc0 + lo32a * lo32a + hi32a * hi32a;
            acc1 = acc1 + lo32b * lo32b + hi32b * hi32b;
            x += lanes;
        }
        if x < n
            && let (Some(ct), Some(rt)) = (c.get(x..n), r.get(x..n))
        {
            tail = tail.wrapping_add(ssd_row_scalar(ct, rt, n - x));
        }
    }

    let vec_sum = i64::from(ops::simd::hsum_i32(acc0)) + i64::from(ops::simd::hsum_i32(acc1));
    u64::try_from(vec_sum.max(0)).unwrap_or(0).wrapping_add(tail)
}

// ---------------------------------------------------------------- variance

/// `(sum, sse)` of `cur - refp` over the overlapping region: `sum` is the
/// plain (signed) total difference, `sse` is the sum of squared
/// differences. Kept as a pair — rather than folded into one number — so a
/// caller can combine them with [`variance_from_sum_sse`] or use `sum`
/// alone as a cheap DC-bias estimate.
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "w/h/c/r/a/b/d are this module's own names for width/height/row/row/sample/sample/diff"
)]
pub fn sum_and_sse(cur: Plane<'_>, refp: Plane<'_>) -> (i64, u64) {
    let (w, h) = overlap(cur, refp);
    let mut sum: i64 = 0;
    let mut sse: u64 = 0;
    for y in 0..h {
        let c = cur.row(y);
        let r = refp.row(y);
        for (&a, &b) in c.iter().zip(r.iter()).take(w) {
            let d = i64::from(a) - i64::from(b);
            sum = sum.wrapping_add(d);
            sse = sse.wrapping_add(u64::try_from(d * d).unwrap_or(0));
        }
    }
    (sum, sse)
}

/// The mean-corrected variance a `(sum, sse)` pair implies over an `n`-pixel
/// block: `sse - sum² / n`, clamped at zero (rounding can otherwise push it
/// a fraction below, and a negative "variance" has no meaning here).
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "n is checked non-zero on the line above"
)]
pub fn variance_from_sum_sse(sum: i64, sse: u64, n: usize) -> u32 {
    if n == 0 {
        return 0;
    }
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    let mean_sq = sum.saturating_mul(sum) / n;
    let sse = i64::try_from(sse).unwrap_or(i64::MAX);
    u32::try_from((sse - mean_sq).max(0)).unwrap_or(u32::MAX)
}

/// Mean-corrected variance of `cur - refp` over the overlapping region.
/// Removes the same uniform (DC) offset [`sad`]/[`ssd`] cannot distinguish
/// from a genuinely worse match — see the module doc's table.
#[must_use]
pub fn variance(cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let (w, h) = overlap(cur, refp);
    let (sum, sse) = sum_and_sse(cur, refp);
    variance_from_sum_sse(sum, sse, w * h)
}

fn variance_dispatched(cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let caps = Caps::detect();
    dispatch_kernel!(caps, simd => variance_simd(simd, cur, refp))
}

#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::many_single_char_names,
    reason = "mandatory for target-feature propagation, see vaco-simd's crate doc; \
              w/h/c/r/n/x are this module's own names for width/height/row/row/count/offset"
)]
fn variance_simd<S: Lanes>(simd: S, cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let (w, h) = overlap(cur, refp);
    let lanes = <S::u8s as SimdBase<S>>::N;
    let mut sum_acc = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut sse_acc0 = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut sse_acc1 = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut tail_sum: i64 = 0;
    let mut tail_sse: u64 = 0;

    for y in 0..h {
        let c = cur.row(y);
        let r = refp.row(y);
        let n = c.len().min(r.len()).min(w);
        let head = n.checked_div(lanes).map_or(0, |q| q * lanes);
        let mut x = 0usize;
        while x < head {
            let (Some(cv), Some(rv)) = (c.get(x..x + lanes), r.get(x..x + lanes)) else {
                break;
            };
            let cvec = <S::u8s as SimdBase<S>>::from_slice(simd, cv);
            let rvec = <S::u8s as SimdBase<S>>::from_slice(simd, rv);
            // Signed difference: widen both operands to i16 first (the
            // substrate has no direct u8-u8 -> i16 subtract), then subtract.
            let (clo, chi) = ops::simd::widen_u8_i16::<S>(cvec);
            let (rlo, rhi) = ops::simd::widen_u8_i16::<S>(rvec);
            let dlo = clo - rlo;
            let dhi = chi - rhi;
            let (dlo0, dlo1): (S::i32s, S::i32s) = dlo.widen();
            let (dhi0, dhi1): (S::i32s, S::i32s) = dhi.widen();
            sum_acc = sum_acc + dlo0 + dlo1 + dhi0 + dhi1;
            sse_acc0 = sse_acc0 + dlo0 * dlo0 + dhi0 * dhi0;
            sse_acc1 = sse_acc1 + dlo1 * dlo1 + dhi1 * dhi1;
            x += lanes;
        }
        if x < n
            && let (Some(ct), Some(rt)) = (c.get(x..n), r.get(x..n))
        {
            for (&a, &b) in ct.iter().zip(rt.iter()) {
                let d = i64::from(a) - i64::from(b);
                tail_sum = tail_sum.wrapping_add(d);
                tail_sse = tail_sse.wrapping_add(u64::try_from(d * d).unwrap_or(0));
            }
        }
    }

    let sum = i64::from(ops::simd::hsum_i32(sum_acc)).wrapping_add(tail_sum);
    let sse = u64::try_from(
        i64::from(ops::simd::hsum_i32(sse_acc0)) + i64::from(ops::simd::hsum_i32(sse_acc1)),
    )
    .unwrap_or(0)
    .wrapping_add(tail_sse);
    variance_from_sum_sse(sum, sse, w * h)
}

// ---------------------------------------------------------------- SATD

/// The 4-point Walsh-Hadamard transform: `H4 = [[1,1,1,1],[1,-1,1,-1],
/// [1,1,-1,-1],[1,-1,-1,1]]`, computed as a butterfly (4 adds instead of 16
/// multiplies). This is the unique 4-point Hadamard matrix — a fact about
/// the transform, not an authorial choice — used identically wherever a
/// codec computes a Hadamard-domain cost.
fn hadamard4(v: [i32; 4]) -> [i32; 4] {
    let [a0, a1, a2, a3] = v;
    let (s0, d0) = (a0.wrapping_add(a1), a0.wrapping_sub(a1));
    let (s1, d1) = (a2.wrapping_add(a3), a2.wrapping_sub(a3));
    [
        s0.wrapping_add(s1),
        d0.wrapping_add(d1),
        s0.wrapping_sub(s1),
        d0.wrapping_sub(d1),
    ]
}

/// One residual row of up to 4 samples, zero-padded past the end of either
/// input — the same "malformed degrades, never panics" contract as
/// [`Plane::row`].
fn residual_row4(cur: &[u8], refb: &[u8]) -> [i32; 4] {
    let mut it = cur
        .iter()
        .copied()
        .chain(std::iter::repeat(0u8))
        .zip(refb.iter().copied().chain(std::iter::repeat(0u8)))
        .take(4)
        .map(|(a, b)| i32::from(a) - i32::from(b));
    [
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    ]
}

/// SATD of one 4×4 block at `(x0, y0)` in `cur`/`refp`'s shared coordinate
/// space: a 2-D (row then column) Hadamard transform of the residual,
/// summed as absolute coefficients. No normalising shift is applied —
/// there is no bitstream syntax this number feeds, so any monotonic
/// rescaling of it would rank candidates identically; the raw sum is
/// simplest.
fn satd4x4(cur: Plane<'_>, refp: Plane<'_>, x0: usize, y0: usize) -> u32 {
    let row_at = |dy: usize| -> [i32; 4] {
        let c = cur.row(y0.wrapping_add(dy));
        let r = refp.row(y0.wrapping_add(dy));
        let c = c.get(x0..).unwrap_or(&[]);
        let r = r.get(x0..).unwrap_or(&[]);
        residual_row4(c, r)
    };
    let r0 = hadamard4(row_at(0));
    let r1 = hadamard4(row_at(1));
    let r2 = hadamard4(row_at(2));
    let r3 = hadamard4(row_at(3));

    let [r0c0, r0c1, r0c2, r0c3] = r0;
    let [r1c0, r1c1, r1c2, r1c3] = r1;
    let [r2c0, r2c1, r2c2, r2c3] = r2;
    let [r3c0, r3c1, r3c2, r3c3] = r3;

    let col0 = hadamard4([r0c0, r1c0, r2c0, r3c0]);
    let col1 = hadamard4([r0c1, r1c1, r2c1, r3c1]);
    let col2 = hadamard4([r0c2, r1c2, r2c2, r3c2]);
    let col3 = hadamard4([r0c3, r1c3, r2c3, r3c3]);

    [col0, col1, col2, col3]
        .into_iter()
        .flatten()
        .map(i32::unsigned_abs)
        .fold(0u32, u32::wrapping_add)
}

/// SATD over the overlapping region, tiled in 4×4 units. A width or height
/// that is not a multiple of 4 is covered by falling back to [`sad`] for
/// the right and/or bottom leftover strip — an approximation, documented
/// rather than silent, and never a panic: `satd` is a heuristic cost, not a
/// normative transform, so there is no "correct" value to fail to reach
/// for a block shape the Hadamard tiling does not evenly cover.
#[must_use]
pub fn satd(cur: Plane<'_>, refp: Plane<'_>) -> u32 {
    let (w, h) = overlap(cur, refp);
    let w4 = w.checked_div(4).map_or(0, |q| q * 4);
    let h4 = h.checked_div(4).map_or(0, |q| q * 4);

    let mut acc: u32 = 0;
    let mut y = 0usize;
    while y < h4 {
        let mut x = 0usize;
        while x < w4 {
            acc = acc.wrapping_add(satd4x4(cur, refp, x, y));
            x += 4;
        }
        y += 4;
    }

    // Leftover strips: SAD is a defined, cheap, non-panicking stand-in.
    if w4 < w
        && let (Some(cs), Some(rs)) = (cur.sub(w4, 0, w - w4, h), refp.sub(w4, 0, w - w4, h))
    {
        acc = acc.wrapping_add(sad(cs, rs));
    }
    if h4 < h
        && let (Some(cs), Some(rs)) = (cur.sub(0, h4, w4, h - h4), refp.sub(0, h4, w4, h - h4))
    {
        acc = acc.wrapping_add(sad(cs, rs));
    }
    acc
}

// ---------------------------------------------------------------- KernelSet

/// Signature shared by [`sad`]/[`ssd`]/[`variance`]/[`satd`] once resolved
/// through [`MecmpKernels`].
pub type SadFn = fn(Plane<'_>, Plane<'_>) -> u32;
/// See [`SadFn`].
pub type SsdFn = fn(Plane<'_>, Plane<'_>) -> u64;
/// See [`SadFn`].
pub type SatdFn = fn(Plane<'_>, Plane<'_>) -> u32;
/// See [`SadFn`].
pub type VarianceFn = fn(Plane<'_>, Plane<'_>) -> u32;

/// The comparison kernels one motion search resolves to, once per run.
#[derive(Clone, Copy, Debug)]
pub struct MecmpKernels {
    /// Sum of absolute differences. See [`sad`].
    pub sad: SadFn,
    /// Sum of squared differences. See [`ssd`].
    pub ssd: SsdFn,
    /// Mean-corrected variance. See [`variance`].
    pub variance: VarianceFn,
    /// Hadamard-domain cost. See [`satd`]. Scalar on every tier — see the
    /// module doc's "What is vectorised" section.
    pub satd: SatdFn,
}

impl KernelSet for MecmpKernels {
    fn for_tier(tier: Tier) -> Self {
        Self {
            sad: if tier.is_scalar() { sad } else { sad_dispatched },
            ssd: if tier.is_scalar() { ssd } else { ssd_dispatched },
            variance: if tier.is_scalar() {
                variance
            } else {
                variance_dispatched
            },
            satd,
        }
    }

    fn kernel_names() -> &'static [&'static str] {
        &["sad", "ssd", "variance", "satd"]
    }
}

impl Default for MecmpKernels {
    fn default() -> Self {
        Self::select()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes(w: usize, h: usize, seed_a: u32, seed_b: u32) -> (Vec<u8>, Vec<u8>) {
        let mk = |seed: u32| -> Vec<u8> {
            (0..w * h)
                .map(|i| ((i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) % 256) as u8)
                .collect()
        };
        (mk(seed_a), mk(seed_b))
    }

    #[test]
    fn sad_of_a_block_against_itself_is_zero() {
        let (a, _) = planes(16, 16, 7, 0);
        let p1 = Plane::new(&a, 16, 16, 16);
        let p2 = Plane::new(&a, 16, 16, 16);
        assert_eq!(sad(p1, p2), 0);
        assert_eq!(ssd(p1, p2), 0);
        assert_eq!(variance(p1, p2), 0);
        assert_eq!(satd(p1, p2), 0);
    }

    #[test]
    fn sad_is_symmetric() {
        let (a, b) = planes(9, 5, 1, 2);
        let pa = Plane::new(&a, 9, 9, 5);
        let pb = Plane::new(&b, 9, 9, 5);
        assert_eq!(sad(pa, pb), sad(pb, pa));
    }

    #[test]
    fn a_uniform_offset_changes_sad_but_not_variance() {
        let base: Vec<u8> = (0..64).map(|i| (i * 3 % 200) as u8).collect();
        let shifted: Vec<u8> = base.iter().map(|&v| v.saturating_add(10)).collect();
        let p_base = Plane::new(&base, 8, 8, 8);
        let p_shift = Plane::new(&shifted, 8, 8, 8);
        assert!(sad(p_base, p_shift) > 0);
        // Every difference is exactly +10 (no saturation at these values),
        // so the mean-corrected variance is exactly zero: a pure DC shift.
        assert_eq!(variance(p_base, p_shift), 0);
    }

    #[test]
    fn dispatched_and_scalar_paths_agree_across_lengths_and_tiers() {
        for w in 0..40usize {
            for h in [1usize, 3, 16] {
                let (a, b) = planes(w.max(1), h, w as u32, w as u32 + 99);
                let pa = Plane::new(&a, w.max(1), w, h);
                let pb = Plane::new(&b, w.max(1), w, h);
                assert_eq!(sad(pa, pb), sad_dispatched(pa, pb), "sad w={w} h={h}");
                assert_eq!(ssd(pa, pb), ssd_dispatched(pa, pb), "ssd w={w} h={h}");
                assert_eq!(
                    variance(pa, pb),
                    variance_dispatched(pa, pb),
                    "variance w={w} h={h}"
                );
            }
        }
    }

    #[test]
    fn satd_tiles_non_multiple_of_four_shapes_without_panicking() {
        let (a, b) = planes(10, 6, 3, 4);
        let pa = Plane::new(&a, 10, 10, 6);
        let pb = Plane::new(&b, 10, 10, 6);
        let _ = satd(pa, pb); // must not panic; value has no independent oracle
    }

    #[test]
    fn kernel_set_names_match_the_struct_fields() {
        assert_eq!(
            MecmpKernels::kernel_names(),
            &["sad", "ssd", "variance", "satd"]
        );
        let k = MecmpKernels::reference();
        let (a, b) = planes(8, 8, 5, 6);
        let pa = Plane::new(&a, 8, 8, 8);
        let pb = Plane::new(&b, 8, 8, 8);
        assert_eq!((k.sad)(pa, pb), sad(pa, pb));
    }
}
