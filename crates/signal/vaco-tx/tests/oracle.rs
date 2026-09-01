//! The direct-DFT oracle: every transform, every precision, against the
//! literal definition in `f64`.
//!
//! This is the ground truth that makes the property tests meaningful. It is
//! `O(n²)`, so it runs at the small and medium sizes — but those are where the
//! decomposition rules are most varied, and a rule that is wrong is wrong at
//! every size.

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
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind, reference};

/// Reproducible pseudo-random input. A fixed LCG rather than a crate, so a
/// failure at "n = 240, seed 7" reproduces exactly.
fn signal(n: usize, seed: u64) -> Vec<f64> {
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

fn run_f64(plan: &Arc<Plan<f64>>, input: &[f64]) -> Vec<f64> {
    let mut tx = Tx::new(Arc::clone(plan));
    let mut out = vec![0.0; plan.output_len()];
    tx.execute(&mut out, input);
    out
}

fn rms_rel(got: &[f64], want: &[f64]) -> f64 {
    let num: f64 = got.iter().zip(want).map(|(a, b)| (a - b) * (a - b)).sum();
    let den: f64 = want.iter().map(|b| b * b).sum::<f64>().max(1e-300);
    (num / den).sqrt()
}

/// Lengths chosen to exercise every rule in the decomposition table:
/// powers of two, the Opus 2·3·5 family, primes (Rader), prime squares and
/// prime products (Good–Thomas and Bluestein), and awkward composites.
const LENGTHS: &[usize] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17, 19, 23, 24, 25, 27, 29, 31, 32, 36, 37,
    40, 45, 48, 49, 53, 60, 63, 64, 71, 81, 96, 97, 100, 105, 113, 120, 121, 125, 128, 143, 169,
    180, 192, 210, 240, 243, 251, 256, 289, 315, 337, 360, 384, 419, 441, 480, 512, 529, 625, 720,
    729, 841, 960, 1024,
];

#[test]
fn fft_forward_matches_a_direct_dft() {
    for &n in LENGTHS {
        let re = signal(n, n as u64);
        let im = signal(n, n as u64 + 1);
        let mut interleaved = vec![0.0f64; 2 * n];
        for i in 0..n {
            interleaved[2 * i] = re[i];
            interleaved[2 * i + 1] = im[i];
        }
        let plan = Plan::<f64>::fft(n, false).unwrap();
        let got = run_f64(&plan, &interleaved);
        let (wr, wi) = reference::dft(&re, &im, false);
        let mut want = vec![0.0f64; 2 * n];
        for i in 0..n {
            want[2 * i] = wr[i];
            want[2 * i + 1] = wi[i];
        }
        let err = rms_rel(&got, &want);
        assert!(
            err < 1e-12,
            "n={n} rel-rms {err:e}; decomposition {:?}",
            plan.describe().decomposition
        );
    }
}

#[test]
fn fft_inverse_matches_a_direct_idft() {
    for &n in LENGTHS {
        let re = signal(n, n as u64 + 7);
        let im = signal(n, n as u64 + 8);
        let mut interleaved = vec![0.0f64; 2 * n];
        for i in 0..n {
            interleaved[2 * i] = re[i];
            interleaved[2 * i + 1] = im[i];
        }
        let plan = Plan::<f64>::fft(n, true).unwrap();
        let got = run_f64(&plan, &interleaved);
        let (wr, wi) = reference::dft(&re, &im, true);
        let mut want = vec![0.0f64; 2 * n];
        for i in 0..n {
            want[2 * i] = wr[i];
            want[2 * i + 1] = wi[i];
        }
        assert!(rms_rel(&got, &want) < 1e-12, "n={n}");
    }
}

#[test]
fn f32_fft_matches_the_f64_reference_within_class_c() {
    for &n in LENGTHS {
        let re = signal(n, n as u64 + 3);
        let im = signal(n, n as u64 + 4);
        let mut inter32 = vec![0.0f32; 2 * n];
        for i in 0..n {
            inter32[2 * i] = re[i] as f32;
            inter32[2 * i + 1] = im[i] as f32;
        }
        let plan = Plan::<f32>::fft(n, false).unwrap();
        let mut tx = Tx::new(Arc::clone(&plan));
        let mut out = vec![0.0f32; plan.output_len()];
        tx.execute(&mut out, &inter32);

        let (wr, wi) = reference::dft(&re, &im, false);
        let got: Vec<f64> = out.iter().map(|&v| f64::from(v)).collect();
        let mut want = vec![0.0f64; 2 * n];
        for i in 0..n {
            want[2 * i] = wr[i];
            want[2 * i + 1] = wi[i];
        }
        let err = rms_rel(&got, &want);
        // Class C: relative RMS <= 2^-20 for f32.
        assert!(err < 9.6e-7, "n={n} rel-rms {err:e}");
    }
}

#[test]
fn rdft_matches_the_reference_both_ways() {
    for &n in LENGTHS {
        let x = signal(n, n as u64 + 11);
        let plan = Plan::<f64>::rdft(n, false).unwrap();
        let got = run_f64(&plan, &x);
        let (wr, wi) = reference::rdft(&x);
        let bins = n / 2 + 1;
        assert_eq!(plan.output_len(), 2 * bins, "n={n}");
        let mut want = vec![0.0f64; 2 * bins];
        for i in 0..bins {
            want[2 * i] = wr[i];
            want[2 * i + 1] = wi[i];
        }
        assert!(rms_rel(&got, &want) < 1e-12, "forward n={n}");

        // Inverse of the forward output is n·x.
        let inv = Plan::<f64>::rdft(n, true).unwrap();
        let back = run_f64(&inv, &got);
        let want_back: Vec<f64> = x.iter().map(|v| v * n as f64).collect();
        assert!(rms_rel(&back, &want_back) < 1e-12, "inverse n={n}");
    }
}

#[test]
fn mdct_and_imdct_match_the_reference() {
    for &n in &[
        4usize, 8, 12, 16, 20, 24, 32, 36, 40, 48, 60, 64, 120, 128, 240, 256, 480, 960, 2048,
    ] {
        let x = signal(n, n as u64 + 21);
        let plan = Plan::<f64>::mdct(n, false, 1.0).unwrap();
        let got = run_f64(&plan, &x);
        let want = reference::mdct(&x);
        assert_eq!(got.len(), n / 2);
        assert!(
            rms_rel(&got, &want) < 1e-12,
            "mdct n={n} rel-rms {:e}",
            rms_rel(&got, &want)
        );

        let coeffs = signal(n / 2, n as u64 + 22);
        let full = reference::imdct(&coeffs);
        let half = Plan::<f64>::mdct(n, true, 1.0).unwrap();
        let got_half = run_f64(&half, &coeffs);
        assert!(rms_rel(&got_half, &full[..n / 2]) < 1e-12, "imdct n={n}");

        let fullplan = Plan::<f64>::new(
            TxKind::Mdct,
            Direction::Inverse,
            n,
            1.0,
            TxFlags::FULL_IMDCT,
        )
        .unwrap();
        let got_full = run_f64(&fullplan, &coeffs);
        assert_eq!(got_full.len(), n);
        assert!(rms_rel(&got_full, &full) < 1e-12, "full imdct n={n}");
    }
}

#[test]
fn dct_ii_and_iii_match_the_reference() {
    for &n in &[
        1usize, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 15, 16, 20, 24, 30, 32, 48, 64, 96, 120, 128, 512,
    ] {
        let x = signal(n, n as u64 + 31);
        let fwd =
            Plan::<f64>::new(TxKind::Dct, Direction::Forward, n, 1.0, TxFlags::empty()).unwrap();
        let got = run_f64(&fwd, &x);
        let want = reference::dct2(&x);
        assert!(rms_rel(&got, &want) < 1e-11, "dct-II n={n}");

        let inv =
            Plan::<f64>::new(TxKind::Dct, Direction::Inverse, n, 1.0, TxFlags::empty()).unwrap();
        let got3 = run_f64(&inv, &x);
        let want3 = reference::dct3(&x);
        assert!(rms_rel(&got3, &want3) < 1e-11, "dct-III n={n}");
    }
}

#[test]
fn dct_i_and_dst_i_match_the_reference() {
    for &n in &[
        2usize, 3, 4, 5, 6, 7, 8, 9, 12, 16, 17, 24, 32, 33, 64, 65, 128,
    ] {
        let x = signal(n, n as u64 + 41);
        let c =
            Plan::<f64>::new(TxKind::DctI, Direction::Forward, n, 1.0, TxFlags::empty()).unwrap();
        let got = run_f64(&c, &x);
        assert!(rms_rel(&got, &reference::dct1(&x)) < 1e-11, "dct-I n={n}");

        let s =
            Plan::<f64>::new(TxKind::DstI, Direction::Forward, n, 1.0, TxFlags::empty()).unwrap();
        let got = run_f64(&s, &x);
        assert!(rms_rel(&got, &reference::dst1(&x)) < 1e-11, "dst-I n={n}");
    }
}

#[test]
fn dct_i_and_dst_i_inverses_undo_the_forward() {
    for &n in &[2usize, 3, 5, 8, 12, 16, 17, 32, 64, 65] {
        let x = signal(n, n as u64 + 51);
        for kind in [TxKind::DctI, TxKind::DstI] {
            let f = Plan::<f64>::new(kind, Direction::Forward, n, 1.0, TxFlags::empty()).unwrap();
            let i = Plan::<f64>::new(kind, Direction::Inverse, n, 1.0, TxFlags::empty()).unwrap();
            let mid = run_f64(&f, &x);
            let back = run_f64(&i, &mid);
            assert!(rms_rel(&back, &x) < 1e-11, "{kind} n={n} round trip");
        }
    }
}

#[test]
fn real_to_real_and_real_to_imaginary_drop_the_other_half() {
    let n = 64;
    let x = signal(n, 99);
    let full = Plan::<f64>::rdft(n, false).unwrap();
    let both = run_f64(&full, &x);

    for (flag, offset) in [
        (TxFlags::REAL_TO_REAL, 0usize),
        (TxFlags::REAL_TO_IMAGINARY, 1),
    ] {
        let p = Plan::<f64>::new(TxKind::Rdft, Direction::Forward, n, 1.0, flag).unwrap();
        assert_eq!(p.output_len(), n / 2 + 1);
        let got = run_f64(&p, &x);
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, both[2 * i + offset], "flag {flag:?} bin {i}");
        }
    }
}
