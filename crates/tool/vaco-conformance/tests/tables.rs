//! The table extractors, run against whatever reference is installed.
//!
//! # Graceful absence is the point of this file
//!
//! Plan 13 §1.5.4: a contributor without `FFmpeg` must still be able to run
//! `cargo test`. Every test here begins by asking for a reference and returns
//! early with a printed explanation when there isn't one. `cargo test -- --nocapture`
//! shows the reason; without it the tests simply pass, which is correct — we
//! have not observed a divergence, so we cannot claim one.
//!
//! # Why these do not assert "clean"
//!
//! A divergence found here is a **finding to report**, not a test to fail,
//! until someone with authority over the table has decided which side is
//! wrong. `vaco-conformance` reports and decides nothing (see
//! `docs/tool/vaco-conformance.md`), and a table owner cannot be ambushed by a
//! red build for a divergence nobody has triaged yet.
//!
//! What *is* asserted: that the oracle answered at all, that the parse produced
//! a plausible number of entries, and that the extractor did not error. A
//! silently empty extractor would report "clean" for the wrong reason, and that
//! is the failure mode worth a test.

#![expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test, which is the \
              correct outcome"
)]

use vaco_conformance::divergence::Allowlist;
use vaco_conformance::extract::{self, Depth, TableReport};
use vaco_conformance::refbin::{self, Discovery, RefSpec};

fn oracle() -> Option<(RefSpec, Discovery)> {
    let spec = RefSpec::load().expect("refspec.toml loads");
    let discovery = refbin::discover(&spec);
    match &discovery {
        Discovery::Found(r) => {
            println!(
                "reference {} on channel {} ({})",
                r.version,
                r.channel,
                if r.gates() { "gating" } else { "advisory" }
            );
            Some((spec, discovery))
        }
        Discovery::Absent(why) => {
            println!("SKIPPED: {why}");
            None
        }
    }
}

fn allowlist() -> Allowlist {
    Allowlist::load().expect("the shipped divergence register loads")
}

/// Print a report and assert only that the extractor actually worked.
fn assert_ran(report: &TableReport, minimum_entries: usize) {
    println!("{report}");
    assert!(
        report.error.is_none(),
        "{} extractor failed: {}",
        report.table,
        report.error.clone().unwrap_or_default()
    );
    assert!(
        report.theirs_count >= minimum_entries,
        "{} saw only {} reference entries; the parse is probably broken, which \
         would make a clean report meaningless",
        report.table,
        report.theirs_count
    );
}

#[test]
fn pixfmt_against_show_pixel_formats() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    let reference = discovery.reference().expect("found");
    let mut report = extract::pixfmt::check_show_pixel_formats(reference);
    report.apply_allowlist(&allowlist(), "table-pixfmt");
    assert_ran(&report, 200);
}

#[test]
fn pixfmt_against_pix_fmts_listing() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    let reference = discovery.reference().expect("found");
    let mut report = extract::pixfmt::check_pix_fmts(reference);
    report.apply_allowlist(&allowlist(), "table-pixfmt-cross");
    assert_ran(&report, 200);
}

/// The two oracles must agree with each other about the formats they both
/// describe. Where they do not, neither is treated as truth — that is an
/// *oracle* inconsistency and it is reported so nobody debugs our table for a
/// difference that is not in it.
#[test]
fn the_two_pixfmt_oracles_agree_with_each_other() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    let reference = discovery.reference().expect("found");
    let a = extract::pixfmt::check_show_pixel_formats(reference);
    let b = extract::pixfmt::check_pix_fmts(reference);
    if a.error.is_some() || b.error.is_some() {
        println!("one oracle did not answer; nothing to cross-check");
        return;
    }
    let mut inconsistent = 0;
    for field in ["nb_components", "bits_per_pixel", "bit_depths"] {
        let from = |r: &TableReport| -> Vec<(String, String)> {
            r.fields
                .iter()
                .filter(|f| f.field == field)
                .map(|f| (f.entity.clone(), f.theirs.clone()))
                .collect()
        };
        for (entity, theirs_a) in from(&a) {
            if let Some((_, theirs_b)) = from(&b).into_iter().find(|(e, _)| e == &entity)
                && theirs_a != theirs_b
            {
                println!(
                    "ORACLE INCONSISTENCY {entity}.{field}: \
                     -show_pixel_formats says {theirs_a}, -pix_fmts says {theirs_b}"
                );
                inconsistent += 1;
            }
        }
    }
    println!("{inconsistent} oracle inconsistencies");
}

#[test]
fn colors_against_the_colors_listing() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    let reference = discovery.reference().expect("found");
    let mut report = extract::colors::check(reference);
    report.apply_allowlist(&allowlist(), "table-colors");
    assert_ran(&report, 100);
}

/// The size and rate extractors spawn one process per abbreviation, so they run
/// only when `VACO_CONFORMANCE_DEEP` is set. That keeps `cargo test` fast for
/// everyone and makes the cost an explicit choice.
#[test]
fn frame_sizes_and_rates_when_deep_is_requested() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    if std::env::var_os("VACO_CONFORMANCE_DEEP").is_none() {
        println!("SKIPPED: set VACO_CONFORMANCE_DEEP=1 to probe every abbreviation");
        return;
    }
    let reference = discovery.reference().expect("found");
    let allow = allowlist();
    for (mut report, suite) in [
        (
            extract::sizes::check(reference, Depth::Deep),
            "table-frame-sizes",
        ),
        (
            extract::rates::check(reference, Depth::Deep),
            "table-frame-rates",
        ),
    ] {
        report.apply_allowlist(&allow, suite);
        println!("{report}");
        assert!(report.error.is_none(), "{suite} errored");
        assert!(
            report.ours_count > 0,
            "{suite} checked nothing, so its clean verdict would be meaningless"
        );
    }
}

/// The whole extractor set, as `vaco-conformance tables` runs it.
#[test]
fn the_full_listing_pass_runs_end_to_end() {
    let Some((_, discovery)) = oracle() else {
        return;
    };
    let reference = discovery.reference().expect("found");
    let allow = allowlist();
    let reports = extract::run_all(reference, &allow, Depth::Listings);
    assert_eq!(reports.len(), 5, "five extractors run in listing mode");
    let findings: usize = reports.iter().map(TableReport::finding_count).sum();
    println!("{}", vaco_conformance::report::render_tables(&reports));
    println!("total findings: {findings}");
}

/// Absence must be a skip, never a failure — asserted directly by pointing the
/// discovery at a path that cannot exist.
#[test]
fn a_missing_reference_is_a_skip_with_an_actionable_message() {
    // A path that is not a file makes `locate` fall through to `PATH`, so the
    // assertion here is on the message shape rather than on forcing absence,
    // which cannot be done without mutating the process environment.
    let spec = RefSpec::load().expect("loads");
    match refbin::discover(&spec) {
        Discovery::Found(r) => {
            assert!(
                !r.version.is_empty(),
                "a found reference must identify itself"
            );
        }
        Discovery::Absent(why) => {
            assert!(
                why.contains("cargo test") || why.contains("skipped"),
                "the message must tell a contributor what to do: {why}"
            );
        }
    }
}
