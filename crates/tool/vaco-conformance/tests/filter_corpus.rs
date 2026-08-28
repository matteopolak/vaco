//! End-to-end proof that the `filter` tool actually runs: this crate's own
//! `tests/conformance/filter/*.toml` corpus, driven through the real
//! `Runner` against a real reference, in-process against a real
//! `vaco_filter_core::Graph` — not a mock of either side.
//!
//! # Why this file, distinct from `tables.rs`
//!
//! `tables.rs` tests the probe-table extractors, which report findings
//! rather than asserting cleanliness (this crate has no authority over
//! ffmpeg/vaco disagreements it did not create). A `filter`-tool case is
//! different: `vaco-filter-scope`'s own filters are already measured to
//! `raw-exact`/`behavioural` and shipped on that basis, so a divergence
//! here — on the very media this suite declares — means either this
//! corpus's own case is wrong, or a real regression landed. Both are this
//! crate's business to fail loudly on, which is why this file *does*
//! assert `Agree` rather than merely "the tool ran".
//!
//! Skips gracefully (never fails) when no reference is installed, the
//! same §1.5.4 contract every other test in this crate honours.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]

use vaco_conformance::case::{Tier, Verdict};
use vaco_conformance::divergence::Allowlist;
use vaco_conformance::manifest;
use vaco_conformance::refbin::{self, Discovery, RefSpec};
use vaco_conformance::runner::Runner;

fn run_suite(file_name: &str) -> Option<Vec<vaco_conformance::runner::Outcome>> {
    let spec = RefSpec::load().expect("refspec.toml loads");
    let discovery = refbin::discover(&spec);
    let Discovery::Found(reference) = &discovery else {
        println!("SKIPPED (no reference): {discovery:?}");
        return None;
    };

    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.join("..").join("..").join("..");
    let path = repo_root
        .join("tests")
        .join("conformance")
        .join("filter")
        .join(file_name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let suite = manifest::Suite::parse(&text).unwrap_or_else(|e| panic!("{file_name}: {e}"));
    let cases = suite.expand();
    assert!(!cases.is_empty(), "{file_name} expanded to zero cases");

    let allowlist = Allowlist::load().expect("the shipped divergence register loads");
    let runner = Runner::new(Some(reference), &allowlist);
    let (outcomes, _tally) = runner.run_all(&cases, Tier::Core);
    Some(outcomes)
}

#[test]
fn the_exact_filters_agree_with_the_reference() {
    let Some(outcomes) = run_suite("vaco-filter-scope-exact.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

#[test]
fn boxblur_agrees_with_the_reference() {
    let Some(outcomes) = run_suite("vaco-filter-blur.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

#[test]
fn pixelize_agrees_with_the_reference() {
    let Some(outcomes) = run_suite("vaco-filter-geometry.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

/// The multi-input `filter`-tool cases in `vaco-filter-key`, covering
/// every multi-input adapter shape the crate uses: `maskedmerge` (3
/// inputs, a hand-rolled `Filter`), `maskedmax`/`maskedmin` (3 inputs,
/// `Paired`), `threshold` (4 inputs, `Paired` with an `input_count`
/// override), `maskedclamp` (3 inputs, `Paired`), `maskedthreshold` in
/// both `mode=abs` and the newly-recovered `mode=diff` (2 inputs,
/// `Paired`), and `premultiply` (2 inputs, `Synced` —
/// `vaco-filter-framesync`, the third and last shape) through
/// `filterexec.rs`'s N-source-node support. Also pins the case *count*,
/// not just that every case that ran agreed — every `[[media]]` entry is
/// `fixed` (see the suite's own comment), so eight axis values must expand
/// to exactly eight cases, never more (media multiplying a case) or fewer
/// (a case silently dropped), which is exactly the shape of bug
/// (`Tier::Smoke` silently excluding every case earlier in this campaign)
/// a bare "for o in outcomes" loop cannot catch on its own.
#[test]
fn multi_input_key_filters_agree_with_the_reference() {
    let Some(outcomes) = run_suite("vaco-filter-key-multi.toml") else {
        return;
    };
    assert_eq!(
        outcomes.len(),
        8,
        "expected exactly eight cases (all media `fixed`, eight axis values); got {outcomes:?}"
    );
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

#[test]
fn convolve_agrees_with_the_reference() {
    let Some(outcomes) = run_suite("vaco-filter-convolve.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

/// `scharr`'s own suite, `raw-tolerant` rather than this file's
/// `raw-exact` -- see `vaco-filter-convolve-scharr.toml`'s own comment
/// for the measurement (a proven, position-uncorrelated max-1-count
/// scatter, not a discoverable rule) and the 2026-08-28
/// `AGENT-CONSTRAINTS.md` owner ruling it ships under.
#[test]
fn scharr_agrees_with_the_reference_within_the_measured_scatter() {
    let Some(outcomes) = run_suite("vaco-filter-convolve-scharr.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

/// `colorkey` on `argb` -- the first case to exercise a *packed* pixel
/// format (`filterexec.rs`'s `plane_size_sum`/`fill_planes`/
/// `extract_output` used to assume every plane was `width * height`
/// bytes, wrong for one packed plane of four interleaved components).
/// `colorkey` could not be conformance tested at all before this: it only
/// accepts RGB-with-alpha formats, none of which the harness could
/// previously supply. See `vaco-filter-key-packed.toml`'s own comment for
/// why the source varies both colour (key/non-key) and pre-existing alpha
/// in one case.
#[test]
fn colorkey_agrees_with_the_reference_on_a_packed_format() {
    let Some(outcomes) = run_suite("vaco-filter-key-packed.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

/// `hstack`/`vstack` -- `vaco-filter-stack`'s first `filter`-tool
/// conformance cases, now that `StackRegistry` is wired into
/// `filterexec.rs`'s `REGISTRIES`. `hstack.rs`'s own module doc already
/// carries an extensive hand-run measurement log against real ffmpeg;
/// this pins the width-sum/height-sum byte-layout half of it as a
/// permanent byte-exact check. `xstack` is not covered — see
/// `vaco-filter-stack.toml`'s own comment.
#[test]
fn hstack_and_vstack_agree_with_the_reference_on_mismatched_dimensions() {
    let Some(outcomes) = run_suite("vaco-filter-stack.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

#[test]
fn the_text_ceiling_filters_still_produce_a_frame() {
    let Some(outcomes) = run_suite("vaco-filter-scope-text-ceiling.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree at the behavioural (outcome-class-only) level: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}

/// `oscilloscope` at `behavioural`, not `raw-tolerant` -- see
/// `vaco-filter-scope-oscilloscope.toml`'s own comment for why the
/// measured formulas do not yet add up to a small, defensible byte
/// tolerance.
#[test]
fn oscilloscope_still_produces_a_frame() {
    let Some(outcomes) = run_suite("vaco-filter-scope-oscilloscope.toml") else {
        return;
    };
    for o in &outcomes {
        println!("{}: {:?}", o.case.id, o.verdict.label());
        assert!(
            matches!(o.verdict, Verdict::Agree),
            "case `{}` did not agree at the behavioural (outcome-class-only) level: {:?}\n  ours:   {}\n  theirs: {}",
            o.case.id,
            o.verdict,
            o.ours_command,
            o.theirs_command
        );
    }
}
