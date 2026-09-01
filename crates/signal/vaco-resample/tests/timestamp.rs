//! Timestamp compensation: soft, hard, `async`, `first_pts` and the manual
//! API.
//!
//! The sample-count assertions below are not arbitrary: they reproduce
//! numbers measured against `FFmpeg` 9.0.1 through the `aresample` filter
//! (see `crate::timestamp`'s module docs and
//! `docs/signal/vaco-resample.md` for the exact commands). Re-deriving those
//! thresholds here — the exact 0.1s hard-compensation boundary, the exact
//! sample count a full-second jump inserts, and `first_pts`'s role as an
//! assumed baseline rather than a label — is this crate's differential test
//! for the feature, without needing `ffmpeg` on the machine that runs
//! `cargo test`.

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    clippy::field_reassign_with_default,
    reason = "test code; a panic here is a failing test, which is the point"
)]

#[path = "common/harness.rs"]
mod harness;

use harness::{budget, spec};
use vaco_chlayout::ChannelLayout;
use vaco_core::Error;
use vaco_resample::{AudioMut, AudioRef, ResampleOptions, Resampler};
use vaco_sampfmt::SampleFmt;

/// Feed `samples` (mono `f64`) through `rs` in one call and return the total
/// output sample count. The output buffer is sized generously enough
/// (`MAX_COMPENSATION_SAMPLES` of slack) that everything comes back in this
/// one call — deliberately, since a `None` (flush) call would end the stream
/// for good, which these tests do not want mid-sequence.
fn feed(rs: &mut Resampler, samples: &[f64]) -> usize {
    let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    let src = AudioRef::packed(SampleFmt::F64, 1, &bytes).unwrap();
    let slack = usize::try_from(vaco_resample::MAX_COMPENSATION_SAMPLES).unwrap_or(0);
    let mut scratch = vec![0u8; (samples.len() + slack + 16) * 8];
    let mut dst = AudioMut::packed(SampleFmt::F64, 1, &mut scratch).unwrap();
    rs.convert(Some(src), &mut dst).unwrap()
}

fn ramp(n: usize) -> Vec<f64> {
    (0..n).map(|i| (i as f64 * 0.01).sin() * 0.5).collect()
}

fn build(opts: &ResampleOptions) -> Resampler {
    let mut b = budget();
    let s = spec(48000, SampleFmt::F64, ChannelLayout::MONO);
    Resampler::new(&s, &s, opts, &mut b).unwrap()
}

// ── the master switch: default `min_comp` refuses compensation outright ────

#[test]
fn default_options_have_no_compensation_pipeline_at_all() {
    // Matching rates, no mixing, no dither, no compensation option touched:
    // this is the direct format-conversion path (§2.1), which has no dsp
    // stage to compensate through. Measured: a full one-second pts jump fed
    // through plain `aresample=48000` (no async) produces exactly the
    // original sample count — the reference does nothing either. We go
    // further and refuse explicitly rather than silently agreeing to a
    // request we cannot act on (constraints: "refuse rather than
    // approximate").
    let mut rs = build(&ResampleOptions::default());
    let err = rs.advance_pts(48000).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    let err = rs.set_compensation(10, 100).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

// ── hard compensation: exact, one-shot, and bounded at the measured 0.1s ───

#[test]
fn hard_compensation_boundary_is_exact_at_min_hard_comp() {
    // Measured against the reference: a 4800-sample (0.100000s) jump at the
    // default `min_hard_comp=0.1` inserts nothing; 4801 samples
    // (0.100021s) inserts *exactly* 4801, as a single step.
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 0.1;
    opts.max_soft_comp = 0.0; // isolate hard from soft

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap(); // establishes the baseline, no compensation
    let x = ramp(2400);
    let out = feed(&mut rs, &x);
    assert_eq!(out, 2400);

    // Exactly at the boundary: no compensation.
    rs.advance_pts(2400 + 4800).unwrap();
    let x = ramp(100);
    let out = feed(&mut rs, &x);
    assert_eq!(out, 100, "a drift of exactly min_hard_comp must not compensate");
}

#[test]
fn hard_compensation_inserts_exactly_one_sample_past_the_boundary() {
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 0.1;
    opts.max_soft_comp = 0.0;

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    let out = feed(&mut rs, &ramp(2400));
    assert_eq!(out, 2400);

    // One sample past the boundary (0.100021s): the reference inserted
    // exactly 4801 samples of silence, as one step.
    rs.advance_pts(2400 + 4801).unwrap();
    let out = feed(&mut rs, &ramp(100));
    assert_eq!(out, 100 + 4801, "hard compensation must fill the exact deficit");
}

#[test]
fn hard_compensation_fills_a_full_second_gap_exactly() {
    // Measured: a one-second (48000-sample) jump under `async=1` inserted
    // exactly 48000 samples of silence, and total decoded duration went
    // from 5.00s to 6.00s as a result — not fewer, not more.
    let mut opts = ResampleOptions::default();
    opts.async_samples = 1.0;

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    let out = feed(&mut rs, &ramp(48000)); // 1 second of real audio
    assert_eq!(out, 48000);

    rs.advance_pts(48000 + 48000).unwrap(); // a further one-second jump
    let out = feed(&mut rs, &ramp(48000));
    assert_eq!(out, 48000 + 48000, "async=1 must fill the whole gap, no more");
}

#[test]
fn hard_compensation_drops_samples_for_negative_drift() {
    // The reference's own docs describe hard compensation as "padding *or
    // trimming*"; a source that runs ahead of its declared timestamps
    // should have the surplus dropped, symmetrically with the insert case.
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 0.1;
    opts.max_soft_comp = 0.0;

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    let out = feed(&mut rs, &ramp(2400));
    assert_eq!(out, 2400);

    // The source is 4801 samples *ahead* of its declared position: drop them.
    rs.advance_pts(2400 - 4801).unwrap();
    let out = feed(&mut rs, &ramp(5000));
    assert_eq!(out, 5000 - 4801, "hard compensation must drop the exact surplus");
}

// ── `first_pts`: an assumed baseline, not a label ───────────────────────────

#[test]
fn first_pts_matching_the_real_start_avoids_a_spurious_correction() {
    // Measured: a stream starting at pts=48000 (a real one-second offset,
    // held constant thereafter — no further drift) produced zero inserted
    // samples under `async=1` whether `first_pts` was left unset or set to
    // the matching 48000. The starting position of a stream is not drift.
    let mut opts = ResampleOptions::default();
    opts.async_samples = 1.0;
    opts.first_pts = 48000;

    let mut rs = build(&opts);
    rs.advance_pts(48000).unwrap(); // matches first_pts: no drift
    let out = feed(&mut rs, &ramp(4800));
    assert_eq!(out, 4800, "a first pts matching first_pts must not compensate");
}

#[test]
fn first_pts_disagreeing_with_the_real_start_reproduces_the_drift() {
    // Measured: forcing `first_pts=0` against a real start of 48000
    // reliably reproduced the "drift" and triggered the expected hard
    // correction — exactly 48000 samples inserted.
    let mut opts = ResampleOptions::default();
    opts.async_samples = 1.0;
    opts.first_pts = 0;

    let mut rs = build(&opts);
    rs.advance_pts(48000).unwrap(); // disagrees with the assumed baseline of 0
    let out = feed(&mut rs, &ramp(4800));
    assert_eq!(
        out,
        48000 + 4800,
        "first_pts must be compared against as a real baseline, not just recorded"
    );
}

#[test]
fn unset_first_pts_takes_the_first_observed_pts_as_baseline() {
    // Same scenario as above but with `first_pts` left at its sentinel: the
    // very first `advance_pts` call must establish the baseline rather than
    // assuming 0, or every stream with a nonzero real start would spuriously
    // compensate.
    let mut opts = ResampleOptions::default();
    opts.async_samples = 1.0;

    let mut rs = build(&opts);
    rs.advance_pts(48000).unwrap(); // first call: becomes the baseline
    let out = feed(&mut rs, &ramp(4800));
    assert_eq!(out, 4800);
}

// ── soft compensation: reacts to sustained drift, not a step jump ──────────

#[test]
fn soft_compensation_is_bounded_by_max_soft_comp_per_window() {
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 999.0; // never hard
    opts.max_soft_comp = 1000.0; // samples/sec
    opts.comp_duration = 1.0; // one-second windows

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    let mut total_in = 0usize;
    let mut total_out = 0usize;
    // A source clock ~0.4% fast: pts advances faster than samples actually
    // arrive, so the tracker sees growing (positive) drift every block.
    for block in 0..50u32 {
        let n = 1000usize;
        total_in += n;
        let declared_pts = (f64::from(block + 1) * n as f64 * 1.004).round() as i64;
        rs.advance_pts(declared_pts).unwrap();
        total_out += feed(&mut rs, &ramp(n));
    }
    // Soft compensation must never insert more than max_soft_comp *
    // comp_duration in any accounting window; over 50 windows of at most
    // 1000 samples each the ceiling is generous but real — this is a sanity
    // bound, not a measured-to-the-sample reference number (§"What is
    // ours, not measured").
    let extra = total_out as i64 - total_in as i64;
    assert!(extra > 0, "a source running fast of its clock should gain samples, got {extra}");
    assert!(
        extra <= 50 * 1000,
        "soft compensation inserted more than the whole stream, extra={extra}"
    );
}

#[test]
fn soft_compensation_does_not_fire_on_a_one_shot_jump_when_hard_is_disabled() {
    // Measured: the same step-discontinuity scenario that drives hard
    // compensation produced no measurable sample-count change under a
    // soft-only configuration (`min_hard_comp` disabled). Soft compensation
    // answers a source clock running at the wrong *rate*, not a single
    // discontinuity.
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 999.0; // hard disabled
    opts.max_soft_comp = 1000.0;
    opts.comp_duration = 1.0;

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    let out = feed(&mut rs, &ramp(2400));
    assert_eq!(out, 2400);

    // A one-off 4800-sample jump, then perfectly regular timestamps after.
    rs.advance_pts(2400 + 4800).unwrap();
    let out = feed(&mut rs, &ramp(100));
    // A single soft window may claim a share of this block, but it must be
    // bounded by max_soft_comp*comp_duration = 1000, not the whole 4800.
    let extra = out as i64 - 100;
    assert!(
        (0..=1000).contains(&extra),
        "soft compensation exceeded its own cap on a single jump, extra={extra}"
    );
}

// ── `out_samples` must bound queued compensation, not just the real input ──
//
// A caller sizes its output buffer from `out_samples`, so it is a heap
// overflow surface in any caller that trusts it (the same property
// `fuzz/fuzz_targets/resample_convert.rs` already asserts for the rest of
// the crate) if compensation can push the real output past it.

#[test]
fn out_samples_accounts_for_queued_hard_compensation() {
    let mut opts = ResampleOptions::default();
    opts.min_comp = 0.0;
    opts.min_hard_comp = 0.1;
    opts.max_soft_comp = 0.0;

    let mut rs = build(&opts);
    rs.advance_pts(0).unwrap();
    assert_eq!(feed(&mut rs, &ramp(2400)), 2400);

    rs.advance_pts(2400 + 20000).unwrap(); // a big, comfortably-hard jump
    let promised = rs.out_samples(100);
    let out = feed(&mut rs, &ramp(100));
    assert!(
        out <= promised,
        "produced {out} samples but out_samples promised at most {promised}"
    );
    assert_eq!(out, 100 + 20000);
}

#[test]
fn out_samples_accounts_for_an_active_soft_window() {
    let mut opts = ResampleOptions::default();
    opts.first_pts = 0;
    let mut rs = build(&opts);
    rs.set_compensation(5000, 200_000).unwrap();

    let promised = rs.out_samples(1000);
    let out = feed(&mut rs, &ramp(1000));
    assert!(
        out <= promised,
        "produced {out} samples but out_samples promised at most {promised}"
    );
}

// ── the manual API: `set_compensation` ─────────────────────────────────────

#[test]
fn manual_set_compensation_adds_exactly_the_requested_delta_in_one_window() {
    // first_pts forces the dsp pipeline into existence without touching
    // min_comp, so the *automatic* policy never fires on its own — only the
    // manual call does.
    let mut opts = ResampleOptions::default();
    opts.first_pts = 0;

    let mut rs = build(&opts);
    rs.set_compensation(500, 1000).unwrap();
    let out = feed(&mut rs, &ramp(2000)); // >= the window, so it fully resolves here
    assert_eq!(out, 2000 + 500);
}

#[test]
fn manual_set_compensation_removes_samples_for_a_negative_delta() {
    let mut opts = ResampleOptions::default();
    opts.first_pts = 0;

    let mut rs = build(&opts);
    rs.set_compensation(-300, 1000).unwrap();
    let out = feed(&mut rs, &ramp(2000));
    assert_eq!(out, 2000 - 300);
}

#[test]
fn manual_set_compensation_spreads_across_multiple_calls() {
    let mut opts = ResampleOptions::default();
    opts.first_pts = 0;

    let mut rs = build(&opts);
    rs.set_compensation(900, 2000).unwrap();
    // Feed it in four 500-sample blocks; the window (2000 samples) spans all
    // four, so the net effect must match a single big block exactly.
    let mut total = 0usize;
    for _ in 0..4 {
        total += feed(&mut rs, &ramp(500));
    }
    assert_eq!(total, 2000 + 900);
}

#[test]
fn manual_set_compensation_rejects_a_delta_past_the_bound() {
    let mut opts = ResampleOptions::default();
    opts.first_pts = 0;
    let mut rs = build(&opts);

    let too_much = i32::try_from(vaco_resample::MAX_COMPENSATION_SAMPLES + 1)
        .unwrap_or(i32::MAX);
    let err = rs.set_compensation(too_much, 1000).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "got {err:?}");
}

#[test]
fn manual_api_refuses_on_a_direct_core_resampler() {
    let mut rs = build(&ResampleOptions::default());
    let err = rs.set_compensation(1, 100).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

// ── `async`'s resolution into (min_comp, max_soft_comp) ─────────────────────
//
// `Policy` and `ResampleOptions::effective_compensation` are crate-private
// (an implementation detail nothing outside this crate should depend on), so
// these compare `async`'s resolved *behaviour* against the equivalent
// explicit thresholds, through the same public API every other test in this
// file uses — which is also the more faithful test, since it is the
// resolved behaviour that must match the reference, not any particular
// internal representation of it.

/// Run the boundary + full-second-jump scenario used throughout this file
/// and return `(before, after)` sample counts.
fn hard_boundary_scenario(opts: &ResampleOptions) -> (usize, usize) {
    let mut rs = build(opts);
    rs.advance_pts(0).unwrap();
    let before = feed(&mut rs, &ramp(2400));
    rs.advance_pts(2400 + 4801).unwrap();
    let after = feed(&mut rs, &ramp(100));
    (before, after)
}

#[test]
fn async_zero_behaves_like_explicit_all_disabled_defaults() {
    let mut explicit = ResampleOptions::default();
    explicit.min_comp = 0.0;
    explicit.min_hard_comp = 0.1;
    explicit.max_soft_comp = 0.0;
    let mut with_async = explicit;
    with_async.async_samples = 0.0;

    assert_eq!(hard_boundary_scenario(&explicit), (2400, 100 + 4801));
    assert_eq!(hard_boundary_scenario(&with_async), (2400, 100 + 4801));
}

#[test]
fn async_one_reproduces_explicit_hard_only_thresholds() {
    let mut via_async = ResampleOptions::default();
    via_async.async_samples = 1.0;

    let mut explicit = ResampleOptions::default();
    explicit.min_comp = 0.0;
    explicit.min_hard_comp = 0.1; // the reference's own default, spelled out
    explicit.max_soft_comp = 0.0;

    assert_eq!(
        hard_boundary_scenario(&via_async),
        hard_boundary_scenario(&explicit),
        "async=1 must resolve to exactly min_comp=0, max_soft_comp=0"
    );
}

#[test]
fn async_above_one_reproduces_the_explicit_soft_cap() {
    let mut via_async = ResampleOptions::default();
    via_async.async_samples = 1000.0;

    let mut explicit = ResampleOptions::default();
    explicit.min_comp = 0.0;
    explicit.min_hard_comp = 0.1;
    explicit.max_soft_comp = 1000.0;
    explicit.comp_duration = 1.0;

    // A drift comfortably inside soft range (below min_hard_comp): both
    // configurations must queue the identical soft correction.
    let scenario = |opts: &ResampleOptions| {
        let mut rs = build(opts);
        rs.advance_pts(0).unwrap();
        let a = feed(&mut rs, &ramp(2400));
        rs.advance_pts(2400 + 2000).unwrap();
        let b = feed(&mut rs, &ramp(1000));
        (a, b)
    };
    assert_eq!(
        scenario(&via_async),
        scenario(&explicit),
        "async=1000 must resolve to exactly min_comp=0, max_soft_comp=1000"
    );
}

// ── option surface: names, defaults and ranges match the measured `-h` ─────

#[test]
fn compensation_option_defaults_match_the_reference() {
    let d = ResampleOptions::default();
    assert_eq!(d.min_comp, f32::MAX, "reference default is FLT_MAX");
    assert_eq!(d.min_hard_comp, 0.1);
    assert_eq!(d.comp_duration, 1.0);
    assert_eq!(d.max_soft_comp, 0.0);
    assert_eq!(d.async_samples, 0.0);
    assert_eq!(d.first_pts, i64::MIN, "reference default is AV_NOPTS_VALUE-shaped");
    assert_eq!(d.first_pts(), None);
}

#[test]
fn compensation_options_parse_by_the_reference_names() {
    let mut o = ResampleOptions::default();
    o.set_from_str("min_comp=0.5:min_hard_comp=0.2:comp_duration=2:max_soft_comp=10:async=3:first_pts=12345")
        .unwrap();
    assert_eq!(o.min_comp, 0.5);
    assert_eq!(o.min_hard_comp, 0.2);
    assert_eq!(o.comp_duration, 2.0);
    assert_eq!(o.max_soft_comp, 10.0);
    assert_eq!(o.async_samples, 3.0);
    assert_eq!(o.first_pts(), Some(12345));
}

#[test]
fn compensation_options_reject_non_finite_values() {
    let mut o = ResampleOptions::default();
    o.min_comp = f32::NAN;
    assert!(o.validate().is_err());

    let mut o = ResampleOptions::default();
    o.min_hard_comp = -1.0;
    assert!(o.validate().is_err());

    let mut o = ResampleOptions::default();
    o.max_soft_comp = f32::INFINITY;
    assert!(o.validate().is_err());
}
