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

use reference::{EXPRESSIONS, MAPS, NUMBERS, Observed, SPECIFIERS};
use vaco_cli_core::{
    MapSpec, NumberLimits, OptionConstants, StreamSpecifier, eval_option, parse_number,
};

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

/// The plain-number grammar, replayed against the `-ac` transcript.
///
/// `-ac` is a C `int` field, so the limits are `int32`. Every row is a verdict
/// the reference gave for that literal.
#[test]
fn plain_number_grammar_matches_the_reference() {
    assert!(NUMBERS.len() > 70, "transcript looks truncated");
    report("number", NUMBERS, |input| {
        match parse_number("ac", input, NumberLimits::int32()) {
            Ok(_) => "OK".to_owned(),
            Err(e) => e.to_string(),
        }
    });
}

/// The expression path, replayed against the `-crf` transcript.
///
/// Two properties, because the transcript distinguishes them: whether the
/// grammar accepted the text at all, and — when it did — whether the value came
/// out NaN. The reference prints the same `Unable to parse …` line for a
/// rejection and for a NaN result, so conflating them would hide four rows
/// (`0/0`, `nan`, `while(0,1)`, `sqrt(-1)`) that parse perfectly well.
#[test]
fn expression_grammar_matches_the_reference() {
    assert!(EXPRESSIONS.len() > 70, "transcript looks truncated");
    // `crf`'s own metadata, read out of the reference's range message:
    // "out of range [-1 - 3.40282e+38]", default 23.
    let crf = OptionConstants::new(23.0, -1.0, 3.402_82e38);
    let mut bad = Vec::new();
    for row in EXPRESSIONS {
        let reference_accepted = row.verdict != "REJECT";
        let ours = eval_option("crf", row.input, crf);
        if reference_accepted != ours.is_ok() {
            bad.push(format!(
                "  {:?}\n    reference: {}\n    vaco:      {}",
                row.input,
                if reference_accepted {
                    "accepted"
                } else {
                    "REJECT"
                },
                if ours.is_ok() { "accepted" } else { "REJECT" },
            ));
            continue;
        }
        if let Ok(v) = ours {
            let reference_nan = row.verdict == "NAN";
            if reference_nan != v.is_nan() {
                bad.push(format!(
                    "  {:?}\n    reference: {}\n    vaco:      {}",
                    row.input,
                    if reference_nan { "NaN" } else { "not NaN" },
                    if v.is_nan() { "NaN" } else { "not NaN" },
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} expression rows disagree with the reference:\n{}",
        bad.len(),
        EXPRESSIONS.len(),
        bad.join("\n")
    );
}

/// The option dialect, asserted directly rather than only via the corpus.
#[test]
fn the_option_dialect_shadows_max_and_min() {
    let crf = OptionConstants::new(23.0, -1.0, 3.402_82e38);
    // Constants, not functions.
    assert!(eval_option("crf", "max(1,2)", crf).is_err());
    assert!(eval_option("crf", "min(1,2)", crf).is_err());
    // ...and they carry the option's own bounds.
    assert_eq!(eval_option("crf", "min", crf), Ok(-1.0));
    assert_eq!(eval_option("crf", "min-1", crf), Ok(-2.0));
    assert_eq!(eval_option("crf", "default", crf), Ok(23.0));
    // Prefix matching still applies: `maxi` is not `max`.
    assert!(eval_option("crf", "maxi", crf).is_err());
    // Filter variables are not in scope on this path.
    for name in ["w", "n", "t"] {
        assert!(eval_option("crf", name, crf).is_err(), "{name}");
    }
}

/// The single most consequential result of the value-grammar probing: the two
/// grammars are *not* the same, and plan 14 §2.5 had it backwards.
#[test]
fn a_plain_number_option_rejects_what_an_expression_option_accepts() {
    let mut number_rejected = 0usize;
    for row in NUMBERS {
        if row.verdict.starts_with("Expected number for") {
            number_rejected += 1;
            // ...and the same text is fine as an expression, when it is one.
        }
    }
    assert!(
        number_rejected > 10,
        "the transcript must exercise rejection"
    );
    // `1*2` is the canonical case.
    assert!(parse_number("ac", "1*2", NumberLimits::int32()).is_err());
    assert!(eval_option("crf", "1*2", OptionConstants::UNKNOWN).is_ok());
}

#[test]
fn the_transcript_covers_every_error_variant() {
    // A transcript that only ever says OK would pass the tests above while
    // proving nothing, so assert the shape of the corpus itself.
    let verdicts: Vec<&str> = SPECIFIERS
        .iter()
        .chain(MAPS)
        .chain(NUMBERS)
        .map(|r| r.verdict)
        .collect();
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
        "Expected number for ac but found",
        "Expected int64 for ac but found",
        "The value for ac was",
    ] {
        assert!(
            verdicts.iter().any(|v| v.starts_with(want)),
            "transcript never exercises {want:?}"
        );
    }
    // And the expression corpus must contain both outcomes.
    assert!(EXPRESSIONS.iter().any(|r| r.verdict == "REJECT"));
    assert!(EXPRESSIONS.iter().any(|r| r.verdict != "REJECT"));
}
