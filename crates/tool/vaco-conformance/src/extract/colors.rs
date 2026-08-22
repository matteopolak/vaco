//! Differential check of `vaco_core::parse`'s named-colour table.
//!
//! # What it is
//!
//! `ffmpeg -colors` prints the reference's named-colour set as
//! `Name<spaces>#rrggbb`. Our table is `vaco_core::parse::color_names` plus
//! `color_by_name`. Both are compared case-insensitively, because the CLI
//! matches names case-insensitively and comparing the *spelling* the reference
//! chose for its listing would be comparing presentation, not behaviour.
//!
//! # What is a divergence and what is not
//!
//! A name only one side knows **is** a divergence: `-fill_color papayawhip` has
//! to mean the same thing to both programs, and a missing name is a hard
//! rejection at the CLI. A differing RGB value is a divergence for the same
//! reason.
//!
//! The listing's *capitalisation* is not compared. That is a presentation
//! choice in a help output; if we ever implement `-colors` ourselves, its text
//! becomes a C0 conformance target in its own right and that is where the
//! spelling gets tested.
//!
//! # How to change it
//!
//! If the reference ever gains an alpha column, extend [`parse_colors`] and add
//! the field to the comparison — do not fold alpha into the hex string, because
//! a report that says `ours=#ff0000ff theirs=#ff0000` is worse than one that
//! names the field.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::extract::{FieldDivergence, TableReport, tidy};
use crate::refbin::Reference;
use crate::run::{Invocation, capture_stdout, run};

/// How many unlisted names are confirmed behaviourally before the extractor
/// stops spawning processes. A handful of divergences is normal; hundreds would
/// mean the listing parse broke, and probing all of them would be slow and
/// pointless.
const MAX_CONFIRMATIONS: usize = 24;

/// Does the reference actually reject `name`, or is it merely absent from the
/// listing?
///
/// A listing is a help output; the parser is the behaviour. Asking the parser
/// directly is what turns "not in the listing" into "genuinely unknown", and
/// the distinction changes what a table owner should do about it.
#[must_use]
pub fn reference_accepts(reference: &Reference, name: &str) -> Option<bool> {
    let inv = Invocation::new(
        &reference.ffprobe,
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={name}:s=2x2:d=0.04"),
            "-show_entries",
            "stream=width",
            "-of",
            "csv=p=0",
        ],
    )
    .with_timeout(Duration::from_secs(20));
    run(&inv).ok().map(|obs| obs.succeeded())
}

/// Parse `ffmpeg -colors`: `Name` then `#rrggbb`.
///
/// Keys are lowercased; the value keeps the reference's own hex spelling.
#[must_use]
pub fn parse_colors(text: &str) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        let (Some(name), Some(hex)) = (cols.next(), cols.next()) else {
            continue;
        };
        let Some(digits) = hex.strip_prefix('#') else {
            continue;
        };
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        out.insert(
            name.to_ascii_lowercase(),
            (name.to_owned(), digits.to_ascii_lowercase()),
        );
    }
    out
}

/// Render our colour as the reference renders it, so a divergence line reads
/// as a comparison rather than as two different notations.
#[must_use]
pub fn ours_hex(name: &str) -> Option<String> {
    let c = vaco_core::parse::color_by_name(name)?;
    Some(format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b))
}

/// Check our colour table against `ffmpeg -colors`.
#[must_use]
pub fn check(reference: &Reference) -> TableReport {
    let inv = Invocation::new(
        &reference.ffmpeg,
        ["-hide_banner", "-nostdin", "-loglevel", "error", "-colors"],
    )
    .with_timeout(Duration::from_secs(20));
    let mut report = TableReport {
        table: "colors".to_owned(),
        oracle: inv.command_line(),
        ..TableReport::default()
    };
    report.notes.push(
        "names are compared case-insensitively; the listing's capitalisation is \
         presentation, and becomes a C0 target only when we implement `-colors`."
            .to_owned(),
    );

    let text = match capture_stdout(&inv) {
        Ok(t) => t,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    let theirs = parse_colors(&text);
    let ours: BTreeMap<String, String> = vaco_core::parse::color_names()
        .map(|n| (n.to_ascii_lowercase(), n.to_owned()))
        .collect();

    report.ours_count = ours.len();
    report.theirs_count = theirs.len();

    for key in ours.keys() {
        if !theirs.contains_key(key) {
            report.only_ours.push(key.clone());
        }
    }
    // Confirm the listing against the parser. `-colors` prints `Gray` but not
    // `Grey`, and `LightGrey` but not `LightGray` — an inconsistency in the
    // reference's own spelling choices — so "absent from the listing" is worth
    // checking against "rejected by the parser" before anyone acts on it.
    let mut confirmed_rejected = 0_usize;
    for name in report.only_ours.iter().take(MAX_CONFIRMATIONS) {
        match reference_accepts(reference, name) {
            Some(true) => report.notes.push(format!(
                "`{name}` is absent from the listing but the reference ACCEPTS it; \
                 the listing is incomplete, not our table"
            )),
            Some(false) => confirmed_rejected += 1,
            None => {}
        }
    }
    if confirmed_rejected > 0 {
        report.notes.push(format!(
            "{confirmed_rejected} of the names only we have were confirmed \
             behaviourally: the reference rejects them at the CLI, so our set is a \
             strict superset rather than a listing artifact"
        ));
    }
    for (key, (spelling, _)) in &theirs {
        if !ours.contains_key(key) {
            report.only_theirs.push(spelling.clone());
        }
    }
    tidy(&mut report.only_ours);
    tidy(&mut report.only_theirs);

    for (key, name) in &ours {
        let Some((_, their_hex)) = theirs.get(key) else {
            continue;
        };
        let Some(our_hex) = ours_hex(name) else {
            report.fields.push(FieldDivergence {
                entity: key.clone(),
                field: "rgb".to_owned(),
                ours: "<name listed but not resolvable>".to_owned(),
                theirs: their_hex.clone(),
            });
            continue;
        };
        if &our_hex != their_hex {
            report.fields.push(FieldDivergence {
                entity: key.clone(),
                field: "rgb".to_owned(),
                ours: format!("#{our_hex}"),
                theirs: format!("#{their_hex}"),
            });
        }
    }
    report.fields.sort();
    report
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "an out-of-range index in a test is a failing test"
)]
mod tests {
    use super::{ours_hex, parse_colors};

    const SAMPLE: &str = "\
name                             #RRGGBB
AliceBlue                        #f0f8ff
AntiqueWhite                     #faebd7
Black                            #000000
";

    #[test]
    fn the_header_line_is_not_a_colour() {
        let m = parse_colors(SAMPLE);
        assert_eq!(m.len(), 3);
        assert!(!m.contains_key("name"));
    }

    #[test]
    fn names_are_keyed_lowercase_and_keep_their_spelling() {
        let m = parse_colors(SAMPLE);
        assert_eq!(m["aliceblue"].0, "AliceBlue");
        assert_eq!(m["aliceblue"].1, "f0f8ff");
    }

    #[test]
    fn our_side_renders_in_the_reference_notation() {
        assert_eq!(ours_hex("AliceBlue").as_deref(), Some("f0f8ff"));
        assert_eq!(ours_hex("black").as_deref(), Some("000000"));
        assert_eq!(ours_hex("not-a-colour"), None);
    }

    #[test]
    fn our_table_is_non_trivial() {
        let n = vaco_core::parse::color_names().count();
        assert!(n > 100, "only {n} colours; the check would prove little");
    }
}
