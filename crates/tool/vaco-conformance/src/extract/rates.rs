//! Differential check of `vaco_core::parse::video_rate`'s abbreviations.
//!
//! # What it is
//!
//! The same shape as [`crate::extract::sizes`], and the same limitation: the
//! reference has no listing of frame-rate abbreviations, so the oracle is asked
//! behaviourally.
//!
//! ```text
//! ffprobe -f lavfi -i color=r=<name>:d=0.04 -show_entries stream=r_frame_rate -of csv=p=0
//! ```
//!
//! # A subtlety worth knowing before reading a failure
//!
//! `r_frame_rate` is reported **reduced**: our table stores `qntsc` as
//! `30000/1001`, and the reference reports `30000/1001` for it too, but a table
//! entry stored unreduced (say `50/2`) would be reported as `25/1` and look
//! like a divergence when it is not. The comparison therefore reduces both
//! sides before comparing, and reports the stored form alongside so the
//! difference between "wrong value" and "different spelling" stays visible.
//!
//! # How to change it
//!
//! [`SUSPECTED`] works as in `sizes`. Do not compare against `avg_frame_rate` —
//! for a synthetic source the two agree, but for real media they legitimately
//! differ, and a check that only works on synthetic input should say so rather
//! than pick the field that happens to work.

use std::time::Duration;

use vaco_core::Rational;

use crate::extract::{Depth, FieldDivergence, TableReport};
use crate::refbin::Reference;
use crate::run::{Invocation, capture_stdout};

/// Rate names we do not have and that plausibly exist. Probed every deep run.
pub const SUSPECTED: [&str; 6] = [
    "ntsc_film",
    "pal-film",
    "film-ntsc",
    "cinema",
    "hfr",
    "drop",
];

/// Ask the oracle what a rate abbreviation resolves to.
///
/// # Errors
/// The reference rejected the name, or the probe failed.
pub fn probe_rate(reference: &Reference, name: &str) -> Result<Rational, String> {
    let inv = Invocation::new(
        &reference.ffprobe,
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:r={name}:d=0.04"),
            "-show_entries",
            "stream=r_frame_rate",
            "-of",
            "csv=p=0",
        ],
    )
    .with_timeout(Duration::from_secs(20));
    let out = capture_stdout(&inv)?;
    parse_rational(out.trim()).ok_or_else(|| format!("unparseable rate `{}`", out.trim()))
}

/// Parse `num/den` as the reference prints it.
#[must_use]
pub fn parse_rational(s: &str) -> Option<Rational> {
    let (n, d) = s.split_once('/')?;
    Some(Rational::new(
        n.trim().parse().ok()?,
        d.trim().parse().ok()?,
    ))
}

/// Compare two rationals by value, not by stored form.
#[must_use]
pub fn same_value(a: Rational, b: Rational) -> bool {
    i64::from(a.num) * i64::from(b.den) == i64::from(b.num) * i64::from(a.den)
}

/// Check our frame-rate abbreviations against the oracle.
#[must_use]
pub fn check(reference: &Reference, depth: Depth) -> TableReport {
    let mut report = TableReport {
        table: "frame-rates".to_owned(),
        oracle: format!(
            "{} -f lavfi -i color=r=<name>:d=0.04 -show_entries stream=r_frame_rate \
             -of csv=p=0",
            reference.ffprobe.display()
        ),
        ..TableReport::default()
    };
    report.notes.push(
        "ONE-DIRECTIONAL, as for frame sizes. Values are compared by ratio, not by \
         stored form, because r_frame_rate is reported reduced."
            .to_owned(),
    );
    if depth == Depth::Listings {
        report
            .notes
            .push("skipped: one process per abbreviation, so this runs under --deep".to_owned());
        return report;
    }

    let ours: Vec<&str> = vaco_core::parse::video_rate_names().collect();
    report.ours_count = ours.len();
    let mut confirmed = 0_usize;

    for name in ours {
        let Some(our_rate) = vaco_core::parse::video_rate(name) else {
            report.fields.push(FieldDivergence {
                entity: name.to_owned(),
                field: "resolvable".to_owned(),
                ours: "<listed but not resolvable>".to_owned(),
                theirs: "-".to_owned(),
            });
            continue;
        };
        match probe_rate(reference, name) {
            Ok(their_rate) => {
                confirmed += 1;
                if !same_value(our_rate, their_rate) {
                    report.fields.push(FieldDivergence {
                        entity: name.to_owned(),
                        field: "rate".to_owned(),
                        ours: format!("{}/{}", our_rate.num, our_rate.den),
                        theirs: format!("{}/{}", their_rate.num, their_rate.den),
                    });
                }
            }
            Err(_) => report.only_ours.push(name.to_owned()),
        }
    }
    report.theirs_count = confirmed;

    for candidate in SUSPECTED {
        if vaco_core::parse::video_rate(candidate).is_some() {
            continue;
        }
        if let Ok(r) = probe_rate(reference, candidate) {
            report
                .only_theirs
                .push(format!("{candidate} = {}/{}", r.num, r.den));
        }
    }
    report.fields.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::{SUSPECTED, parse_rational, same_value};
    use vaco_core::Rational;

    #[test]
    fn rationals_parse_as_the_reference_prints_them() {
        assert_eq!(
            parse_rational("30000/1001"),
            Some(Rational::new(30000, 1001))
        );
        assert_eq!(parse_rational("25/1"), Some(Rational::new(25, 1)));
        assert_eq!(parse_rational("N/A"), None);
        assert_eq!(parse_rational("25"), None);
    }

    #[test]
    fn comparison_is_by_value_not_by_stored_form() {
        assert!(same_value(Rational::new(50, 2), Rational::new(25, 1)));
        assert!(same_value(
            Rational::new(30000, 1001),
            Rational::new(30000, 1001)
        ));
        assert!(!same_value(
            Rational::new(24, 1),
            Rational::new(24000, 1001)
        ));
    }

    #[test]
    fn every_name_we_list_resolves() {
        for name in vaco_core::parse::video_rate_names() {
            assert!(
                vaco_core::parse::video_rate(name).is_some(),
                "`{name}` is listed but does not parse"
            );
        }
    }

    #[test]
    fn suspected_names_are_actually_absent_from_our_table() {
        for candidate in SUSPECTED {
            assert!(
                vaco_core::parse::video_rate(candidate).is_none(),
                "`{candidate}` is in our table now; drop it from SUSPECTED"
            );
        }
    }
}
