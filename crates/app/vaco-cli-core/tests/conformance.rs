//! Replay the recorded reference transcript against our parsers.
//!
//! 1775 rows, each a verdict the shipped `ffmpeg 8.1` gave for one input. This
//! is the crate's real contract: which command lines parse and which are
//! rejected, and with what text.
//!
//! The transcript is data (`tests/reference.rs`), captured once by observation.
//! Nothing here runs the reference binary, so the test is fast, offline and
//! reproducible — and the recorded reference version is pinned in that file.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]

mod reference;

use reference::{MAPS, Observed, SPECIFIERS};
use vaco_cli_core::{MapSpec, StreamSpecifier};

/// What our parser says, in the reference's vocabulary.
fn our_spec_verdict(input: &str) -> String {
    match StreamSpecifier::parse(input) {
        Ok(_) => "OK".to_owned(),
        Err(e) => e.to_string(),
    }
}

fn our_map_verdict(input: &str) -> String {
    match MapSpec::parse(input) {
        Ok(_) => "OK".to_owned(),
        Err(e) => e.to_string(),
    }
}

fn report(kind: &str, rows: &[Observed], ours: impl Fn(&str) -> String) {
    let mut bad = Vec::new();
    for row in rows {
        let got = ours(row.input);
        if got != row.verdict {
            bad.push(format!(
                "  {:?}\n    reference: {}\n    vaco:      {}",
                row.input, row.verdict, got
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} {kind} rows disagree with the reference:\n{}",
        bad.len(),
        rows.len(),
        bad.join("\n")
    );
}

#[test]
fn stream_specifier_grammar_matches_the_reference() {
    assert!(SPECIFIERS.len() > 1500, "transcript looks truncated");
    report("specifier", SPECIFIERS, our_spec_verdict);
}

#[test]
fn map_grammar_matches_the_reference() {
    assert!(MAPS.len() > 90, "transcript looks truncated");
    report("map", MAPS, our_map_verdict);
}

#[test]
fn the_transcript_covers_every_error_variant() {
    // A transcript that only ever says OK would pass the two tests above while
    // proving nothing, so assert the shape of the corpus itself.
    let mut kinds = std::collections::BTreeSet::new();
    for row in SPECIFIERS.iter().chain(MAPS) {
        // First token of the message is enough to identify the variant.
        kinds.insert(row.verdict.split(':').next().unwrap_or(""));
    }
    for want in [
        "OK",
        "Trailing garbage at the end of a stream specifier",
        "Trailing garbage after stream specifier",
        "Stream type specified multiple times",
        "Cannot combine multiple program/group designators in a single stream specifier",
        "Multiple disposition specifiers",
        "Expected program ID, got",
        "Expected stream group idx/ID, got",
        "Expected stream ID, got",
        "Invalid disposition specifier",
        "Invalid stream specifier",
    ] {
        assert!(kinds.contains(want), "transcript never exercises {want:?}");
    }
}
