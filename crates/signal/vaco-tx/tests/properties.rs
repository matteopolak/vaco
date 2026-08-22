//! Structural properties, and the differential test between the SIMD and
//! scalar kernels.
//!
//! Round-trip alone is a weak test: a permutation that is its own inverse
//! round-trips perfectly while being completely wrong. Linearity, Parseval and
//! the shift theorem each fail on a different class of bug, which is why all
//! four are here.

#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    reason = "test code: lengths are literals and a failed index is a failed test"
)]

use std::sync::Arc;
use vaco_tx::{Decomposition, Direction, Plan, Tx, TxFlags, TxKind};

fn lcg(seed: u64, n: usize) -> Vec<f64> {
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((s >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        })
        .collect()
}

fn fft_f64(n: usize, inverse: bool, input: &[f64]) -> Vec<f64> {
    let plan = Plan::<f64>::fft(n, inverse).unwrap();
    let mut tx = Tx::new(Arc::clone(&plan));
    let mut out = vec![0.0; plan.output_len()];
    tx.execute(&mut out, input);
    out
}

fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f64, f64::max)
}

const LENGTHS: &[usize] = &[
    1, 2, 3, 5, 6, 7, 8, 11, 12, 13, 16, 17, 19, 23, 24, 30, 32, 36, 45, 49, 60, 64, 91, 97, 100,
    120, 121, 128, 143, 240, 250, 256, 289, 360, 480, 512, 720, 960, 1000, 1024,
];

#[test]
fn inverse_of_forward_is_n_times_identity() {
    for &n in LENGTHS {
        let x = lcg(n as u64, 2 * n);
        let mid = fft_f64(n, false, &x);
        let back = fft_f64(n, true, &mid);
        let want: Vec<f64> = x.iter().map(|v| v * n as f64).collect();
        let tol = 1e-9 * (n as f64);
        assert!(
            max_abs_diff(&back, &want) < tol,
            "n={n} round trip error {}",
            max_abs_diff(&back, &want)
        );
    }
}

#[test]
fn transform_is_linear() {
    for &n in LENGTHS {
        let a = lcg(n as u64 + 100, 2 * n);
        let b = lcg(n as u64 + 200, 2 * n);
        let (alpha, beta) = (0.375, -1.25);
        let mixed: Vec<f64> = a
            .iter()
            .zip(&b)
            .map(|(x, y)| alpha * x + beta * y)
            .collect();

        let fa = fft_f64(n, false, &a);
        let fb = fft_f64(n, false, &b);
        let fm = fft_f64(n, false, &mixed);
        let want: Vec<f64> = fa
            .iter()
            .zip(&fb)
            .map(|(x, y)| alpha * x + beta * y)
            .collect();
        assert!(max_abs_diff(&fm, &want) < 1e-9 * n as f64, "n={n}");
    }
}

#[test]
fn parseval_holds() {
    for &n in LENGTHS {
        let x = lcg(n as u64 + 300, 2 * n);
        let f = fft_f64(n, false, &x);
        let time: f64 = x.iter().map(|v| v * v).sum();
        let freq: f64 = f.iter().map(|v| v * v).sum::<f64>() / n as f64;
        assert!(
            (time - freq).abs() <= 1e-9 * time.max(1.0),
            "n={n}: {time} vs {freq}"
        );
    }
}

#[test]
fn dc_input_gives_a_single_bin() {
    for &n in LENGTHS {
        let mut x = vec![0.0; 2 * n];
        for i in 0..n {
            x[2 * i] = 1.0;
        }
        let f = fft_f64(n, false, &x);
        assert!((f[0] - n as f64).abs() < 1e-9 * n as f64, "n={n} dc bin");
        assert!(f[1].abs() < 1e-9 * n as f64, "n={n} dc imaginary");
        for k in 1..n {
            assert!(
                f[2 * k].abs() < 1e-8 * n as f64 && f[2 * k + 1].abs() < 1e-8 * n as f64,
                "n={n} bin {k} should be zero, got ({}, {})",
                f[2 * k],
                f[2 * k + 1]
            );
        }
    }
}

#[test]
fn unit_impulse_gives_a_flat_spectrum() {
    for &n in LENGTHS {
        let mut x = vec![0.0; 2 * n];
        x[0] = 1.0;
        let f = fft_f64(n, false, &x);
        for k in 0..n {
            let mag = f[2 * k].hypot(f[2 * k + 1]);
            assert!((mag - 1.0).abs() < 1e-9, "n={n} bin {k} magnitude {mag}");
        }
    }
}

/// The single most common FFT bug is a flipped twiddle sign, and it shows up
/// here as energy in bin `n-k` instead of bin `k`.
#[test]
fn a_pure_tone_lands_in_its_own_bin() {
    for &n in LENGTHS {
        if n < 4 {
            continue;
        }
        let k0 = n / 4;
        let mut x = vec![0.0; 2 * n];
        for j in 0..n {
            let t = core::f64::consts::TAU * (k0 * j % n) as f64 / n as f64;
            x[2 * j] = t.cos();
            x[2 * j + 1] = t.sin();
        }
        let f = fft_f64(n, false, &x);
        for k in 0..n {
            let mag = f[2 * k].hypot(f[2 * k + 1]);
            let want = if k == k0 { n as f64 } else { 0.0 };
            assert!(
                (mag - want).abs() < 1e-7 * n as f64,
                "n={n} k0={k0}: bin {k} magnitude {mag}, wanted {want}"
            );
        }
    }
}

/// A circular shift in time is a linear phase ramp in frequency.
#[test]
fn shift_theorem_holds() {
    for &n in LENGTHS {
        if n < 3 {
            continue;
        }
        let x = lcg(n as u64 + 400, 2 * n);
        let d = n / 3 + 1;
        let mut shifted = vec![0.0; 2 * n];
        for j in 0..n {
            let src = (j + n - d % n) % n;
            shifted[2 * j] = x[2 * src];
            shifted[2 * j + 1] = x[2 * src + 1];
        }
        let fx = fft_f64(n, false, &x);
        let fs = fft_f64(n, false, &shifted);
        for k in 0..n {
            let theta = -core::f64::consts::TAU * ((d * k) % n) as f64 / n as f64;
            let (s, c) = theta.sin_cos();
            let wr = fx[2 * k] * c - fx[2 * k + 1] * s;
            let wi = fx[2 * k] * s + fx[2 * k + 1] * c;
            assert!(
                (fs[2 * k] - wr).abs() < 1e-8 * n as f64
                    && (fs[2 * k + 1] - wi).abs() < 1e-8 * n as f64,
                "n={n} k={k}"
            );
        }
    }
}

/// The vector kernels must reproduce the scalar reference **exactly**.
///
/// They share one butterfly source and neither uses FMA, so equality is
/// achievable and is a far sharper instrument than a tolerance. What this
/// really tests is the load/store indexing in `simd.rs`, which is the only
/// thing that differs between the two paths.
#[test]
fn simd_and_scalar_agree_bit_for_bit() {
    for &n in LENGTHS {
        for inverse in [false, true] {
            let x: Vec<f32> = lcg(n as u64 + 500, 2 * n)
                .into_iter()
                .map(|v| v as f32)
                .collect();
            let plan = Plan::<f32>::fft(n, inverse).unwrap();

            let mut vector = Tx::new(Arc::clone(&plan));
            let mut a = vec![0.0f32; plan.output_len()];
            vector.execute(&mut a, &x);

            let mut scalar = Tx::new(Arc::clone(&plan));
            scalar.set_scalar_reference(true);
            let mut b = vec![0.0f32; plan.output_len()];
            scalar.execute(&mut b, &x);

            assert_eq!(a, b, "n={n} inverse={inverse}: SIMD diverged from scalar");
        }
    }
}

#[test]
fn simd_and_scalar_agree_for_every_derived_kind() {
    for &n in &[64usize, 120, 240, 256, 480, 960, 1024] {
        let cases: &[(TxKind, usize)] = &[
            (TxKind::Rdft, n),
            (TxKind::Mdct, n),
            (TxKind::Dct, n),
            (TxKind::DctI, n),
            (TxKind::DstI, n),
        ];
        for &(kind, len) in cases {
            for dir in [Direction::Forward, Direction::Inverse] {
                let Ok(plan) = Plan::<f32>::new(kind, dir, len, 1.0, TxFlags::empty()) else {
                    continue;
                };
                let x: Vec<f32> = lcg(len as u64 + 600, plan.input_len())
                    .into_iter()
                    .map(|v| v as f32)
                    .collect();
                let mut v = Tx::new(Arc::clone(&plan));
                let mut a = vec![0.0f32; plan.output_len()];
                v.execute(&mut a, &x);
                let mut s = Tx::new(Arc::clone(&plan));
                s.set_scalar_reference(true);
                let mut b = vec![0.0f32; plan.output_len()];
                s.execute(&mut b, &x);
                assert_eq!(a, b, "{kind} {dir:?} len={len}");
            }
        }
    }
}

/// `Plan::new` must never panic, never hang and never allocate unboundedly,
/// for any length a caller can name.
///
/// Stands in for the `cargo-fuzz` target the crate would otherwise carry:
/// `fuzz/` lives outside this crate's directory, so the same coverage is
/// exercised exhaustively over the small range and by sampling above it.
#[test]
fn plan_new_is_total_over_every_length() {
    for n in 1usize..=2048 {
        let made = Plan::<f32>::fft(n, false);
        assert!(made.is_ok(), "no plan for length {n}");
        let plan = made.unwrap();
        assert_eq!(plan.len(), n);
        assert_eq!(plan.input_len(), 2 * n);
        assert!(plan.scratch_len() >= 2 * n || n == 1);
    }
    // Large primes, prime powers and the awkward composites in between.
    for &n in &[
        4093usize, 4096, 6561, 8191, 10007, 16384, 19683, 32003, 65521, 65536, 99991, 131071,
    ] {
        assert!(Plan::<f32>::fft(n, false).is_ok(), "no plan for {n}");
    }
    // And the boundary: accepted at the cap, rejected above it.
    assert!(Plan::<f32>::fft(1 << 24, false).is_ok());
    assert!(Plan::<f32>::fft((1 << 24) + 1, false).is_err());
    assert!(Plan::<f32>::fft(0, false).is_err());
}

/// The selector itself is otherwise untested code: every rule in the table
/// should fire for the length it is meant to.
#[test]
fn each_decomposition_rule_fires_where_expected() {
    let rule = |n: usize| Plan::<f32>::fft(n, false).unwrap().describe().decomposition;

    assert!(matches!(rule(1), Decomposition::Identity));
    assert!(matches!(rule(1024), Decomposition::MixedRadix { .. }));
    assert!(matches!(rule(960), Decomposition::MixedRadix { .. }));
    // 11 is prime and below the direct-DFT threshold.
    assert!(matches!(rule(11), Decomposition::Direct { n: 11 }));
    // 143 = 11·13: coprime, so Good-Thomas, with Rader or Direct underneath.
    assert!(matches!(rule(143), Decomposition::PrimeFactor { .. }));
    // 176 = 16·11: the smooth part peels off.
    assert!(matches!(rule(176), Decomposition::PrimeFactor { .. }));
    // A prime above the direct threshold goes to Rader.
    assert!(matches!(rule(97), Decomposition::Rader { p: 97, .. }));
    // A prime power that is neither smooth nor prime falls to Bluestein.
    assert!(matches!(rule(121), Decomposition::Bluestein { .. }));
    assert!(matches!(rule(2809), Decomposition::Bluestein { .. }));
}

#[test]
fn inplace_execution_matches_out_of_place() {
    for &n in &[8usize, 60, 64, 120, 256] {
        let x: Vec<f32> = lcg(n as u64 + 700, 2 * n)
            .into_iter()
            .map(|v| v as f32)
            .collect();
        let out_of_place = Plan::<f32>::fft(n, false).unwrap();
        let mut t = Tx::new(Arc::clone(&out_of_place));
        let mut want = vec![0.0f32; 2 * n];
        t.execute(&mut want, &x);

        let inplace =
            Plan::<f32>::new(TxKind::Fft, Direction::Forward, n, 1.0, TxFlags::INPLACE).unwrap();
        let mut t2 = Tx::new(inplace);
        let mut buf = x.clone();
        t2.execute_inplace(&mut buf);
        assert_eq!(buf, want, "n={n}");
    }
}

#[test]
fn scale_is_applied_to_the_output() {
    let n = 64;
    let x: Vec<f32> = lcg(11, 2 * n).into_iter().map(|v| v as f32).collect();
    let unit = Plan::<f32>::fft(n, false).unwrap();
    let mut t = Tx::new(Arc::clone(&unit));
    let mut a = vec![0.0f32; 2 * n];
    t.execute(&mut a, &x);

    let scaled =
        Plan::<f32>::new(TxKind::Fft, Direction::Forward, n, 0.25, TxFlags::empty()).unwrap();
    let mut t2 = Tx::new(scaled);
    let mut b = vec![0.0f32; 2 * n];
    t2.execute(&mut b, &x);
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(*x * 0.25, *y);
    }
}

#[test]
fn inplace_without_the_flag_is_rejected_at_plan_time() {
    // MDCT forward has different input and output lengths, so INPLACE cannot
    // be honoured and must fail loudly rather than silently corrupting.
    assert!(
        Plan::<f32>::new(TxKind::Mdct, Direction::Forward, 64, 1.0, TxFlags::INPLACE).is_err()
    );
    assert!(
        Plan::<f32>::new(
            TxKind::Rdft,
            Direction::Forward,
            64,
            1.0,
            TxFlags::REAL_TO_REAL | TxFlags::REAL_TO_IMAGINARY
        )
        .is_err()
    );
}

/// Randomised round-trip and linearity, over arbitrary data at every awkward
/// length the decomposition table can reach.
///
/// The deterministic tests above pin specific signals; this one goes looking
/// for the input that breaks them. Lengths are drawn from the same set rather
/// than from `1..=N` because a shrunk counterexample is only useful if the
/// length it names is one a codec could actually ask for.
mod randomised {
    use super::{LENGTHS, fft_f64, max_abs_diff};
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn round_trip_holds_for_arbitrary_data(
            idx in 0usize..LENGTHS.len(),
            raw in proptest::collection::vec(-1.0f64..1.0, 2..4096),
        ) {
            let n = LENGTHS[idx];
            let mut x = vec![0.0f64; 2 * n];
            for (i, slot) in x.iter_mut().enumerate() {
                *slot = raw[i % raw.len()];
            }
            let back = fft_f64(n, true, &fft_f64(n, false, &x));
            let want: Vec<f64> = x.iter().map(|v| v * n as f64).collect();
            prop_assert!(max_abs_diff(&back, &want) < 1e-9 * n as f64, "n={}", n);
        }

        #[test]
        fn linearity_holds_for_arbitrary_data(
            idx in 0usize..LENGTHS.len(),
            a in proptest::collection::vec(-1.0f64..1.0, 2..2048),
            b in proptest::collection::vec(-1.0f64..1.0, 2..2048),
            alpha in -4.0f64..4.0,
        ) {
            let n = LENGTHS[idx];
            let xa: Vec<f64> = (0..2 * n).map(|i| a[i % a.len()]).collect();
            let xb: Vec<f64> = (0..2 * n).map(|i| b[i % b.len()]).collect();
            let mix: Vec<f64> = xa.iter().zip(&xb).map(|(p, q)| alpha * p + q).collect();
            let fa = fft_f64(n, false, &xa);
            let fb = fft_f64(n, false, &xb);
            let fm = fft_f64(n, false, &mix);
            let want: Vec<f64> = fa.iter().zip(&fb).map(|(p, q)| alpha * p + q).collect();
            prop_assert!(max_abs_diff(&fm, &want) < 1e-8 * n as f64, "n={}", n);
        }
    }
}
