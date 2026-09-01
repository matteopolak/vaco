//! The `fearless_simd` adoption checklist (plan 12 §11), executed.
//!
//! Run with `cargo bench -p vaco-simd`. The numbers in
//! `docs/core/simd-adoption-measurements.md` come from here.
//!
//! # What is compared, and why it is a fair yardstick
//!
//! Every gap in `vaco_simd::ops::simd` has a native instruction on real hardware
//! that the substrate does not expose. We cannot call it — `kernel!` expands
//! `unsafe` into our crate and is closed to us (D12 addendum). But we can
//! measure against it, because **LLVM emits it from a plain scalar Rust loop.**
//!
//! Each pair is:
//!
//! * **native** — an ordinary Rust loop at `opt-level = 3` with the target's
//!   SIMD baseline available. LLVM auto-vectorises it and reaches `uqadd`,
//!   `urhadd`, `uabd`, `sqadd`, `addv`, `umlal`, `smull` directly.
//! * **composed** — our `ops::simd` composition through `dispatch_kernel!`.
//!
//! `composed / native` is the ratio the checklist asks for, in cycles rather
//! than counted by eye. The auto-vectorised loop is a real shipping
//! alternative, not a hypothetical, so this is not a soft baseline.
//!
//! # Method, and the trap it avoids
//!
//! **Both sides of every pair are `#[inline(never)]` functions in [`probes`],
//! and the timing loop calls exactly those symbols.** The first version of this
//! benchmark inlined each loop into its own timing closure, and reported 0.45x
//! for a composition whose disassembly is byte-identical to the baseline. Two
//! implementations that compile to the same machine code must measure the same;
//! when they do not, the harness is measuring itself. Timing named symbols also
//! means every number here has a disassembly you can go and read — see
//! [`probes`] for how.
//!
//! Min-of-N over repeated passes with a warmup, on L1-resident buffers.
//! Minimum rather than mean because microbenchmark noise is one-sided.
//!
//! # What this cannot measure
//!
//! A level this machine does not have. aarch64 has exactly one, so these
//! numbers are NEON-only; the x86 columns need a different host.

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::integer_division,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::inline_always,
    clippy::assign_op_pattern,
    clippy::wildcard_imports,
    clippy::must_use_candidate,
    clippy::manual_div_ceil,
    clippy::cast_possible_wrap,
    reason = "benchmark harness: it prints, it indexes fixed-size buffers, and it must not be defensive on the hot path"
)]

use std::hint::black_box;
use std::time::Instant;

use vaco_simd::prelude::*;
use vaco_simd::{Caps, dispatch_kernel, ops};

/// Buffer length in elements. 4 KiB stays in L1 everywhere we care about, so
/// this measures the kernel and not the memory system.
const N: usize = 4096;
/// Passes per timed sample.
const ITERS: usize = 500;
/// Timed samples; the minimum is reported.
const REPS: usize = 100;

/// The 8-tap FIR's parameters. The taps sum to 64, so an `i16` accumulator
/// cannot overflow for `u8` input (255 · 64 = 16320 < 32767).
const TAPS: usize = 8;
const FIR_SHIFT: u32 = 6;
const COEFFS: [i16; TAPS] = [-1, 4, -10, 58, 17, -5, 1, 0];

// ---------------------------------------------------------------------------
// level-generic bodies
// ---------------------------------------------------------------------------

macro_rules! composed2 {
    ($fname:ident, $vec:ident, $elem:ty, $op:path) => {
        #[inline(always)]
        fn $fname<S: Lanes>(simd: S, a: &[$elem], b: &[$elem], out: &mut [$elem]) {
            let n = <<S as Lanes>::$vec as SimdBase<S>>::N;
            for ((ac, bc), oc) in a
                .chunks_exact(n)
                .zip(b.chunks_exact(n))
                .zip(out.chunks_exact_mut(n))
            {
                let va = <<S as Lanes>::$vec as SimdBase<S>>::from_slice(simd, ac);
                let vb = <<S as Lanes>::$vec as SimdBase<S>>::from_slice(simd, bc);
                $op(va, vb).store_slice(oc);
            }
        }
    };
}

composed2!(c_sat_add_u8, u8s, u8, ops::simd::saturating_add_u8);
composed2!(c_sat_sub_u8, u8s, u8, ops::simd::saturating_sub_u8);
composed2!(c_avg_round, u8s, u8, ops::simd::rounded_avg_u8);
composed2!(c_abs_diff, u8s, u8, ops::simd::abs_diff_u8);
composed2!(c_sat_add_i16, i16s, i16, ops::simd::saturating_add_i16);

/// The same composition, four vectors per iteration.
///
/// Exists to test a specific hypothesis about the Group 1 residuals: the
/// compositions *do* select the native instruction (read the disassembly), but
/// LLVM unrolls a `for x in a.iter().zip(b)` loop 4x and does not unroll a
/// `chunks_exact` loop at all. If that is the whole story, manual 4x batching
/// should close the gap exactly.
#[inline(always)]
fn c_avg_round_x4<S: Lanes>(simd: S, a: &[u8], b: &[u8], out: &mut [u8]) {
    let n = <S::u8s as SimdBase<S>>::N;
    let w = n * 4;
    for ((ac, bc), oc) in a
        .chunks_exact(w)
        .zip(b.chunks_exact(w))
        .zip(out.chunks_exact_mut(w))
    {
        // Explicit, not a nested loop: four independent load-op-store chains
        // with no loop-carried anything, which is what gives the out-of-order
        // engine something to overlap.
        let v0 = ops::simd::rounded_avg_u8(
            <S::u8s as SimdBase<S>>::from_slice(simd, &ac[..n]),
            <S::u8s as SimdBase<S>>::from_slice(simd, &bc[..n]),
        );
        let v1 = ops::simd::rounded_avg_u8(
            <S::u8s as SimdBase<S>>::from_slice(simd, &ac[n..2 * n]),
            <S::u8s as SimdBase<S>>::from_slice(simd, &bc[n..2 * n]),
        );
        let v2 = ops::simd::rounded_avg_u8(
            <S::u8s as SimdBase<S>>::from_slice(simd, &ac[2 * n..3 * n]),
            <S::u8s as SimdBase<S>>::from_slice(simd, &bc[2 * n..3 * n]),
        );
        let v3 = ops::simd::rounded_avg_u8(
            <S::u8s as SimdBase<S>>::from_slice(simd, &ac[3 * n..]),
            <S::u8s as SimdBase<S>>::from_slice(simd, &bc[3 * n..]),
        );
        v0.store_slice(&mut oc[..n]);
        v1.store_slice(&mut oc[n..2 * n]);
        v2.store_slice(&mut oc[2 * n..3 * n]);
        v3.store_slice(&mut oc[3 * n..]);
    }
}

#[inline(always)]
fn c_abs_i16<S: Lanes>(simd: S, a: &[i16], out: &mut [i16]) {
    let n = <S::i16s as SimdBase<S>>::N;
    for (ac, oc) in a.chunks_exact(n).zip(out.chunks_exact_mut(n)) {
        let va = <S::i16s as SimdBase<S>>::from_slice(simd, ac);
        ops::simd::abs_i16(va).store_slice(oc);
    }
}

/// Four independent accumulators.
///
/// The single-accumulator form is a 1024-long chain of dependent vector adds,
/// each with ~2 cycles of latency and nothing to fill them. LLVM splits the
/// scalar loop into eight accumulators automatically; it will not do that to a
/// hand-written `chunks_exact` loop, because reassociating integer adds is
/// legal but it has no reason to think the loop is latency-bound.
///
/// This is not a substrate gap. It is the single most important kernel-authoring
/// rule that came out of these measurements.
#[inline(always)]
fn c_hsum_x4<S: Lanes>(simd: S, a: &[i32]) -> i32 {
    let n = <S::i32s as SimdBase<S>>::N;
    let zero = <S::i32s as SimdBase<S>>::splat(simd, 0);
    let mut acc = [zero; 4];
    for c in a.chunks_exact(n * 4) {
        for (slot, part) in acc.iter_mut().zip(c.chunks_exact(n)) {
            *slot = *slot + <S::i32s as SimdBase<S>>::from_slice(simd, part);
        }
    }
    let sum = (acc[0] + acc[1]) + (acc[2] + acc[3]);
    ops::simd::hsum_i32(sum)
}

#[inline(always)]
fn c_madd<S: Lanes>(simd: S, a: &[i16], b: &[i16], out: &mut [i32]) {
    let n = <S::i16s as SimdBase<S>>::N;
    for ((ac, bc), oc) in a
        .chunks_exact(n)
        .zip(b.chunks_exact(n))
        .zip(out.chunks_exact_mut(n / 2))
    {
        let va = <S::i16s as SimdBase<S>>::from_slice(simd, ac);
        let vb = <S::i16s as SimdBase<S>>::from_slice(simd, bc);
        ops::simd::madd_i16_i32::<S>(va, vb).store_slice(oc);
    }
}

#[inline(always)]
fn c_hsum_hoisted<S: Lanes>(simd: S, a: &[i32]) -> i32 {
    let n = <S::i32s as SimdBase<S>>::N;
    let mut acc = <S::i32s as SimdBase<S>>::splat(simd, 0);
    for c in a.chunks_exact(n) {
        acc = acc + <S::i32s as SimdBase<S>>::from_slice(simd, c);
    }
    ops::simd::hsum_i32(acc)
}

#[inline(always)]
fn c_hsum_per_chunk<S: Lanes>(simd: S, a: &[i32]) -> i32 {
    let n = <S::i32s as SimdBase<S>>::N;
    let mut total = 0i32;
    for c in a.chunks_exact(n) {
        let v = <S::i32s as SimdBase<S>>::from_slice(simd, c);
        total = total.wrapping_add(ops::simd::hsum_i32(v));
    }
    total
}

fn fir8_scalar(src: &[u8], dst: &mut [u8]) {
    for (i, o) in dst.iter_mut().enumerate() {
        let mut acc: i16 = 1 << (FIR_SHIFT - 1);
        for (t, &c) in COEFFS.iter().enumerate() {
            acc = acc.wrapping_add(i16::from(src[i + t]).wrapping_mul(c));
        }
        *o = ops::clip_u8(i32::from(acc >> FIR_SHIFT));
    }
}

/// Variant A: reload and re-widen the source once per tap. The obvious
/// translation, and the one a contributor writes first.
#[inline(always)]
fn fir8_reload<S: Lanes>(simd: S, src: &[u8], dst: &mut [u8]) {
    let n = <S::u8s as SimdBase<S>>::N;
    let round = <S::i16s as SimdBase<S>>::splat(simd, 1 << (FIR_SHIFT - 1));
    for (i, out) in dst.chunks_exact_mut(n).enumerate() {
        let base = i * n;
        let mut acc = (round, round);
        for (t, &c) in COEFFS.iter().enumerate() {
            let v = <S::u8s as SimdBase<S>>::from_slice(simd, &src[base + t..base + t + n]);
            acc = ops::simd::wmla_u8_i16::<S>(acc, v, c);
        }
        ops::simd::pack_u8_from_i16::<S>(acc.0 >> FIR_SHIFT, acc.1 >> FIR_SHIFT).store_slice(out);
    }
}

/// Variant C: variant A, two output vectors per iteration.
///
/// Variant A's tap loop is eight *dependent* multiply-accumulates into one pair
/// of accumulators. Two output vectors in flight gives the out-of-order engine a
/// second independent chain to interleave — the same fix that took the
/// horizontal reduction from 3.96x to 1.01x, applied to the operation plan 12
/// calls its largest performance risk.
#[inline(always)]
fn fir8_reload_x2<S: Lanes>(simd: S, src: &[u8], dst: &mut [u8]) {
    let n = <S::u8s as SimdBase<S>>::N;
    let round = <S::i16s as SimdBase<S>>::splat(simd, 1 << (FIR_SHIFT - 1));
    for (i, out) in dst.chunks_exact_mut(n * 2).enumerate() {
        let base = i * n * 2;
        let mut acc0 = (round, round);
        let mut acc1 = (round, round);
        for (t, &c) in COEFFS.iter().enumerate() {
            let v0 = <S::u8s as SimdBase<S>>::from_slice(simd, &src[base + t..base + t + n]);
            let v1 =
                <S::u8s as SimdBase<S>>::from_slice(simd, &src[base + n + t..base + n + t + n]);
            acc0 = ops::simd::wmla_u8_i16::<S>(acc0, v0, c);
            acc1 = ops::simd::wmla_u8_i16::<S>(acc1, v1, c);
        }
        let (lo, hi) = out.split_at_mut(n);
        ops::simd::pack_u8_from_i16::<S>(acc0.0 >> FIR_SHIFT, acc0.1 >> FIR_SHIFT).store_slice(lo);
        ops::simd::pack_u8_from_i16::<S>(acc1.0 >> FIR_SHIFT, acc1.1 >> FIR_SHIFT).store_slice(hi);
    }
}

#[inline(always)]
fn tap<const T: usize, S: Lanes>(
    acc: (S::i16s, S::i16s),
    a0: S::i16s,
    a1: S::i16s,
    b0: S::i16s,
    c: i16,
) -> (S::i16s, S::i16s) {
    (
        ops::simd::wmla_i16(acc.0, a0.slide::<T>(a1), c),
        ops::simd::wmla_i16(acc.1, a1.slide::<T>(b0), c),
    )
}

/// Variant B: hoist the widen out of the tap loop and reach neighbouring taps
/// with `slide`. This is the structure plan 11 §5.6 prescribes.
#[inline(always)]
fn fir8_slide<S: Lanes>(simd: S, src: &[u8], dst: &mut [u8]) {
    let n = <S::u8s as SimdBase<S>>::N;
    let round = <S::i16s as SimdBase<S>>::splat(simd, 1 << (FIR_SHIFT - 1));
    for (i, out) in dst.chunks_exact_mut(n).enumerate() {
        let base = i * n;
        let v0 = <S::u8s as SimdBase<S>>::from_slice(simd, &src[base..base + n]);
        let v1 = <S::u8s as SimdBase<S>>::from_slice(simd, &src[base + n..base + 2 * n]);
        let (a0, a1) = ops::simd::widen_u8_i16::<S>(v0);
        let (b0, _b1) = ops::simd::widen_u8_i16::<S>(v1);

        let mut acc = (round, round);
        acc = tap::<0, S>(acc, a0, a1, b0, COEFFS[0]);
        acc = tap::<1, S>(acc, a0, a1, b0, COEFFS[1]);
        acc = tap::<2, S>(acc, a0, a1, b0, COEFFS[2]);
        acc = tap::<3, S>(acc, a0, a1, b0, COEFFS[3]);
        acc = tap::<4, S>(acc, a0, a1, b0, COEFFS[4]);
        acc = tap::<5, S>(acc, a0, a1, b0, COEFFS[5]);
        acc = tap::<6, S>(acc, a0, a1, b0, COEFFS[6]);
        acc = tap::<7, S>(acc, a0, a1, b0, COEFFS[7]);

        ops::simd::pack_u8_from_i16::<S>(acc.0 >> FIR_SHIFT, acc.1 >> FIR_SHIFT).store_slice(out);
    }
}

#[inline(always)]
fn trivial<S: Lanes>(_simd: S, x: u32) -> u32 {
    x.wrapping_mul(2_654_435_761)
}

// ---------------------------------------------------------------------------
// masked-lane select (#127's spike): is there a composition gap at all, and
// if the native op is free, does a hand-rolled bitwise blend cost anything?
// ---------------------------------------------------------------------------

/// The substrate's own select, reached through the dedicated `mask8x16` type
/// rather than composed from bitwise ops. `mask_i8` must already be
/// canonical (every lane `0` or `-1`) — that is `mask8x16::from_slice`'s own
/// documented input shape, matching what a real `simd_gt`/`simd_eq` produces.
#[inline(always)]
fn c_select_native<S: Lanes>(
    simd: S,
    mask_i8: &[i8],
    a: &[u8],
    b: &[u8],
    out: &mut [u8],
) {
    for (((mc, ac), bc), oc) in mask_i8
        .as_chunks::<16>()
        .0
        .iter()
        .zip(a.as_chunks::<16>().0.iter())
        .zip(b.as_chunks::<16>().0.iter())
        .zip(out.as_chunks_mut::<16>().0.iter_mut())
    {
        let m = fearless_simd::mask8x16::<S>::from_slice(simd, mc);
        let va = fearless_simd::u8x16::<S>::from_slice(simd, ac);
        let vb = fearless_simd::u8x16::<S>::from_slice(simd, bc);
        m.select(va, vb).store_slice(oc);
    }
}

/// The same result composed from `and`/`andnot`/`or` on the canonical
/// `0x00`/`0xFF` byte mask directly, with no dedicated mask type at all.
/// `mask_u8` carries the same canonical pattern as `mask_i8` above, just
/// reinterpreted as unsigned bytes rather than converted to the substrate's
/// opaque mask representation.
#[inline(always)]
fn c_select_bitwise<S: Lanes>(simd: S, mask_u8: &[u8], a: &[u8], b: &[u8], out: &mut [u8]) {
    for (((mc, ac), bc), oc) in mask_u8
        .as_chunks::<16>()
        .0
        .iter()
        .zip(a.as_chunks::<16>().0.iter())
        .zip(b.as_chunks::<16>().0.iter())
        .zip(out.as_chunks_mut::<16>().0.iter_mut())
    {
        let vm = fearless_simd::u8x16::<S>::from_slice(simd, mc);
        let va = fearless_simd::u8x16::<S>::from_slice(simd, ac);
        let vb = fearless_simd::u8x16::<S>::from_slice(simd, bc);
        ((vm & va) | (!vm & vb)).store_slice(oc);
    }
}

/// The `i16` sibling of [`c_select_native`] — `ops::simd::select_i16`'s own
/// shape, closing `vaco-codec-dsp-deblock`'s recorded blocker at the width
/// its per-sample decisions actually need. `mask_i16` must already be
/// canonical, same requirement as `mask_i8` above.
#[inline(always)]
fn c_select_native_i16<S: Lanes>(simd: S, mask_i16: &[i16], a: &[i16], b: &[i16], out: &mut [i16]) {
    for (((mc, ac), bc), oc) in mask_i16
        .as_chunks::<8>()
        .0
        .iter()
        .zip(a.as_chunks::<8>().0.iter())
        .zip(b.as_chunks::<8>().0.iter())
        .zip(out.as_chunks_mut::<8>().0.iter_mut())
    {
        let m = fearless_simd::mask16x8::<S>::from_slice(simd, mc);
        let va = fearless_simd::i16x8::<S>::from_slice(simd, ac);
        let vb = fearless_simd::i16x8::<S>::from_slice(simd, bc);
        m.select(va, vb).store_slice(oc);
    }
}

/// The `i32` sibling of [`c_select_native`].
#[inline(always)]
fn c_select_native_i32<S: Lanes>(simd: S, mask_i32: &[i32], a: &[i32], b: &[i32], out: &mut [i32]) {
    for (((mc, ac), bc), oc) in mask_i32
        .as_chunks::<4>()
        .0
        .iter()
        .zip(a.as_chunks::<4>().0.iter())
        .zip(b.as_chunks::<4>().0.iter())
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let m = fearless_simd::mask32x4::<S>::from_slice(simd, mc);
        let va = fearless_simd::i32x4::<S>::from_slice(simd, ac);
        let vb = fearless_simd::i32x4::<S>::from_slice(simd, bc);
        m.select(va, vb).store_slice(oc);
    }
}

// ---------------------------------------------------------------------------
// probes — the only implementations that are timed, and the only ones to
// disassemble. `#[inline(never)]` so each keeps a symbol after fat LTO.
// ---------------------------------------------------------------------------

/// The A/B pairs, as named symbols.
///
/// Read them back with, from the target directory's `release/deps`:
///
/// ```text
/// SYM=$(nm adoption-* | grep 'probe_composed_fir8_reload$' | awk '{print $3}')
/// objdump -d --disassemble-symbols="$SYM" adoption-*
/// ```
///
/// Two checklist items are settled that way:
///
/// * **items 1 and 2 — is the baseline honest?** A "native" row only means
///   something if LLVM really did reach the instruction the substrate lacks.
///   Look for `uqadd`, `uabd`, `urhadd`, `sqadd`, `umlal`, `smull` in
///   `probe_scalar_*`.
/// * **item 5 — does the dispatched body inline?** `probe_composed_*` must
///   contain **no `bl`** other than cold `core::panicking` edges.
pub mod probes {
    use super::*;

    macro_rules! pair {
        ($scalar:ident, $composed:ident, $elem:ty, $body:path, $op:expr) => {
            #[inline(never)]
            pub fn $scalar(a: &[$elem], b: &[$elem], out: &mut [$elem]) {
                for ((x, y), o) in a.iter().zip(b).zip(out.iter_mut()) {
                    *o = $op(*x, *y);
                }
            }

            #[inline(never)]
            pub fn $composed(caps: Caps, a: &[$elem], b: &[$elem], out: &mut [$elem]) {
                dispatch_kernel!(caps, s => $body(s, a, b, out));
            }
        };
    }

    pair!(
        scalar_sat_add_u8,
        composed_sat_add_u8,
        u8,
        c_sat_add_u8,
        u8::saturating_add
    );
    pair!(
        scalar_sat_sub_u8,
        composed_sat_sub_u8,
        u8,
        c_sat_sub_u8,
        u8::saturating_sub
    );
    pair!(
        scalar_abs_diff_u8,
        composed_abs_diff_u8,
        u8,
        c_abs_diff,
        u8::abs_diff
    );
    pair!(
        scalar_sat_add_i16,
        composed_sat_add_i16,
        i16,
        c_sat_add_i16,
        i16::saturating_add
    );
    pair!(
        scalar_avg_round_u8,
        composed_avg_round_u8,
        u8,
        c_avg_round,
        |x, y| { ((u16::from(x) + u16::from(y) + 1) / 2) as u8 }
    );

    #[inline(never)]
    pub fn scalar_abs_i16(a: &[i16], out: &mut [i16]) {
        for (x, o) in a.iter().zip(out.iter_mut()) {
            *o = x.wrapping_abs();
        }
    }

    #[inline(never)]
    pub fn composed_abs_i16(caps: Caps, a: &[i16], out: &mut [i16]) {
        dispatch_kernel!(caps, s => c_abs_i16(s, a, out));
    }

    #[inline(never)]
    pub fn composed_avg_round_u8_x4(caps: Caps, a: &[u8], b: &[u8], out: &mut [u8]) {
        dispatch_kernel!(caps, s => c_avg_round_x4(s, a, b, out));
    }

    #[inline(never)]
    pub fn composed_hsum_x4(caps: Caps, a: &[i32]) -> i32 {
        dispatch_kernel!(caps, s => c_hsum_x4(s, a))
    }

    #[inline(never)]
    pub fn scalar_madd(a: &[i16], b: &[i16], out: &mut [i32]) {
        for ((&[x0, x1], &[y0, y1]), o) in a
            .as_chunks::<2>()
            .0
            .iter()
            .zip(b.as_chunks::<2>().0.iter())
            .zip(out.iter_mut())
        {
            *o = ops::madd_i16_i32(x0, y0, x1, y1);
        }
    }

    #[inline(never)]
    pub fn composed_madd(caps: Caps, a: &[i16], b: &[i16], out: &mut [i32]) {
        dispatch_kernel!(caps, s => c_madd(s, a, b, out));
    }

    #[inline(never)]
    pub fn scalar_hsum(a: &[i32]) -> i32 {
        a.iter().copied().fold(0i32, i32::wrapping_add)
    }

    #[inline(never)]
    pub fn composed_hsum_hoisted(caps: Caps, a: &[i32]) -> i32 {
        dispatch_kernel!(caps, s => c_hsum_hoisted(s, a))
    }

    #[inline(never)]
    pub fn composed_hsum_per_chunk(caps: Caps, a: &[i32]) -> i32 {
        dispatch_kernel!(caps, s => c_hsum_per_chunk(s, a))
    }

    #[inline(never)]
    pub fn scalar_fir8(src: &[u8], dst: &mut [u8]) {
        fir8_scalar(src, dst);
    }

    #[inline(never)]
    pub fn composed_fir8_reload(caps: Caps, src: &[u8], dst: &mut [u8]) {
        dispatch_kernel!(caps, s => fir8_reload(s, src, dst));
    }

    #[inline(never)]
    pub fn composed_fir8_reload_x2(caps: Caps, src: &[u8], dst: &mut [u8]) {
        dispatch_kernel!(caps, s => fir8_reload_x2(s, src, dst));
    }

    #[inline(never)]
    pub fn composed_fir8_slide(caps: Caps, src: &[u8], dst: &mut [u8]) {
        dispatch_kernel!(caps, s => fir8_slide(s, src, dst));
    }

    #[inline(never)]
    pub fn scalar_select(mask: &[u8], a: &[u8], b: &[u8], out: &mut [u8]) {
        for (((m, x), y), o) in mask.iter().zip(a).zip(b).zip(out.iter_mut()) {
            *o = if *m != 0 { *x } else { *y };
        }
    }

    #[inline(never)]
    pub fn composed_select_native(
        caps: Caps,
        mask_i8: &[i8],
        a: &[u8],
        b: &[u8],
        out: &mut [u8],
    ) {
        dispatch_kernel!(caps, s => c_select_native(s, mask_i8, a, b, out));
    }

    #[inline(never)]
    pub fn composed_select_bitwise(caps: Caps, mask_u8: &[u8], a: &[u8], b: &[u8], out: &mut [u8]) {
        dispatch_kernel!(caps, s => c_select_bitwise(s, mask_u8, a, b, out));
    }

    #[inline(never)]
    pub fn scalar_select_i16(mask: &[i16], a: &[i16], b: &[i16], out: &mut [i16]) {
        for (((m, x), y), o) in mask.iter().zip(a).zip(b).zip(out.iter_mut()) {
            *o = if *m != 0 { *x } else { *y };
        }
    }

    #[inline(never)]
    pub fn composed_select_native_i16(caps: Caps, mask: &[i16], a: &[i16], b: &[i16], out: &mut [i16]) {
        dispatch_kernel!(caps, s => c_select_native_i16(s, mask, a, b, out));
    }

    #[inline(never)]
    pub fn scalar_select_i32(mask: &[i32], a: &[i32], b: &[i32], out: &mut [i32]) {
        for (((m, x), y), o) in mask.iter().zip(a).zip(b).zip(out.iter_mut()) {
            *o = if *m != 0 { *x } else { *y };
        }
    }

    #[inline(never)]
    pub fn composed_select_native_i32(caps: Caps, mask: &[i32], a: &[i32], b: &[i32], out: &mut [i32]) {
        dispatch_kernel!(caps, s => c_select_native_i32(s, mask, a, b, out));
    }

    #[inline(never)]
    pub fn scalar_yuv(y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
        vaco_simd::example::yuv420p_to_rgb24_row_scalar(y, u, v, rgb);
    }

    #[inline(never)]
    pub fn composed_yuv(y: &[u8], u: &[u8], v: &[u8], rgb: &mut [u8]) {
        vaco_simd::example::yuv420p_to_rgb24_row_dispatched(y, u, v, rgb);
    }
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// Spin for ~300 ms before measuring anything.
///
/// macOS starts a process's main thread on an efficiency core and only promotes
/// it once it looks busy. Without this, the first few measurements in a run come
/// back 2-3x slow and the whole table is unreproducible. This was not a
/// hypothesis — it is what a 45 ns row turning into 132 ns between two runs of
/// an unchanged binary actually was.
fn promote_to_performance_core() {
    let t = Instant::now();
    let mut x = 1u64;
    while t.elapsed().as_millis() < 300 {
        for _ in 0..10_000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        }
        black_box(x);
    }
}

/// Time two implementations **interleaved**, round by round, and return the
/// minimum each achieved.
///
/// Measuring A to completion and then B gives each a different slice of the
/// machine's mood: a different core, a different clock, a different neighbour.
/// Alternating means both see the same conditions, so the *ratio* — which is
/// the only number this benchmark is really reporting — survives even when the
/// absolute times drift.
fn time_pair(mut a: impl FnMut(), mut b: impl FnMut()) -> (f64, f64) {
    for _ in 0..50 {
        a();
        b();
    }
    let (mut best_a, mut best_b) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..REPS {
        let t = Instant::now();
        for _ in 0..ITERS {
            a();
        }
        let na = t.elapsed().as_nanos() as f64 / ITERS as f64;

        let t = Instant::now();
        for _ in 0..ITERS {
            b();
        }
        let nb = t.elapsed().as_nanos() as f64 / ITERS as f64;

        if na < best_a {
            best_a = na;
        }
        if nb < best_b {
            best_b = nb;
        }
    }
    (best_a, best_b)
}

struct Table {
    title: String,
    rows: Vec<(String, f64, f64, String)>,
}

impl Table {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    fn row(&mut self, name: &str, native: f64, composed: f64, note: &str) {
        self.rows
            .push((name.to_owned(), native, composed, note.to_owned()));
    }

    fn print(&self) {
        println!("\n### {}\n", self.title);
        println!("| operation | native (ns) | composed (ns) | composed/native | note |");
        println!("|---|---:|---:|---:|---|");
        for (name, native, composed, note) in &self.rows {
            println!(
                "| `{name}` | {native:.1} | {composed:.1} | **{:.2}x** | {note} |",
                composed / native
            );
        }
    }
}

// ---------------------------------------------------------------------------

fn group_gaps(caps: Caps) {
    let a8: Vec<u8> = (0..N).map(|i| ((i * 37) & 0xFF) as u8).collect();
    let b8: Vec<u8> = (0..N).map(|i| ((i * 91) & 0xFF) as u8).collect();
    let a16: Vec<i16> = (0..N).map(|i| (i as i32 * 613 - 20000) as i16).collect();
    let b16: Vec<i16> = (0..N).map(|i| (i as i32 * 271 - 15000) as i16).collect();
    let mut x = vec![0u8; N];
    let mut y = vec![0u8; N];
    let mut x16 = vec![0i16; N];
    let mut y16 = vec![0i16; N];

    let mut t = Table::new("Group 1 — gap compositions vs the instruction LLVM reaches for");

    macro_rules! pair_u8 {
        ($name:literal, $s:path, $c:path, $note:literal) => {{
            $s(&a8, &b8, &mut x);
            $c(caps, &a8, &b8, &mut y);
            assert_eq!(x, y, concat!($name, ": composition diverged"));
            let (n, c) = time_pair(
                || $s(black_box(&a8), black_box(&b8), black_box(&mut x)),
                || $c(caps, black_box(&a8), black_box(&b8), black_box(&mut y)),
            );
            t.row($name, n, c, $note);
        }};
    }

    pair_u8!(
        "saturating_add_u8",
        probes::scalar_sat_add_u8,
        probes::composed_sat_add_u8,
        "`min(!b) + b`, 3 ops vs 1"
    );
    pair_u8!(
        "saturating_sub_u8",
        probes::scalar_sat_sub_u8,
        probes::composed_sat_sub_u8,
        "`max(b) - b`, 2 ops vs 1"
    );
    pair_u8!(
        "rounded_avg_u8",
        probes::scalar_avg_round_u8,
        probes::composed_avg_round_u8,
        "`(a\\|b) - ((a^b)>>1)`, 4 ops vs 1"
    );
    pair_u8!(
        "rounded_avg_u8 (batched 4x)",
        probes::scalar_avg_round_u8,
        probes::composed_avg_round_u8_x4,
        "same composition, four vectors per iteration"
    );
    pair_u8!(
        "abs_diff_u8",
        probes::scalar_abs_diff_u8,
        probes::composed_abs_diff_u8,
        "`max - min`, 3 ops vs 1"
    );

    probes::scalar_sat_add_i16(&a16, &b16, &mut x16);
    probes::composed_sat_add_i16(caps, &a16, &b16, &mut y16);
    assert_eq!(x16, y16, "saturating_add_i16: composition diverged");
    let (n, c) = time_pair(
        || probes::scalar_sat_add_i16(black_box(&a16), black_box(&b16), black_box(&mut x16)),
        || {
            probes::composed_sat_add_i16(
                caps,
                black_box(&a16),
                black_box(&b16),
                black_box(&mut y16),
            );
        },
    );
    t.row(
        "saturating_add_i16",
        n,
        c,
        "widen/add/`saturating_narrow`, ~5 ops vs 1",
    );

    probes::scalar_abs_i16(&a16, &mut x16);
    probes::composed_abs_i16(caps, &a16, &mut y16);
    assert_eq!(x16, y16, "abs_i16: composition diverged");
    let (n, c) = time_pair(
        || probes::scalar_abs_i16(black_box(&a16), black_box(&mut x16)),
        || probes::composed_abs_i16(caps, black_box(&a16), black_box(&mut y16)),
    );
    t.row("abs_i16", n, c, "`max(x, -x)`, 2 ops vs 1");

    t.print();
}

fn group_reduction(caps: Caps) {
    let a: Vec<i32> = (0..N).map(|i| i as i32 * 7 - 9000).collect();
    assert_eq!(
        probes::scalar_hsum(&a),
        probes::composed_hsum_hoisted(caps, &a)
    );
    assert_eq!(
        probes::scalar_hsum(&a),
        probes::composed_hsum_per_chunk(caps, &a)
    );

    assert_eq!(probes::scalar_hsum(&a), probes::composed_hsum_x4(caps, &a));

    let mut t = Table::new("Group 2 — horizontal reduction: where the accumulator lives");
    let (n, hoisted) = time_pair(
        || {
            black_box(probes::scalar_hsum(black_box(&a)));
        },
        || {
            black_box(probes::composed_hsum_hoisted(caps, black_box(&a)));
        },
    );
    let (_, per_chunk) = time_pair(
        || {
            black_box(probes::scalar_hsum(black_box(&a)));
        },
        || {
            black_box(probes::composed_hsum_per_chunk(caps, black_box(&a)));
        },
    );
    t.row(
        "hsum_i32 (vector accumulator hoisted)",
        n,
        hoisted,
        "one reduction per invocation — the correct shape",
    );
    t.row(
        "hsum_i32 (reduced once per chunk)",
        n,
        per_chunk,
        "the mistake the op's own docs warn about",
    );
    let (_, x4) = time_pair(
        || {
            black_box(probes::scalar_hsum(black_box(&a)));
        },
        || {
            black_box(probes::composed_hsum_x4(caps, black_box(&a)));
        },
    );
    t.row(
        "hsum_i32 (four hoisted accumulators)",
        n,
        x4,
        "**the rule**: one accumulator is a latency chain, four is not",
    );
    t.print();
}

fn group_madd(caps: Caps) {
    let a: Vec<i16> = (0..N).map(|i| (i as i32 * 613 - 20000) as i16).collect();
    let b: Vec<i16> = (0..N).map(|i| (i as i32 * 271 - 15000) as i16).collect();
    let mut x = vec![0i32; N / 2];
    let mut y = vec![0i32; N / 2];

    probes::scalar_madd(&a, &b, &mut x);
    probes::composed_madd(caps, &a, &b, &mut y);
    assert_eq!(x, y, "madd_i16_i32: composition diverged");

    let mut t = Table::new("Group 3 — the `pmaddwd` shape (pairwise dot product)");
    let (n, c) = time_pair(
        || probes::scalar_madd(black_box(&a), black_box(&b), black_box(&mut x)),
        || probes::composed_madd(caps, black_box(&a), black_box(&b), black_box(&mut y)),
    );
    t.row(
        "madd_i16_i32",
        n,
        c,
        "no composition exists: widen x2, mul x2, unzip x2, add",
    );
    t.print();
}

fn group_fir(caps: Caps) {
    // Generous headroom: variant B reads two vectors ahead.
    let src: Vec<u8> = (0..N + 256).map(|i| ((i * 53) & 0xFF) as u8).collect();
    let mut dst = vec![0u8; N];
    let mut check = vec![0u8; N];

    probes::scalar_fir8(&src, &mut check);
    probes::composed_fir8_reload(caps, &src, &mut dst);
    assert_eq!(dst, check, "fir8_reload diverged from the scalar reference");
    dst.fill(0);
    probes::composed_fir8_reload_x2(caps, &src, &mut dst);
    assert_eq!(
        dst, check,
        "fir8_reload_x2 diverged from the scalar reference"
    );
    dst.fill(0);
    probes::composed_fir8_slide(caps, &src, &mut dst);
    assert_eq!(dst, check, "fir8_slide diverged from the scalar reference");

    let mut t = Table::new(format!(
        "Group 4 — 8-tap u8 horizontal FIR, {N} output samples (the `pmaddubsw` shape)"
    ));
    let mut dst2 = vec![0u8; N];
    let (n, reload) = time_pair(
        || probes::scalar_fir8(black_box(&src), black_box(&mut dst)),
        || probes::composed_fir8_reload(caps, black_box(&src), black_box(&mut dst2)),
    );
    let (_, reload_x2) = time_pair(
        || probes::scalar_fir8(black_box(&src), black_box(&mut dst)),
        || probes::composed_fir8_reload_x2(caps, black_box(&src), black_box(&mut dst2)),
    );
    let (_, slide) = time_pair(
        || probes::scalar_fir8(black_box(&src), black_box(&mut dst)),
        || probes::composed_fir8_slide(caps, black_box(&src), black_box(&mut dst2)),
    );

    t.row(
        "fir8 — reload + widen per tap",
        n,
        reload,
        "the obvious translation",
    );
    t.row(
        "fir8 — reload, batched 2 output vectors",
        n,
        reload_x2,
        "**the same batching rule as Group 2**",
    );
    t.row(
        "fir8 — widen hoisted, `slide` per tap",
        n,
        slide,
        "plan 11 §5.6's prescribed structure",
    );
    t.print();
}

/// **#127's spike.** Deblocking's own text names "masked-lane select" as the
/// technique to gate the design on. The substrate already provides it
/// natively (`mask8x16::select`, backed by `pblendvb`/`bsl`/`vpternlog` per
/// target) — so the real question is not "how do we compose select", it is
/// "does using the dedicated mask type cost anything over a plain bitwise
/// blend of canonical `0x00`/`0xFF` bytes computed some other way". Both
/// sides here start from the *same* canonical pattern, so this isolates
/// exactly that choice.
fn group_select(caps: Caps) {
    // A non-trivial, non-degenerate split: not all-true or all-false (which
    // some backends could special-case) and not a simple period-2 alternation
    // (which is exactly [`vaco_simd::testing::edge_patterns`]'s own pattern
    // and would not show whether a *mixed* mask changes anything).
    let mask_u8: Vec<u8> = (0..N)
        .map(|i| if (i * 2_654_435_761_u32 as usize) % 5 < 2 { 0xFF } else { 0x00 })
        .collect();
    let mask_i8: Vec<i8> = mask_u8.iter().map(|&m| if m != 0 { -1 } else { 0 }).collect();
    let a: Vec<u8> = (0..N).map(|i| ((i * 37) & 0xFF) as u8).collect();
    let b: Vec<u8> = (0..N).map(|i| ((i * 91) & 0xFF) as u8).collect();
    let mut x = vec![0u8; N];
    let mut y = vec![0u8; N];

    probes::scalar_select(&mask_u8, &a, &b, &mut x);
    probes::composed_select_native(caps, &mask_i8, &a, &b, &mut y);
    assert_eq!(x, y, "select (native mask type): composition diverged");
    y.fill(0);
    probes::composed_select_bitwise(caps, &mask_u8, &a, &b, &mut y);
    assert_eq!(x, y, "select (bitwise blend): composition diverged");

    let mut t = Table::new("Group 7 — masked-lane select (#127's spike)");
    let (n, native) = time_pair(
        || probes::scalar_select(black_box(&mask_u8), black_box(&a), black_box(&b), black_box(&mut x)),
        || {
            probes::composed_select_native(
                caps,
                black_box(&mask_i8),
                black_box(&a),
                black_box(&b),
                black_box(&mut y),
            );
        },
    );
    let (_, bitwise) = time_pair(
        || probes::scalar_select(black_box(&mask_u8), black_box(&a), black_box(&b), black_box(&mut x)),
        || {
            probes::composed_select_bitwise(
                caps,
                black_box(&mask_u8),
                black_box(&a),
                black_box(&b),
                black_box(&mut y),
            );
        },
    );
    t.row(
        "select (mask8x16::select, native)",
        n,
        native,
        "the dedicated mask type; one instruction per vector on every tier",
    );
    t.row(
        "select (and/andnot/or on canonical bytes)",
        n,
        bitwise,
        "`(m&a)|(!m&b)`, 3 ops vs 1 — the fallback when a mask never existed as a first-class value",
    );

    // `i16`/`i32`: the widths #619's deblocking blocker actually needs —
    // widened sample differences and their alpha/beta/tC0 comparisons never
    // fit in a `u8` lane once the subtraction can go negative.
    let mask16: Vec<i16> = (0..N)
        .map(|i| if (i * 2_654_435_761_u32 as usize) % 5 < 2 { -1 } else { 0 })
        .collect();
    let a16: Vec<i16> = (0..N).map(|i| (i as i32 * 37 - 5000) as i16).collect();
    let b16: Vec<i16> = (0..N).map(|i| (i as i32 * 91 - 3000) as i16).collect();
    let mut x16 = vec![0i16; N];
    let mut y16 = vec![0i16; N];
    probes::scalar_select_i16(&mask16, &a16, &b16, &mut x16);
    probes::composed_select_native_i16(caps, &mask16, &a16, &b16, &mut y16);
    assert_eq!(x16, y16, "select_i16: composition diverged");
    let (n16, native16) = time_pair(
        || probes::scalar_select_i16(black_box(&mask16), black_box(&a16), black_box(&b16), black_box(&mut x16)),
        || {
            probes::composed_select_native_i16(
                caps,
                black_box(&mask16),
                black_box(&a16),
                black_box(&b16),
                black_box(&mut y16),
            );
        },
    );
    t.row(
        "select_i16 (mask16x8::select, native)",
        n16,
        native16,
        "same shape as select_u8, at the width deblocking's own decisions actually compare",
    );

    let mask32: Vec<i32> = (0..N)
        .map(|i| if (i * 2_654_435_761_u32 as usize) % 5 < 2 { -1 } else { 0 })
        .collect();
    let a32: Vec<i32> = (0..N).map(|i| i as i32 * 37 - 5000).collect();
    let b32: Vec<i32> = (0..N).map(|i| i as i32 * 91 - 3000).collect();
    let mut x32 = vec![0i32; N];
    let mut y32 = vec![0i32; N];
    probes::scalar_select_i32(&mask32, &a32, &b32, &mut x32);
    probes::composed_select_native_i32(caps, &mask32, &a32, &b32, &mut y32);
    assert_eq!(x32, y32, "select_i32: composition diverged");
    let (n32, native32) = time_pair(
        || probes::scalar_select_i32(black_box(&mask32), black_box(&a32), black_box(&b32), black_box(&mut x32)),
        || {
            probes::composed_select_native_i32(
                caps,
                black_box(&mask32),
                black_box(&a32),
                black_box(&b32),
                black_box(&mut y32),
            );
        },
    );
    t.row(
        "select_i32 (mask32x4::select, native)",
        n32,
        native32,
        "same shape, for a kernel that accumulates wider than 8-bit depth needs",
    );

    t.print();
}

fn group_dispatch(caps: Caps) {
    println!("\n### Group 5 — dispatch overhead\n");
    println!(
        "| calls per pass | via `dispatch_kernel!` | via plain `fn` pointer | delta per call |"
    );
    println!("|---:|---:|---:|---:|");
    let fp: fn(u32) -> u32 = |x| x.wrapping_mul(2_654_435_761);
    for calls in [1usize, 10, 100] {
        let (dispatched, direct) = time_pair(
            || {
                let mut x = black_box(1u32);
                for _ in 0..calls {
                    x = dispatch_kernel!(caps, s => trivial(s, black_box(x)));
                }
                black_box(x);
            },
            || {
                let mut x = black_box(1u32);
                for _ in 0..calls {
                    x = fp(black_box(x));
                }
                black_box(x);
            },
        );
        println!(
            "| {calls} | {dispatched:.2} ns | {direct:.2} ns | {:+.3} ns |",
            (dispatched - direct) / calls as f64
        );
    }
}

fn group_example() {
    let width = 1920usize;
    let y: Vec<u8> = (0..width).map(|i| ((i * 31) & 0xFF) as u8).collect();
    let u: Vec<u8> = (0..width / 2).map(|i| ((i * 17) & 0xFF) as u8).collect();
    let v: Vec<u8> = (0..width / 2).map(|i| ((i * 53) & 0xFF) as u8).collect();
    let mut a = vec![0u8; width * 3];
    let mut b = vec![0u8; width * 3];

    probes::scalar_yuv(&y, &u, &v, &mut a);
    probes::composed_yuv(&y, &u, &v, &mut b);
    assert_eq!(a, b, "yuv420p_to_rgb24_row: kernel diverged");

    let mut t = Table::new("Group 6 — the worked example: yuv420p -> rgb24, one 1920px row");
    let (n, c) = time_pair(
        || {
            probes::scalar_yuv(
                black_box(&y),
                black_box(&u),
                black_box(&v),
                black_box(&mut a),
            );
        },
        || {
            probes::composed_yuv(
                black_box(&y),
                black_box(&u),
                black_box(&v),
                black_box(&mut b),
            );
        },
    );
    t.row(
        "yuv420p_to_rgb24_row",
        n,
        c,
        "here a ratio BELOW 1.0 is the win",
    );
    t.print();
}

/// The native `u8` lane count for a token; the token bound by
/// `dispatch_kernel!` is an anonymous `impl Simd`, so its associated types need
/// a generic function to name them.
#[inline(always)]
fn u8_lanes<S: Lanes>(_simd: S) -> usize {
    <S::u8s as SimdBase<S>>::N
}

fn main() {
    promote_to_performance_core();
    let caps = Caps::detect();
    println!("# fearless_simd adoption measurements");
    println!(
        "\nhost: {} / {} · tier: {} · native u8 lanes: {}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        caps.tier(),
        dispatch_kernel!(caps, s => u8_lanes(s)),
    );
    println!(
        "\n`native` = a plain Rust loop auto-vectorised by LLVM, reaching the instruction \
         the substrate does not expose. `composed` = our `ops::simd` composition through \
         `dispatch_kernel!`. Both sides are `#[inline(never)]` symbols in `probes`, so every \
         number here has a disassembly. Min-of-{REPS} over {ITERS}-pass samples, \
         {N}-element buffers."
    );

    group_gaps(caps);
    group_reduction(caps);
    group_madd(caps);
    group_fir(caps);
    group_select(caps);
    group_dispatch(caps);
    group_example();
}
