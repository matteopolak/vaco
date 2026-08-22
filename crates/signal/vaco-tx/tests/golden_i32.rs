//! Golden vectors pinning the `i32` arithmetic contract.
//!
//! # Why these exist and what changing them means
//!
//! The Q31 paths are a **specification**, not an implementation detail: several
//! codecs define fixed-point decoding normatively and are conformance-tested
//! against exact output. Every rounding and saturation decision in
//! `vaco_tx::fixed` compounds through `log_r(n)` stages, so "close enough" is a
//! conformance failure rather than a quality trade.
//!
//! A digest below changing is therefore **a codec-affecting decision**, not a
//! test fixup. If one moves, either the contract changed deliberately — in which
//! case the change is reviewed, documented in `docs/signal/vaco-tx.md` and
//! versioned with the crate — or something broke.
//!
//! # Regenerating
//!
//! ```text
//! cargo test -p vaco-tx --test golden_i32 -- --ignored --nocapture print_golden
//! ```
//!
//! and paste the emitted table. Review the diff; do not paste it blind.
//!
//! # Why digests rather than full vectors
//!
//! A 2048-point transform is 4096 words. Committing them all would make the
//! diff unreadable, which defeats the point of review. FNV-1a over the exact
//! output words has no rounding of its own, so a single changed LSB anywhere
//! changes the digest — the property that matters. Small cases additionally
//! carry their literal output, so a failure is debuggable without a regenerate
//! step.

#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    reason = "test code: lengths are literals and a failed index is a failed test"
)]

use std::sync::Arc;
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind, fixed};

/// The pinned input generator. Deterministic, spread across the Q31 range, and
/// **part of the contract**: changing it invalidates every digest.
fn input(len: usize, seed: u64) -> Vec<i32> {
    let mut s = seed ^ 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            // Keep two bits of headroom so the vectors exercise ordinary codec
            // data rather than the saturation edge, which has its own test.
            ((s >> 32) as i32) >> 2
        })
        .collect()
}

fn fnv1a(words: &[i32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

struct Case {
    kind: TxKind,
    inverse: bool,
    len: usize,
    flags: TxFlags,
}

const fn c(kind: TxKind, inverse: bool, len: usize, flags: TxFlags) -> Case {
    Case {
        kind,
        inverse,
        len,
        flags,
    }
}

/// The `(kind, direction, len, flags)` combinations real codecs use.
///
/// MDCT 2048/256 is AAC LC, 512/256 is AC-3, 960/480/240/120 is Opus and
/// AAC-LD/ELD, 64 is ATRAC and DTS. The FFT entries are the underlying
/// complex lengths.
static GOLDEN: &[Case] = &[
    c(TxKind::Fft, false, 8, TxFlags::empty()),
    c(TxKind::Fft, false, 15, TxFlags::empty()),
    c(TxKind::Fft, false, 64, TxFlags::empty()),
    c(TxKind::Fft, false, 120, TxFlags::empty()),
    c(TxKind::Fft, false, 128, TxFlags::empty()),
    c(TxKind::Fft, false, 240, TxFlags::empty()),
    c(TxKind::Fft, false, 512, TxFlags::empty()),
    c(TxKind::Fft, true, 512, TxFlags::empty()),
    c(TxKind::Fft, false, 11, TxFlags::empty()),
    c(TxKind::Fft, false, 121, TxFlags::empty()),
    c(TxKind::Mdct, false, 2048, TxFlags::empty()),
    c(TxKind::Mdct, true, 2048, TxFlags::empty()),
    c(TxKind::Mdct, true, 2048, TxFlags::FULL_IMDCT),
    c(TxKind::Mdct, false, 256, TxFlags::empty()),
    c(TxKind::Mdct, false, 512, TxFlags::empty()),
    c(TxKind::Mdct, true, 512, TxFlags::empty()),
    c(TxKind::Mdct, false, 960, TxFlags::empty()),
    c(TxKind::Mdct, true, 480, TxFlags::empty()),
    c(TxKind::Mdct, false, 64, TxFlags::empty()),
    c(TxKind::Rdft, false, 512, TxFlags::empty()),
    c(TxKind::Rdft, true, 512, TxFlags::empty()),
    c(TxKind::Rdft, false, 2048, TxFlags::REAL_TO_REAL),
    c(TxKind::Dct, false, 32, TxFlags::empty()),
    c(TxKind::Dct, true, 32, TxFlags::empty()),
    c(TxKind::Dct, false, 512, TxFlags::empty()),
    c(TxKind::DctI, false, 65, TxFlags::empty()),
    c(TxKind::DstI, false, 63, TxFlags::empty()),
];

/// Digests, in the order of [`GOLDEN`]. Split out so a regeneration is a
/// single reviewable block rather than 27 scattered edits.
static DIGESTS: &[u64] = &include!("golden_i32_digests.in");

fn run(case: &Case) -> Vec<i32> {
    let dir = if case.inverse {
        Direction::Inverse
    } else {
        Direction::Forward
    };
    let plan = Plan::<i32>::new(case.kind, dir, case.len, fixed::ONE, case.flags).unwrap();
    let mut tx = Tx::new(Arc::clone(&plan));
    let x = input(
        plan.input_len(),
        case.len as u64 * 31 + u64::from(case.inverse),
    );
    let mut out = vec![0i32; plan.output_len()];
    tx.execute(&mut out, &x);
    out
}

#[test]
fn golden_vectors_are_unchanged() {
    assert_eq!(
        GOLDEN.len(),
        DIGESTS.len(),
        "the case table and the digest table have drifted apart"
    );
    for (case, &want) in GOLDEN.iter().zip(DIGESTS) {
        let got = fnv1a(&run(case));
        assert_eq!(
            got,
            want,
            "{} {} len={} flags={:?}: digest changed. This is a CONTRACT change; see the module docs.",
            case.kind,
            if case.inverse { "inverse" } else { "forward" },
            case.len,
            case.flags
        );
    }
}

/// A handful of literal outputs, so a digest failure can be diagnosed without
/// regenerating anything.
#[test]
fn small_vectors_are_literal() {
    // FFT of length 4 over a DC input of 0.5: one bin at 0.5, the rest zero.
    // Stage scaling divides by 4 across two radix-2-equivalent stages, so the
    // DC bin comes back at exactly the input value.
    let plan = Plan::<i32>::new(
        TxKind::Fft,
        Direction::Forward,
        4,
        fixed::ONE,
        TxFlags::empty(),
    )
    .unwrap();
    let mut tx = Tx::new(plan);
    let half = 1 << 30;
    let x = [half, 0, half, 0, half, 0, half, 0];
    let mut out = [0i32; 8];
    tx.execute(&mut out, &x);
    assert_eq!(out, [half, 0, 0, 0, 0, 0, 0, 0]);

    // Unit impulse: every bin is the input, divided by n.
    let plan = Plan::<i32>::new(
        TxKind::Fft,
        Direction::Forward,
        8,
        fixed::ONE,
        TxFlags::empty(),
    )
    .unwrap();
    let mut tx = Tx::new(plan);
    let mut x = [0i32; 16];
    x[0] = 1 << 30;
    let mut out = [0i32; 16];
    tx.execute(&mut out, &x);
    for k in 0..8 {
        assert_eq!(out[2 * k], (1 << 30) / 8, "bin {k} real");
        assert_eq!(out[2 * k + 1], 0, "bin {k} imaginary");
    }
}

/// The contract's headline claim: identical output on every run, in any order,
/// from any plan built for the same parameters.
#[test]
fn fixed_point_is_deterministic_across_plans_and_runs() {
    for case in GOLDEN {
        let a = run(case);
        let b = run(case);
        assert_eq!(a, b, "{} len={} not reproducible", case.kind, case.len);
    }
}

/// Full-scale input must saturate, never wrap. A wrap would show up as a sign
/// flip, which is the single worst failure mode a fixed-point decoder has.
#[test]
fn saturation_never_wraps() {
    for &n in &[8usize, 15, 64, 120, 512] {
        let plan = Plan::<i32>::fft(n, false).unwrap();
        let mut tx = Tx::new(Arc::clone(&plan));
        let x = vec![i32::MIN; 2 * n];
        let mut out = vec![0i32; 2 * n];
        tx.execute(&mut out, &x);
        // Every output is a bounded combination of inputs at -1.0, so the DC
        // bin is -1.0 (saturated) and nothing may come back positive-large.
        assert!(out[0] <= 0, "n={n} DC bin wrapped to {}", out[0]);
    }
}

#[test]
#[ignore = "regenerates the golden table; run explicitly and review the diff"]
fn print_golden() {
    println!("[");
    for case in GOLDEN {
        let d = format!("{:016x}", fnv1a(&run(case)));
        let grouped = d
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("_");
        println!(
            "    0x{}, // {} {} len={}",
            grouped,
            case.kind,
            if case.inverse { "inv" } else { "fwd" },
            case.len
        );
    }
    println!("]");
}

/// Precision, measured rather than assumed.
///
/// Scale-every-stage costs roughly `log₂n / 2` bits of SNR. Measured on this
/// input: 152.5 dB at n=64 falling to 138.9 dB at n=2048 — about 23 effective
/// bits at the largest AAC size, comfortably above the 16- and 24-bit output
/// the codecs produce. The floors below sit ~6 dB under the measurement, so a
/// real regression fails here long before it becomes audible, while ordinary
/// noise does not.
#[test]
fn fixed_point_snr_meets_the_documented_floor() {
    for &(n, floor_db) in &[
        (64usize, 145.0f64),
        (128, 143.0),
        (256, 141.0),
        (512, 136.0),
        (1024, 134.0),
        (2048, 132.0),
    ] {
        let x = input(2 * n, n as u64 + 5);
        let plan = Plan::<i32>::fft(n, false).unwrap();
        let mut tx = Tx::new(Arc::clone(&plan));
        let mut out = vec![0i32; 2 * n];
        tx.execute(&mut out, &x);

        let q = f64::from(1u32 << 31);
        let re: Vec<f64> = (0..n).map(|i| f64::from(x[2 * i]) / q).collect();
        let im: Vec<f64> = (0..n).map(|i| f64::from(x[2 * i + 1]) / q).collect();
        let (wr, wi) = vaco_tx::reference::dft(&re, &im, false);

        let mut sig = 0.0;
        let mut noise = 0.0;
        for k in 0..n {
            // The fixed-point transform produces DFT/n.
            for (want, got) in [
                (wr[k] / n as f64, f64::from(out[2 * k]) / q),
                (wi[k] / n as f64, f64::from(out[2 * k + 1]) / q),
            ] {
                sig += want * want;
                noise += (want - got) * (want - got);
            }
        }
        let snr = 10.0 * (sig / noise.max(1e-300)).log10();
        println!("n={n}: SNR {snr:.1} dB");
        assert!(
            snr >= floor_db,
            "n={n}: SNR {snr:.1} dB is below the documented floor of {floor_db} dB"
        );
    }
}
