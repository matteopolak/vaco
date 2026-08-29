//! Operations `fearless_simd` does not provide, composed from ones it does.
//!
//! Measured during the D12 adoption review; the real numbers are in
//! `docs/core/simd-adoption-measurements.md`. Most compose cheaply; the exception
//! is widening multiply-add, which has no composition and is the single largest
//! performance risk in the project (plan 12).
//!
//! # Layout: two mirrored namespaces
//!
//! * **This module** holds the **scalar references** — one lane, obviously
//!   correct, written with `std`'s own primitives wherever one exists. These are
//!   the oracle.
//! * **[`simd`]** holds the **vector compositions** under the *same names*. Each
//!   is proved lane-for-lane equal to its scalar sibling by a proptest in
//!   `tests/ops_agree.rs`.
//!
//! So `ops::rounded_avg_u8(a, b)` and `ops::simd::rounded_avg_u8(a, b)` compute
//! the same function, on one lane and on N lanes respectively.
//!
//! # Why these live here and not in kernels
//!
//! Plan 11 §5.4 rule 5: anything in the gap table is called through this module,
//! never open-coded. One composition, one place to fix, and one place to *delete*
//! when the substrate grows the operation natively. `kernel!` — the substrate's
//! escape hatch to raw intrinsics — expands `unsafe` into the calling crate and
//! is therefore closed to us (D12 addendum), so composition is the only route.

/// Unsigned saturating add: `min(a, !b) + b`. 3 operations.
#[must_use]
pub fn saturating_add_u8(a: u8, b: u8) -> u8 {
    a.saturating_add(b)
}

/// Unsigned saturating subtract: `max(a, b) - b`. 2 operations.
#[must_use]
pub fn saturating_sub_u8(a: u8, b: u8) -> u8 {
    a.saturating_sub(b)
}

/// Signed saturating add on `i16`. `widen` → add → `saturating_narrow`, ~5 operations.
#[must_use]
pub fn saturating_add_i16(a: i16, b: i16) -> i16 {
    a.saturating_add(b)
}

/// Signed saturating subtract on `i16`. `widen` → sub → `saturating_narrow`, ~5 operations.
#[must_use]
pub fn saturating_sub_i16(a: i16, b: i16) -> i16 {
    a.saturating_sub(b)
}

/// Rounded average: `(a | b) - ((a ^ b) >> 1)`. 4 operations, exact, and
/// stays in width — better than the obvious widen-add-shift-narrow.
#[must_use]
pub const fn rounded_avg_u8(a: u8, b: u8) -> u8 {
    (a | b) - ((a ^ b) >> 1)
}

/// Truncating average: `(a & b) + ((a ^ b) >> 1)`. 4 operations, same properties
/// as [`rounded_avg_u8`].
#[must_use]
pub const fn truncated_avg_u8(a: u8, b: u8) -> u8 {
    (a & b) + ((a ^ b) >> 1)
}

/// Absolute difference: `max(a, b) - min(a, b)`. 3 operations.
#[must_use]
pub const fn abs_diff_u8(a: u8, b: u8) -> u8 {
    a.abs_diff(b)
}

/// Integer absolute value on `i16`: `max(x, -x)`. 2 operations.
///
/// Saturating, so `abs_i16(i16::MIN) == i16::MAX`, matching what `max(x, -x)`
/// produces in a vector where `-i16::MIN` wraps to `i16::MIN`… except it does
/// not: `max(i16::MIN, i16::MIN)` is `i16::MIN`. The vector composition and this
/// reference agree on that, and the proptest pins it. Callers who cannot accept
/// `i16::MIN` passing through must range-limit first.
#[must_use]
pub const fn abs_i16(x: i16) -> i16 {
    // `wrapping_neg` reproduces exactly what the vector `neg` does at MIN.
    let n = x.wrapping_neg();
    if x > n { x } else { n }
}

/// Integer absolute value on `i32`: `max(x, -x)`. 2 operations. Same `MIN`
/// caveat as [`abs_i16`].
#[must_use]
pub const fn abs_i32(x: i32) -> i32 {
    let n = x.wrapping_neg();
    if x > n { x } else { n }
}

/// Horizontal sum of `i32` lanes, wrapping. `2·log₂N + 1` operations as a tree.
///
/// **Hoist this out of loops.** Keep a vector accumulator across the loop body
/// and reduce once per kernel invocation; a reduction inside an inner loop
/// serialises the whole thing.
#[must_use]
pub fn hsum_i32(lanes: &[i32]) -> i32 {
    lanes.iter().copied().fold(0i32, i32::wrapping_add)
}

/// Horizontal minimum of `i32` lanes. Empty input yields [`i32::MAX`].
#[must_use]
pub fn hmin_i32(lanes: &[i32]) -> i32 {
    lanes.iter().copied().fold(i32::MAX, i32::min)
}

/// Horizontal maximum of `i32` lanes. Empty input yields [`i32::MIN`].
#[must_use]
pub fn hmax_i32(lanes: &[i32]) -> i32 {
    lanes.iter().copied().fold(i32::MIN, i32::max)
}

/// One output lane of the `pmaddwd` shape: a pairwise dot product of adjacent
/// `i16` pairs, accumulated into `i32`.
///
/// **This is the expensive one.** There is no composition; the vector form is
/// widen ×2, multiply ×2, unzip ×2, add — see [`simd::madd_i16_i32`] and the
/// measurement report. Prefer [`wmla_u8_i16`], which needs no widening multiply
/// at all, whenever the coefficient is a broadcast scalar rather than a vector.
#[must_use]
pub const fn madd_i16_i32(a0: i16, b0: i16, a1: i16, b1: i16) -> i32 {
    // Wrapping, because the vector form wraps: `i16::MIN * i16::MIN` twice is
    // `2^31`, one past `i32::MAX`. `pmaddwd` itself wraps there too.
    ((a0 as i32).wrapping_mul(b0 as i32)).wrapping_add((a1 as i32).wrapping_mul(b1 as i32))
}

/// One output lane of a broadcast-coefficient widening multiply-accumulate:
/// `acc + u8 × i16`, in `i16`.
///
/// This is the shape a separable FIR, a colour matrix and an interpolation
/// filter actually need, and it needs **no** widening multiply: the widen
/// happens once per source vector rather than once per tap. Wrapping on
/// overflow, matching the vector form.
#[must_use]
pub const fn wmla_u8_i16(acc: i16, v: u8, c: i16) -> i16 {
    acc.wrapping_add((v as i16).wrapping_mul(c))
}

/// Clip an `i32` to `0..=255`. The scalar sibling of [`simd::pack_u8_from_i16`].
#[must_use]
pub const fn clip_u8(x: i32) -> u8 {
    if x < 0 {
        0
    } else if x > 255 {
        255
    } else {
        x as u8
    }
}

/// Transpose a 4x4 matrix: `out[r][c] = m[c][r]`. The scalar sibling of
/// [`simd::transpose4x4_i32`] — see that function's doc for why a 2-D
/// separable transform needs this at all.
#[must_use]
pub const fn transpose4x4_i32(m: [[i32; 4]; 4]) -> [[i32; 4]; 4] {
    [
        [m[0][0], m[1][0], m[2][0], m[3][0]],
        [m[0][1], m[1][1], m[2][1], m[3][1]],
        [m[0][2], m[1][2], m[2][2], m[3][2]],
        [m[0][3], m[1][3], m[2][3], m[3][3]],
    ]
}

/// Scalar reference for the masked-lane-select row op ([`dispatched_select_u8_row`]):
/// pick `a` where `mask` is nonzero, else `b`.
///
/// `mask` need not be canonical (`0`/`0xFF`) here — only [`simd::select_u8`]'s
/// substrate-level input has that requirement, and this oracle exists
/// precisely so a caller can hand it whatever the mask actually is.
#[must_use]
pub const fn select_u8(mask: u8, a: u8, b: u8) -> u8 {
    if mask != 0 { a } else { b }
}

/// The vector compositions. Same names, same semantics, N lanes at a time.
///
/// Every function here is `#[inline(always)]`, without exception: that is how
/// the dispatched level's target-feature context reaches the body. Removing one
/// does not make the code slower in a way any test would catch — it makes it
/// compile at the ambient baseline, silently.
pub mod simd {
    use crate::Lanes;
    use fearless_simd::{Bytes, Select, SimdBase, SimdInt, SimdNarrow, SimdWiden};

    /// Unsigned saturating add: `min(a, !b) + b`.
    ///
    /// Exact, in-width, no widening. Generic over lane count, so it works on
    /// `S::u8s` (native width) and on the fixed `u8x16`/`u8x32`/`u8x64` alike.
    #[inline(always)]
    pub fn saturating_add_u8<S: Lanes, V: SimdInt<S, Element = u8>>(a: V, b: V) -> V {
        a.min(!b) + b
    }

    /// Unsigned saturating subtract: `max(a, b) - b`.
    #[inline(always)]
    pub fn saturating_sub_u8<S: Lanes, V: SimdInt<S, Element = u8>>(a: V, b: V) -> V {
        a.max(b) - b
    }

    /// Unsigned saturating add on `u16` lanes. Same identity, same cost.
    #[inline(always)]
    pub fn saturating_add_u16<S: Lanes, V: SimdInt<S, Element = u16>>(a: V, b: V) -> V {
        a.min(!b) + b
    }

    /// Unsigned saturating subtract on `u16` lanes.
    #[inline(always)]
    pub fn saturating_sub_u16<S: Lanes, V: SimdInt<S, Element = u16>>(a: V, b: V) -> V {
        a.max(b) - b
    }

    /// Signed saturating add on `i16` lanes: `widen` → add → `saturating_narrow`.
    ///
    /// The widening is what costs; `saturating_narrow` itself is native. Note
    /// the shape: two i32 halves are added and re-packed, so this is ~5
    /// operations for one output vector.
    #[inline(always)]
    pub fn saturating_add_i16<S, V>(a: V, b: V) -> V
    where
        S: Lanes,
        V: SimdInt<S, Element = i16> + SimdWiden<S>,
        V::Widened: SimdNarrow<S, Narrowed = V>,
    {
        let (al, ah) = a.widen();
        let (bl, bh) = b.widen();
        (al + bl).saturating_narrow(ah + bh)
    }

    /// Signed saturating subtract on `i16` lanes.
    #[inline(always)]
    pub fn saturating_sub_i16<S, V>(a: V, b: V) -> V
    where
        S: Lanes,
        V: SimdInt<S, Element = i16> + SimdWiden<S>,
        V::Widened: SimdNarrow<S, Narrowed = V>,
    {
        let (al, ah) = a.widen();
        let (bl, bh) = b.widen();
        (al - bl).saturating_narrow(ah - bh)
    }

    /// Rounded average (`pavgb` / `urhadd`): `(a | b) - ((a ^ b) >> 1)`.
    ///
    /// Exact and cannot overflow, because `(a ^ b) >> 1 <= a | b` always. That
    /// is why bi-prediction averaging in motion compensation never has to widen.
    #[inline(always)]
    pub fn rounded_avg_u8<S: Lanes, V: SimdInt<S, Element = u8>>(a: V, b: V) -> V {
        (a | b) - ((a ^ b) >> 1u32)
    }

    /// Truncating average: `(a & b) + ((a ^ b) >> 1)`.
    #[inline(always)]
    pub fn truncated_avg_u8<S: Lanes, V: SimdInt<S, Element = u8>>(a: V, b: V) -> V {
        (a & b) + ((a ^ b) >> 1u32)
    }

    /// Lanewise absolute difference: `max(a, b) - min(a, b)`.
    ///
    /// Cheap in isolation. The cost of a SAD is in the *accumulation*, which
    /// `psadbw` does for free and we must do with a widen and an add — keep a
    /// `u16`/`i32` vector accumulator and reduce once, never per block.
    #[inline(always)]
    pub fn abs_diff_u8<S: Lanes, V: SimdInt<S, Element = u8>>(a: V, b: V) -> V {
        a.max(b) - a.min(b)
    }

    /// Integer absolute value: `max(x, -x)`.
    ///
    /// `abs` exists on `SimdFloat` only; there is no integer `abs` and no
    /// integer `mul_add` in the substrate. `i16::MIN` passes through unchanged,
    /// exactly as the scalar reference documents.
    #[inline(always)]
    pub fn abs_i16<S, V>(x: V) -> V
    where
        S: Lanes,
        V: SimdInt<S, Element = i16> + core::ops::Neg<Output = V>,
    {
        x.max(-x)
    }

    /// Integer absolute value on `i32` lanes. Same `MIN` caveat.
    #[inline(always)]
    pub fn abs_i32<S, V>(x: V) -> V
    where
        S: Lanes,
        V: SimdInt<S, Element = i32> + core::ops::Neg<Output = V>,
    {
        x.max(-x)
    }

    /// Horizontal sum of `i32` lanes, wrapping.
    ///
    /// Generic over lane count via the vector's own slice view, which the
    /// backends lower to a native reduction where one exists (`addv` on NEON).
    /// [`hsum_i32x4_tree`] is the explicit rotate-and-add tree plan 11 §5.6
    /// specifies; the measurement report says which is faster here.
    ///
    /// **Hoist it out of loops** either way.
    #[inline(always)]
    pub fn hsum_i32<S: Lanes, V: SimdInt<S, Element = i32>>(v: V) -> i32 {
        v.as_slice().iter().copied().fold(0i32, i32::wrapping_add)
    }

    /// Horizontal minimum of `i32` lanes.
    #[inline(always)]
    pub fn hmin_i32<S: Lanes, V: SimdInt<S, Element = i32>>(v: V) -> i32 {
        v.as_slice().iter().copied().fold(i32::MAX, i32::min)
    }

    /// Horizontal maximum of `i32` lanes.
    #[inline(always)]
    pub fn hmax_i32<S: Lanes, V: SimdInt<S, Element = i32>>(v: V) -> i32 {
        v.as_slice().iter().copied().fold(i32::MIN, i32::max)
    }

    /// The explicit `log₂N` rotate-and-add reduction tree, for the 128-bit block.
    ///
    /// Written out because the tree cannot be expressed generically: it needs
    /// `rotate_elements_left::<OFFSET>` with a *const* offset derived from `V::N`,
    /// and Rust cannot use an associated const as a const-generic argument.
    /// So a generic tree would need one impl per width. [`hsum_i32`] is the
    /// generic path; this exists to measure the difference.
    #[inline(always)]
    pub fn hsum_i32x4_tree<S: Lanes>(v: fearless_simd::i32x4<S>) -> i32 {
        let a = v + v.rotate_elements_left::<2>();
        let b = a + a.rotate_elements_left::<1>();
        b.as_slice().first().copied().unwrap_or(0)
    }

    /// The `pmaddwd` shape: pairwise dot product of adjacent `i16` pairs into
    /// `i32`. **No composition exists** — this is the assembled equivalent.
    ///
    /// `widen` ×2 (four vectors), `mul` ×2, `unzip_low`/`unzip_high`, `add`.
    /// One native instruction becomes roughly nine.
    ///
    /// A kernel PR that calls this must say in review why
    /// [`wmla_u8_i16`] does not fit, because the broadcast-coefficient shape
    /// needs no widening multiply at all and is several times cheaper.
    #[inline(always)]
    pub fn madd_i16_i32<S: Lanes>(a: S::i16s, b: S::i16s) -> S::i32s {
        let (a_lo, a_hi) = a.widen();
        let (b_lo, b_hi) = b.widen();
        let p_lo = a_lo * b_lo;
        let p_hi = a_hi * b_hi;
        // The concatenation [p_lo, p_hi] is the full lanewise product vector.
        // Its even lanes plus its odd lanes is exactly the pairwise dot product.
        p_lo.unzip_low(p_hi) + p_lo.unzip_high(p_hi)
    }

    /// Broadcast-coefficient widening multiply-accumulate: `acc += widen(v) * c`.
    ///
    /// Two `i16` accumulators cover one `u8` vector's worth of lanes, so this
    /// returns a pair. Costs widen(1) + mul(2) + add(2).
    ///
    /// **Prefer [`wmla_i16`] inside a tap loop**: hoist the widen out and the
    /// per-tap cost drops to a multiply and an add. That restructuring is the
    /// whole answer to the widening-multiply gap.
    #[inline(always)]
    pub fn wmla_u8_i16<S: Lanes>(acc: (S::i16s, S::i16s), v: S::u8s, c: i16) -> (S::i16s, S::i16s) {
        let (lo, hi) = v.widen();
        (
            wmla_i16::<S, S::i16s>(acc.0, lo.bitcast::<S::i16s>(), c),
            wmla_i16::<S, S::i16s>(acc.1, hi.bitcast::<S::i16s>(), c),
        )
    }

    /// The hoisted form: `acc + v * c` on already-widened `i16` lanes.
    ///
    /// Two operations, and on a backend with FMA-for-integers it would be one.
    /// This is the operation a tap loop should be built from.
    #[inline(always)]
    pub fn wmla_i16<S: Lanes, V: SimdInt<S, Element = i16>>(acc: V, v: V, c: i16) -> V {
        acc + v * c
    }

    /// Widen a `u8` vector straight to `i16` lanes, low half then high half.
    ///
    /// `widen` produces `u16`; every filter wants `i16`. The bitcast is free and
    /// value-preserving because a widened `u8` is at most 255.
    #[inline(always)]
    pub fn widen_u8_i16<S: Lanes>(v: S::u8s) -> (S::i16s, S::i16s) {
        let (lo, hi) = v.widen();
        (lo.bitcast::<S::i16s>(), hi.bitcast::<S::i16s>())
    }

    /// The 128-bit-block form of [`pack_u8_from_i16`].
    ///
    /// Needed as a separate function because the substrate's fixed-width types
    /// (`i16x8<S>`) and its native-width associated types (`S::i16s`) do not
    /// unify in a generic bound: `S::i16s` is `i16x8<S>` on NEON/SSE but
    /// `i16x16<S>` on AVX2. A helper that must name both the input and its
    /// narrowed *and* re-signed output cannot be written once for both families
    /// without an unnameable pile of `where` clauses. This duplication is the
    /// price, and it is a real (small) ergonomic cost of the substrate.
    #[inline(always)]
    pub fn pack_u8_from_i16x8<S: Lanes>(
        lo: fearless_simd::i16x8<S>,
        hi: fearless_simd::i16x8<S>,
    ) -> fearless_simd::u8x16<S> {
        let zero = fearless_simd::i16x8::splat(lo.witness(), 0);
        let lo = lo.max(zero).bitcast::<fearless_simd::u16x8<S>>();
        let hi = hi.max(zero).bitcast::<fearless_simd::u16x8<S>>();
        lo.saturating_narrow(hi)
    }

    /// Lanewise select at native width: pick `a` where `mask` is true, else `b`.
    ///
    /// **Not a composition.** `S::mask8s` already implements `Select<S::u8s>`
    /// (#127's spike measured this directly: there was no gap to fill), so
    /// this is a one-line pass to the substrate's native masked-lane select —
    /// `pblendvb`/`vpternlog` on x86, `bsl` on NEON. It is named here anyway
    /// so a kernel body reaches it under this crate's own vocabulary rather
    /// than importing `fearless_simd::Select` directly, matching every other
    /// name in this module.
    ///
    /// `mask` must be canonical: every lane exactly `0` or `!0`, the same
    /// requirement `fearless_simd::Select`'s own docs state and the shape a
    /// real `simd_gt`/`simd_eq` produces. [`crate::ops::select_u8`] is the
    /// scalar oracle and does not share that requirement — canonicalise
    /// before crossing into this function, not after.
    #[inline(always)]
    pub fn select_u8<S: Lanes>(mask: S::mask8s, a: S::u8s, b: S::u8s) -> S::u8s {
        mask.select(a, b)
    }

    /// The vector sibling of [`crate::ops::transpose4x4_i32`]: given four
    /// vectors read as matrix rows, returns four vectors read as matrix
    /// columns — `out[c].as_slice()[r] == rows[r].as_slice()[c]`.
    ///
    /// **Not a lanewise composition.** A 2-D Hadamard/Walsh transform is
    /// separable (`H·M·H`, the identical row-combination applied twice),
    /// but only the *vector* axis can be combined by a plain add/sub tree —
    /// combining lanes *within* one vector needs an actual shuffle, which
    /// this substrate has no direct 4x4 transpose for. So the second pass
    /// transposes first (moving the in-lane axis to the vector axis) and
    /// then reuses the same combination. Built the way `_MM_TRANSPOSE4_PS`
    /// is: pair adjacent rows at 32-bit granularity with `zip_low`/
    /// `zip_high`, then finish the swap at 64-bit granularity via a
    /// value-preserving `bitcast` — two interleaves, no named intrinsic.
    #[inline(always)]
    pub fn transpose4x4_i32<S: Lanes>(
        rows: [fearless_simd::i32x4<S>; 4],
    ) -> [fearless_simd::i32x4<S>; 4] {
        let [r0, r1, r2, r3] = rows;
        let t0 = r0.zip_low(r1); // [r0.0, r1.0, r0.1, r1.1]
        let t1 = r0.zip_high(r1); // [r0.2, r1.2, r0.3, r1.3]
        let t2 = r2.zip_low(r3); // [r2.0, r3.0, r2.1, r3.1]
        let t3 = r2.zip_high(r3); // [r2.2, r3.2, r2.3, r3.3]

        let t0 = t0.bitcast::<fearless_simd::i64x2<S>>();
        let t1 = t1.bitcast::<fearless_simd::i64x2<S>>();
        let t2 = t2.bitcast::<fearless_simd::i64x2<S>>();
        let t3 = t3.bitcast::<fearless_simd::i64x2<S>>();

        let c0 = t0.zip_low(t2).bitcast::<fearless_simd::i32x4<S>>(); // [r0.0, r1.0, r2.0, r3.0]
        let c1 = t0.zip_high(t2).bitcast::<fearless_simd::i32x4<S>>(); // [r0.1, r1.1, r2.1, r3.1]
        let c2 = t1.zip_low(t3).bitcast::<fearless_simd::i32x4<S>>(); // [r0.2, r1.2, r2.2, r3.2]
        let c3 = t1.zip_high(t3).bitcast::<fearless_simd::i32x4<S>>(); // [r0.3, r1.3, r2.3, r3.3]
        [c0, c1, c2, c3]
    }

    /// Clamp `i16` lanes to `0..=255` and pack two vectors into one `u8` vector.
    ///
    /// **This is `packuswb` / `sqxtun`, and the substrate does not have it.**
    /// `SimdNarrow` for `i16` narrows to `i8`, not to `u8`; only `u16` narrows to
    /// `u8`. So the signed→unsigned pack every pixel-producing kernel ends with
    /// costs a `max(0)` and a bitcast on top of the native saturating narrow.
    ///
    /// Two extra operations per output vector, on the last step of essentially
    /// every video kernel. Cheap individually, ubiquitous in aggregate.
    #[inline(always)]
    pub fn pack_u8_from_i16<S: Lanes>(lo: S::i16s, hi: S::i16s) -> S::u8s {
        let zero = <S::i16s as SimdBase<S>>::splat(lo.witness(), 0);
        let lo = lo.max(zero).bitcast::<S::u16s>();
        let hi = hi.max(zero).bitcast::<S::u16s>();
        lo.saturating_narrow(hi)
    }
}

/// Dispatched, ready-to-call masked-lane select over a whole row.
///
/// The one exception to this module's own rule that [`ops`](self) holds only
/// `S`-generic compositions for a kernel body to call: every other crate that
/// wants [`simd::select_u8`] would otherwise have to depend on `fearless_simd`
/// directly to canonicalise a mask and drive the chunk loop, which is exactly
/// the coupling the D11 boundary exists to avoid (`fearless_simd` is meant to
/// appear in exactly one manifest under `crates/`). This function is that
/// boundary: callers pass plain byte slices, `mask` need not be canonical
/// (see [`select_u8`]), and dispatch/canonicalisation/tail handling all
/// happen inside.
///
/// `mask`, `a`, `b` and `out` must be the same length; a length mismatch is
/// handled by processing only the shared prefix, since this is a library
/// entry point over caller-controlled slices and should not panic on it.
pub fn dispatched_select_u8_row(caps: crate::Caps, mask: &[u8], a: &[u8], b: &[u8], out: &mut [u8]) {
    let n = mask.len().min(a.len()).min(b.len()).min(out.len());
    let (Some(mask), Some(a), Some(b), Some(out)) =
        (mask.get(..n), a.get(..n), b.get(..n), out.get_mut(..n))
    else {
        // Every slice is at least `n` long by construction (`n` is the
        // minimum of all four lengths), so this is unreachable; the `else`
        // exists only so the shared-prefix truncation above never needs
        // indexing or an `unwrap`.
        return;
    };
    crate::dispatch_kernel!(caps, s => select_row(s, mask, a, b, out));
}

/// The level-generic body behind [`dispatched_select_u8_row`].
#[inline(always)]
#[allow(
    clippy::integer_division,
    reason = "computing the largest multiple-of-native-width prefix length; truncation is the point"
)]
fn select_row<S: crate::Lanes>(simd: S, mask: &[u8], a: &[u8], b: &[u8], out: &mut [u8]) {
    use fearless_simd::{Select, SimdBase, SimdMask};

    let n = <S::u8s as SimdBase<S>>::N;
    let full = (mask.len() / n) * n;
    let (mask_full, mask_tail) = mask.split_at(full);
    let (a_full, a_tail) = a.split_at(full);
    let (b_full, b_tail) = b.split_at(full);
    let (out_full, out_tail) = out.split_at_mut(full);

    for (((mc, ac), bc), oc) in mask_full
        .chunks_exact(n)
        .zip(a_full.chunks_exact(n))
        .zip(b_full.chunks_exact(n))
        .zip(out_full.chunks_exact_mut(n))
    {
        // Canonicalise to the substrate's documented mask shape (every lane
        // exactly `0` or `-1`) before naming the mask type at all.
        let mask_i8: Vec<i8> = mc.iter().map(|&m| if m != 0 { -1 } else { 0 }).collect();
        let m = <S::mask8s as SimdMask<S>>::from_slice(simd, &mask_i8);
        let va = <S::u8s as SimdBase<S>>::from_slice(simd, ac);
        let vb = <S::u8s as SimdBase<S>>::from_slice(simd, bc);
        m.select(va, vb).store_slice(oc);
    }

    for (((m, a), b), o) in mask_tail.iter().zip(a_tail).zip(b_tail).zip(out_tail.iter_mut()) {
        *o = select_u8(*m, *a, *b);
    }
}
