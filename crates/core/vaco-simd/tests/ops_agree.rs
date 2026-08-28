//! Every composed operation in `vaco_simd::ops::simd`, proved lane-for-lane
//! equal to its scalar reference in `vaco_simd::ops`.
//!
//! Two harnesses, deliberately:
//!
//! * **proptest** — random input, wide coverage, shrinks a failure to a minimal
//!   case.
//! * **the edge corpus** — deterministic, no seed, hits saturation boundaries,
//!   `0`/`MAX`, walking bits and every loop-tail length from 0 to 193.
//!
//! Integer kernels are compared with `assert_eq`, never with a tolerance. A
//! backend that rounds or saturates differently at one level is a correctness
//! bug we want to find here, not after 150 kernels exist.

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    clippy::integer_division,
    clippy::inline_always,
    clippy::manual_midpoint,
    clippy::manual_div_ceil,
    reason = "test code: a panic is the reporting mechanism, and every index is bounded by a length assertion above it"
)]

use proptest::prelude::*;
use vaco_simd::prelude::*;
use vaco_simd::testing;

/// Build a whole-slice driver for a binary lane-wise op: chunk at the native
/// width, apply the vector composition, and hand the tail to the scalar
/// reference. This is exactly the shape plan 11 §5.4 rule 1 requires of a real
/// kernel, so the harness exercises the tail path too.
macro_rules! elementwise2 {
    ($name:ident, $vec:ident, $elem:ty, $simd_op:path, $scalar_op:path) => {
        fn $name(a: &[$elem], b: &[$elem]) -> Vec<$elem> {
            #[inline(always)]
            fn body<S: Lanes>(simd: S, a: &[$elem], b: &[$elem], out: &mut [$elem]) {
                let n = <<S as Lanes>::$vec as SimdBase<S>>::N;
                let mut ai = a.chunks_exact(n);
                let mut bi = b.chunks_exact(n);
                let mut oi = out.chunks_exact_mut(n);
                for ((ac, bc), oc) in (&mut ai).zip(&mut bi).zip(&mut oi) {
                    let va = <<S as Lanes>::$vec as SimdBase<S>>::from_slice(simd, ac);
                    let vb = <<S as Lanes>::$vec as SimdBase<S>>::from_slice(simd, bc);
                    $simd_op(va, vb).store_slice(oc);
                }
                for ((x, y), o) in ai
                    .remainder()
                    .iter()
                    .zip(bi.remainder())
                    .zip(oi.into_remainder())
                {
                    *o = $scalar_op(*x, *y);
                }
            }

            assert_eq!(a.len(), b.len());
            let mut out = vec![<$elem>::default(); a.len()];
            let caps = Caps::detect();
            vaco_simd::dispatch_kernel!(caps, simd => body(simd, a, b, &mut out));
            out
        }
    };
}

macro_rules! elementwise1 {
    ($name:ident, $vec:ident, $elem:ty, $simd_op:path, $scalar_op:path) => {
        fn $name(a: &[$elem]) -> Vec<$elem> {
            #[inline(always)]
            fn body<S: Lanes>(simd: S, a: &[$elem], out: &mut [$elem]) {
                let n = <<S as Lanes>::$vec as SimdBase<S>>::N;
                let mut ai = a.chunks_exact(n);
                let mut oi = out.chunks_exact_mut(n);
                for (ac, oc) in (&mut ai).zip(&mut oi) {
                    let va = <<S as Lanes>::$vec as SimdBase<S>>::from_slice(simd, ac);
                    $simd_op(va).store_slice(oc);
                }
                for (x, o) in ai.remainder().iter().zip(oi.into_remainder()) {
                    *o = $scalar_op(*x);
                }
            }

            let mut out = vec![<$elem>::default(); a.len()];
            let caps = Caps::detect();
            vaco_simd::dispatch_kernel!(caps, simd => body(simd, a, &mut out));
            out
        }
    };
}

elementwise2!(
    v_sat_add_u8,
    u8s,
    u8,
    ops::simd::saturating_add_u8,
    ops::saturating_add_u8
);
elementwise2!(
    v_sat_sub_u8,
    u8s,
    u8,
    ops::simd::saturating_sub_u8,
    ops::saturating_sub_u8
);
elementwise2!(
    v_sat_add_u16,
    u16s,
    u16,
    ops::simd::saturating_add_u16,
    u16::saturating_add
);
elementwise2!(
    v_sat_sub_u16,
    u16s,
    u16,
    ops::simd::saturating_sub_u16,
    u16::saturating_sub
);
elementwise2!(
    v_sat_add_i16,
    i16s,
    i16,
    ops::simd::saturating_add_i16,
    ops::saturating_add_i16
);
elementwise2!(
    v_sat_sub_i16,
    i16s,
    i16,
    ops::simd::saturating_sub_i16,
    ops::saturating_sub_i16
);
elementwise2!(
    v_avg_round,
    u8s,
    u8,
    ops::simd::rounded_avg_u8,
    ops::rounded_avg_u8
);
elementwise2!(
    v_avg_trunc,
    u8s,
    u8,
    ops::simd::truncated_avg_u8,
    ops::truncated_avg_u8
);
elementwise2!(
    v_abs_diff,
    u8s,
    u8,
    ops::simd::abs_diff_u8,
    ops::abs_diff_u8
);
elementwise1!(v_abs_i16, i16s, i16, ops::simd::abs_i16, ops::abs_i16);
elementwise1!(v_abs_i32, i32s, i32, ops::simd::abs_i32, ops::abs_i32);

// --- the deterministic edge sweep ---------------------------------------

#[test]
fn edge_corpus_saturating_add_u8() {
    testing::check_binary_u8("saturating_add_u8", v_sat_add_u8, ops::saturating_add_u8);
}

#[test]
fn edge_corpus_saturating_sub_u8() {
    testing::check_binary_u8("saturating_sub_u8", v_sat_sub_u8, ops::saturating_sub_u8);
}

#[test]
fn edge_corpus_rounded_avg_u8() {
    testing::check_binary_u8("rounded_avg_u8", v_avg_round, ops::rounded_avg_u8);
}

#[test]
fn edge_corpus_truncated_avg_u8() {
    testing::check_binary_u8("truncated_avg_u8", v_avg_trunc, ops::truncated_avg_u8);
}

#[test]
fn edge_corpus_abs_diff_u8() {
    testing::check_binary_u8("abs_diff_u8", v_abs_diff, ops::abs_diff_u8);
}

/// `select_u8` is ternary, so it does not fit [`testing::check_binary_u8`]'s
/// shape; sweep [`testing::edge_patterns`] directly as the mask source
/// against two distinguishable ramps.
#[test]
fn edge_corpus_select_u8() {
    for len in 0..=200usize {
        let a: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        let b: Vec<u8> = a.iter().rev().copied().collect();
        for mask in testing::edge_patterns(len) {
            let want: Vec<u8> = mask
                .iter()
                .zip(&a)
                .zip(&b)
                .map(|((&m, &x), &y)| ops::select_u8(m, x, y))
                .collect();
            assert_eq!(v_select(&mask, &a, &b), want, "select_u8 len={len}");
        }
    }
}

// --- the reductions and the widening shapes -----------------------------

/// The three horizontal reductions, plus the explicit rotate tree, over exactly
/// one native vector.
fn reductions(vals: &[i32]) -> [(i32, i32); 4] {
    #[inline(always)]
    fn body<S: Lanes>(simd: S, vals: &[i32]) -> [(i32, i32); 4] {
        let n = <S::i32s as SimdBase<S>>::N;
        let head = &vals[..n];
        let v = <S::i32s as SimdBase<S>>::from_slice(simd, head);
        let block = i32x4::from_slice(simd, &vals[..4]);
        [
            (ops::simd::hsum_i32(v), ops::hsum_i32(head)),
            (ops::simd::hmin_i32(v), ops::hmin_i32(head)),
            (ops::simd::hmax_i32(v), ops::hmax_i32(head)),
            (ops::simd::hsum_i32x4_tree(block), ops::hsum_i32(&vals[..4])),
        ]
    }
    let caps = Caps::detect();
    vaco_simd::dispatch_kernel!(caps, simd => body(simd, vals))
}

/// `madd_i16_i32`: the `pmaddwd` shape, over exactly one native vector.
fn madd(a: &[i16], b: &[i16]) -> (Vec<i32>, Vec<i32>) {
    #[inline(always)]
    fn body<S: Lanes>(simd: S, a: &[i16], b: &[i16]) -> (Vec<i32>, Vec<i32>) {
        let n = <S::i16s as SimdBase<S>>::N;
        let va = <S::i16s as SimdBase<S>>::from_slice(simd, &a[..n]);
        let vb = <S::i16s as SimdBase<S>>::from_slice(simd, &b[..n]);
        let got = ops::simd::madd_i16_i32::<S>(va, vb).as_slice().to_vec();
        let want = (0..n / 2)
            .map(|i| ops::madd_i16_i32(a[2 * i], b[2 * i], a[2 * i + 1], b[2 * i + 1]))
            .collect();
        (got, want)
    }
    let caps = Caps::detect();
    vaco_simd::dispatch_kernel!(caps, simd => body(simd, a, b))
}

/// `wmla_u8_i16`: the broadcast-coefficient widening MAC, and the `u8 -> i16`
/// widen it is built on.
fn wmla(acc: &[i16], src: &[u8], c: i16) -> (Vec<i16>, Vec<i16>) {
    #[inline(always)]
    fn body<S: Lanes>(simd: S, acc: &[i16], src: &[u8], c: i16) -> (Vec<i16>, Vec<i16>) {
        let n8 = <S::u8s as SimdBase<S>>::N;
        let n16 = <S::i16s as SimdBase<S>>::N;
        let a0 = <S::i16s as SimdBase<S>>::from_slice(simd, &acc[..n16]);
        let a1 = <S::i16s as SimdBase<S>>::from_slice(simd, &acc[n16..2 * n16]);
        let v = <S::u8s as SimdBase<S>>::from_slice(simd, &src[..n8]);

        let (r0, r1) = ops::simd::wmla_u8_i16::<S>((a0, a1), v, c);
        let mut got = r0.as_slice().to_vec();
        got.extend_from_slice(r1.as_slice());

        let want = (0..n8)
            .map(|i| ops::wmla_u8_i16(acc[i], src[i], c))
            .collect();
        (got, want)
    }
    let caps = Caps::detect();
    vaco_simd::dispatch_kernel!(caps, simd => body(simd, acc, src, c))
}

/// `pack_u8_from_i16`: the `packuswb` the substrate does not have.
fn pack(lo: &[i16], hi: &[i16]) -> (Vec<u8>, Vec<u8>) {
    #[inline(always)]
    fn body<S: Lanes>(simd: S, lo: &[i16], hi: &[i16]) -> (Vec<u8>, Vec<u8>) {
        let n = <S::i16s as SimdBase<S>>::N;
        let vl = <S::i16s as SimdBase<S>>::from_slice(simd, &lo[..n]);
        let vh = <S::i16s as SimdBase<S>>::from_slice(simd, &hi[..n]);
        let got = ops::simd::pack_u8_from_i16::<S>(vl, vh).as_slice().to_vec();
        let want = lo[..n]
            .iter()
            .chain(&hi[..n])
            .map(|&x| ops::clip_u8(i32::from(x)))
            .collect();
        (got, want)
    }
    let caps = Caps::detect();
    vaco_simd::dispatch_kernel!(caps, simd => body(simd, lo, hi))
}

/// Drives [`ops::dispatched_select_u8_row`] — already whole-slice and
/// tail-handling on its own, so unlike the other drivers above this needs no
/// `dispatch_kernel!`/native-width plumbing of its own.
fn v_select(mask: &[u8], a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; a.len()];
    ops::dispatched_select_u8_row(Caps::detect(), mask, a, b, &mut out);
    out
}

// --- proptests ----------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn saturating_add_u8_agrees(pairs in prop::collection::vec(any::<(u8, u8)>(), 0..200)) {
        let (a, b): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        let want: Vec<u8> = a.iter().zip(&b).map(|(&x, &y)| ops::saturating_add_u8(x, y)).collect();
        prop_assert_eq!(v_sat_add_u8(&a, &b), want);
    }

    #[test]
    fn saturating_sub_u8_agrees(pairs in prop::collection::vec(any::<(u8, u8)>(), 0..200)) {
        let (a, b): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        let want: Vec<u8> = a.iter().zip(&b).map(|(&x, &y)| ops::saturating_sub_u8(x, y)).collect();
        prop_assert_eq!(v_sat_sub_u8(&a, &b), want);
    }

    #[test]
    fn saturating_add_u16_agrees(pairs in prop::collection::vec(any::<(u16, u16)>(), 0..200)) {
        let (a, b): (Vec<u16>, Vec<u16>) = pairs.into_iter().unzip();
        let want: Vec<u16> = a.iter().zip(&b).map(|(&x, &y)| x.saturating_add(y)).collect();
        prop_assert_eq!(v_sat_add_u16(&a, &b), want);
    }

    #[test]
    fn saturating_sub_u16_agrees(pairs in prop::collection::vec(any::<(u16, u16)>(), 0..200)) {
        let (a, b): (Vec<u16>, Vec<u16>) = pairs.into_iter().unzip();
        let want: Vec<u16> = a.iter().zip(&b).map(|(&x, &y)| x.saturating_sub(y)).collect();
        prop_assert_eq!(v_sat_sub_u16(&a, &b), want);
    }

    #[test]
    fn saturating_add_i16_agrees(pairs in prop::collection::vec(any::<(i16, i16)>(), 0..200)) {
        let (a, b): (Vec<i16>, Vec<i16>) = pairs.into_iter().unzip();
        let want: Vec<i16> = a.iter().zip(&b).map(|(&x, &y)| ops::saturating_add_i16(x, y)).collect();
        prop_assert_eq!(v_sat_add_i16(&a, &b), want);
    }

    #[test]
    fn saturating_sub_i16_agrees(pairs in prop::collection::vec(any::<(i16, i16)>(), 0..200)) {
        let (a, b): (Vec<i16>, Vec<i16>) = pairs.into_iter().unzip();
        let want: Vec<i16> = a.iter().zip(&b).map(|(&x, &y)| ops::saturating_sub_i16(x, y)).collect();
        prop_assert_eq!(v_sat_sub_i16(&a, &b), want);
    }

    #[test]
    fn rounded_avg_u8_agrees(pairs in prop::collection::vec(any::<(u8, u8)>(), 0..200)) {
        let (a, b): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        // Cross-checked against the definition, not just against our own
        // identity: the rounded average really is (a + b + 1) / 2.
        let want: Vec<u8> = a.iter().zip(&b)
            .map(|(&x, &y)| ((u16::from(x) + u16::from(y) + 1) / 2) as u8)
            .collect();
        prop_assert_eq!(v_avg_round(&a, &b), want);
    }

    #[test]
    fn truncated_avg_u8_agrees(pairs in prop::collection::vec(any::<(u8, u8)>(), 0..200)) {
        let (a, b): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        let want: Vec<u8> = a.iter().zip(&b)
            .map(|(&x, &y)| ((u16::from(x) + u16::from(y)) / 2) as u8)
            .collect();
        prop_assert_eq!(v_avg_trunc(&a, &b), want);
    }

    #[test]
    fn abs_diff_u8_agrees(pairs in prop::collection::vec(any::<(u8, u8)>(), 0..200)) {
        let (a, b): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        let want: Vec<u8> = a.iter().zip(&b).map(|(&x, &y)| x.abs_diff(y)).collect();
        prop_assert_eq!(v_abs_diff(&a, &b), want);
    }

    #[test]
    fn select_u8_agrees(triples in prop::collection::vec(any::<(u8, u8, u8)>(), 0..200)) {
        let mut mask = Vec::new();
        let mut a = Vec::new();
        let mut b = Vec::new();
        for (m, x, y) in triples {
            mask.push(m);
            a.push(x);
            b.push(y);
        }
        let want: Vec<u8> = mask.iter().zip(&a).zip(&b)
            .map(|((&m, &x), &y)| ops::select_u8(m, x, y))
            .collect();
        prop_assert_eq!(v_select(&mask, &a, &b), want);
    }

    #[test]
    fn abs_i16_agrees(a in prop::collection::vec(any::<i16>(), 0..200)) {
        let want: Vec<i16> = a.iter().map(|&x| ops::abs_i16(x)).collect();
        prop_assert_eq!(v_abs_i16(&a), want);
    }

    #[test]
    fn abs_i32_agrees(a in prop::collection::vec(any::<i32>(), 0..200)) {
        let want: Vec<i32> = a.iter().map(|&x| ops::abs_i32(x)).collect();
        prop_assert_eq!(v_abs_i32(&a), want);
    }

    #[test]
    fn reductions_agree(vals in prop::collection::vec(any::<i32>(), 64)) {
        for (got, want) in reductions(&vals) {
            prop_assert_eq!(got, want);
        }
    }

    #[test]
    fn madd_i16_i32_agrees(
        a in prop::collection::vec(any::<i16>(), 64),
        b in prop::collection::vec(any::<i16>(), 64),
    ) {
        let (got, want) = madd(&a, &b);
        prop_assert_eq!(got, want);
    }

    #[test]
    fn wmla_u8_i16_agrees(
        acc in prop::collection::vec(any::<i16>(), 128),
        src in prop::collection::vec(any::<u8>(), 64),
        c in any::<i16>(),
    ) {
        let (got, want) = wmla(&acc, &src, c);
        prop_assert_eq!(got, want);
    }

    #[test]
    fn pack_u8_from_i16_agrees(
        lo in prop::collection::vec(any::<i16>(), 64),
        hi in prop::collection::vec(any::<i16>(), 64),
    ) {
        let (got, want) = pack(&lo, &hi);
        prop_assert_eq!(got, want);
    }
}

/// `abs` on `MIN` is the one place a composition cannot match `std`, and the
/// reference is written to match the *vector*, not the other way round. Pin it
/// so nobody "fixes" the reference later.
#[test]
fn abs_of_min_passes_through() {
    assert_eq!(ops::abs_i16(i16::MIN), i16::MIN);
    assert_eq!(ops::abs_i32(i32::MIN), i32::MIN);
    assert_eq!(v_abs_i16(&[i16::MIN; 64]), vec![i16::MIN; 64]);
    assert_eq!(v_abs_i32(&[i32::MIN; 64]), vec![i32::MIN; 64]);
}
