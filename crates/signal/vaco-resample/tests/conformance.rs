//! Differential conformance against recorded `FFmpeg` 8.1 output.
//!
//! These tests do not need `ffmpeg` on the machine: the reference's output is
//! recorded in `tests/common/golden.rs`, following the pattern `vaco-chlayout`
//! established. Plan 13 §1.5.4 requires `cargo test` to pass without the
//! reference binary present, and a recording is also the only way a CI run can
//! notice that *we* changed when the reference did not.
//!
//! Every assertion here carries the grade it is asserting, so a regression
//! reports as "this was Exact and is now Equivalent" rather than as a number.

#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    clippy::cast_possible_wrap,
    clippy::drop_non_drop,
    clippy::field_reassign_with_default,
    clippy::redundant_closure_for_method_calls,
    clippy::collapsible_if,
    reason = "test and benchmark code; a panic here is a failing test, which is the point"
)]

#[path = "common/golden.rs"]
mod golden;
#[path = "common/harness.rs"]
mod harness;

use harness::{max_abs_diff, run_f64, simple, snr};
use vaco_chlayout::ChannelLayout;
use vaco_resample::design::{DesignParams, Window, build_bank};
use vaco_resample::mix::{MixLevels, build_matrix};
use vaco_resample::{DitherMethod, ResampleOptions};

fn mono() -> ChannelLayout {
    ChannelLayout::MONO
}

// ---------------------------------------------------------------------------
// The coefficient bank
// ---------------------------------------------------------------------------

#[test]
fn bank_1to2_matches_the_reference() {
    let mut b = harness::budget();
    let params = DesignParams {
        phases: 2,
        filter_size: 32,
        factor: DesignParams::factor(48000, 96000, 0.97),
        window: Window::Kaiser,
        kaiser_beta: 9.0,
    };
    assert_eq!(params.factor, 1.0, "an upsample clamps the factor to 1");
    let bank = build_bank::<f64>(&params, &mut b).unwrap();
    assert_eq!(bank.taps, 32);
    assert_eq!(bank.centre, 15);

    // Phase 0 is an exact unit impulse. This is the strongest single check on
    // the design: only `factor == 1` makes `sinc` vanish at every non-zero
    // integer, and only the absence of a per-phase normalisation leaves the
    // centre tap at exactly 1.
    let p0 = bank.phase(0).unwrap();
    for (j, v) in p0.iter().enumerate() {
        let want = if j == 15 { 1.0 } else { 0.0 };
        assert_eq!(*v, want, "phase 0 tap {j}");
    }

    let p1 = bank.phase(1).unwrap();
    let mut worst = 0.0_f64;
    for (j, v) in p1.iter().enumerate() {
        worst = worst.max((v - golden::BANK_1TO2_PHASE1[j]).abs());
    }
    // The golden values were recorded at full `f64` precision, so this is a
    // last-bit comparison, not a tolerance.
    assert!(
        worst < 1e-15,
        "phase 1 differs from the reference by {worst:e}"
    );
}

#[test]
fn downsampling_stretches_the_filter() {
    let mut b = harness::budget();
    let factor = DesignParams::factor(48000, 32000, 0.97);
    assert!((factor - 2.0 / 3.0 * 0.97).abs() < 1e-15);
    let params = DesignParams {
        phases: 2,
        filter_size: 32,
        factor,
        window: Window::Kaiser,
        kaiser_beta: 9.0,
    };
    let bank = build_bank::<f64>(&params, &mut b).unwrap();
    // MEASURED: the support of a downsampled impulse is 49.5 input samples.
    assert_eq!(bank.taps, 50);
    let p0 = bank.phase(0).unwrap();
    // Centre tap is the factor itself, up to the phase-0 normalisation.
    //
    // The reference measures 0.646_672_861_011_427; we compute 0.646_676_6.
    // That 5.8e-6 relative gap is the whole of the downsampling residual
    // documented in `design`, and it is pinned here so that a change to the
    // design shows up as a moved number rather than as a moved SNR.
    let want = 0.646_672_861_011_427_f64;
    let got = p0[bank.centre];
    let rel = ((got - want) / want).abs();
    assert!(
        rel < 1e-5,
        "downsample centre tap {got} against the reference's {want} (rel {rel:e})"
    );
}

// ---------------------------------------------------------------------------
// Rate conversion
// ---------------------------------------------------------------------------

/// Grades, in the D11 vocabulary, for each recorded conversion.
#[test]
fn rate_conversion_grades() {
    let cases: [(&str, u32, u32, &[f64], f64); 5] = [
        (
            "44100 -> 48000 (up)",
            44100,
            48000,
            &golden::RES_44100_48000,
            250.0,
        ),
        (
            "48000 -> 96000 (up)",
            48000,
            96000,
            &golden::RES_48000_96000,
            250.0,
        ),
        (
            "8000 -> 48000 (up)",
            8000,
            48000,
            &golden::RES_8000_48000,
            250.0,
        ),
        (
            "48000 -> 44100 (down)",
            48000,
            44100,
            &golden::RES_48000_44100,
            100.0,
        ),
        (
            "48000 -> 32000 (down)",
            48000,
            32000,
            &golden::RES_48000_32000,
            100.0,
        ),
    ];
    for (name, ir, or, want, floor) in cases {
        let mut rs = simple(ir, or, mono(), mono());
        let got = run_f64(&mut rs, &golden::SIGNAL_256, 1, 1, 1024);
        assert_eq!(
            got.len(),
            want.len(),
            "{name}: output sample count must match exactly"
        );
        let s = snr(want, &got);
        println!(
            "  {name:24} SNR = {s:7.1} dB  max|Δ| = {:.3e}",
            max_abs_diff(want, &got)
        );
        assert!(
            s >= floor,
            "{name}: SNR {s:.1} dB is below the declared floor of {floor} dB"
        );
    }
}

/// The reference's output count is `ceil(in · p / q)`. Getting this wrong is a
/// hard failure, not a tolerance: it desynchronises a pipeline.
#[test]
fn output_sample_counts_match() {
    for (ir, or, n, want) in [
        (44100u32, 48000u32, 100usize, 109usize),
        (44100, 48000, 1000, 1089),
        (44100, 48000, 44100, 48000),
        (48000, 44100, 48000, 44100),
        (48000, 96000, 1, 2),
        (48000, 32000, 3, 2),
    ] {
        let mut rs = simple(ir, or, mono(), mono());
        let got = run_f64(&mut rs, &vec![0.0; n], 1, 1, 4096);
        assert_eq!(got.len(), want, "{ir} -> {or} with {n} input samples");
    }
}

// ---------------------------------------------------------------------------
// Rematrixing
// ---------------------------------------------------------------------------

#[test]
fn mix_matrices_match_the_reference() {
    let levels = MixLevels::default();
    let mut exact = 0;
    let mut divergent: Vec<&str> = Vec::new();
    for (a, b, rows, want) in golden::MATRIX_PAIRS {
        let (Some(li), Some(lo)) = (ChannelLayout::from_name(a), ChannelLayout::from_name(b))
        else {
            panic!("layout name `{a}` or `{b}` did not parse");
        };
        let m = build_matrix(
            &li,
            &lo,
            &levels,
            vaco_resample::MatrixEncoding::None,
            false,
        )
        .unwrap();
        assert_eq!(m.rows, rows, "{a} -> {b}: row count");
        let got = m.as_slice();
        assert_eq!(got.len(), want.len(), "{a} -> {b}: size");
        if got.iter().zip(want).all(|(x, y)| x == y) {
            exact += 1;
        } else {
            let worst = got
                .iter()
                .zip(want)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0_f64, f64::max);
            println!("  {a:12} -> {b:8} max|Δ| = {worst:.3e}");
            divergent.push(a);
        }
    }
    println!(
        "  {exact}/{} layout pairs bit-identical",
        golden::MATRIX_PAIRS.len()
    );
    assert!(
        divergent.is_empty(),
        "layout pairs that no longer match the reference: {divergent:?}"
    );
}

/// Rematrixing a layout to itself must be the identity, for every standard
/// layout the vocabulary knows.
#[test]
fn identity_matrix_for_every_standard_layout() {
    let levels = MixLevels::default();
    for (name, layout) in ChannelLayout::standard() {
        let m = build_matrix(
            &layout,
            &layout,
            &levels,
            vaco_resample::MatrixEncoding::None,
            false,
        )
        .unwrap();
        for o in 0..m.rows {
            for i in 0..m.cols {
                let want = if o == i { 1.0 } else { 0.0 };
                assert_eq!(m.get(o, i), want, "{name}: m[{o}][{i}]");
            }
        }
    }
}

/// Integer output applies a `1.0` ceiling to the largest row; float output does
/// not. Both measured.
#[test]
fn normalisation_depends_on_the_output_format() {
    let levels = MixLevels::default();
    let l51 = ChannelLayout::from_name("5.1").unwrap();
    let st = ChannelLayout::STEREO;
    let float = build_matrix(
        &l51,
        &st,
        &levels,
        vaco_resample::MatrixEncoding::None,
        false,
    )
    .unwrap();
    let int = build_matrix(
        &l51,
        &st,
        &levels,
        vaco_resample::MatrixEncoding::None,
        true,
    )
    .unwrap();
    assert!(
        (float.peak() - (1.0 + 2.0 * f64::from(core::f32::consts::FRAC_1_SQRT_2))).abs() < 1e-12,
        "float output is not rescaled: peak {}",
        float.peak()
    );
    assert!(
        (int.peak() - 1.0).abs() < 1e-12,
        "integer output is capped at 1.0: peak {}",
        int.peak()
    );
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[test]
fn option_names_match_the_reference_spelling() {
    let mut o = ResampleOptions::default();
    o.set_from_str("clev=0.5:slev=0.25:lfe_mix_level=0.125:filter_size=64:cutoff=0.9")
        .unwrap();
    assert_eq!(o.center_mix_level, 0.5);
    assert_eq!(o.surround_mix_level, 0.25);
    assert_eq!(o.lfe_mix_level, 0.125);
    assert_eq!(o.filter_size, 64);
    assert_eq!(o.cutoff, 0.9);

    let mut o = ResampleOptions::default();
    o.set_from_str("center_mix_level=0.5:surround_mix_level=0.25:rematrix_volume=2")
        .unwrap();
    assert_eq!(o.center_mix_level, 0.5);
    assert_eq!(o.rematrix_volume, 2.0);

    // soxr is accepted and aliased, never rejected and never silent.
    let mut o = ResampleOptions::default();
    o.set_from_str("resampler=soxr:precision=28:cheby=1")
        .unwrap();
    assert_eq!(o.engine, vaco_resample::Engine::Soxr);

    let mut o = ResampleOptions::default();
    assert!(o.set("nonesuch", "1").is_err());
    assert!(o.set("filter_size", "x").is_err());
    assert!(o.set_from_str("filter_size").is_err());
}

#[test]
fn every_dither_name_parses() {
    for name in [
        "none",
        "rectangular",
        "triangular",
        "triangular_hp",
        "lipshitz",
        "f_weighted",
        "modified_e_weighted",
        "improved_e_weighted",
        "shibata",
        "low_shibata",
        "high_shibata",
    ] {
        let m = DitherMethod::from_name(name).unwrap();
        assert_eq!(m.name(), name);
    }
    assert!(DitherMethod::from_name("nope").is_err());
}
