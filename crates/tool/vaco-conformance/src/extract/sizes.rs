//! Differential check of `vaco_core::parse::image_size`'s abbreviations.
//!
//! # What it is
//!
//! The reference has **no listing command** for frame-size abbreviations, so
//! the oracle has to be interrogated behaviourally: ask it to build a source of
//! that size and read back the geometry it chose.
//!
//! ```text
//! ffprobe -f lavfi -i color=s=<name>:d=0.04 -show_entries stream=width,height -of csv=p=0
//! ```
//!
//! That is black-box observation of a shipped binary (§1.7.1) and it is the
//! sanctioned way to interrogate the oracle (§1.7.3 step 3).
//!
//! # The limitation, stated plainly
//!
//! This check is **one-directional**. It proves that every name we accept means
//! to the reference what it means to us. It cannot enumerate names the
//! reference knows and we lack, because there is nothing to enumerate from —
//! and recovering them by reading `libavutil`'s parser is the one thing the
//! bright-line rule forbids.
//!
//! The mitigation, which is not a substitute for a listing: [`SUSPECTED`]
//! carries names that plausibly exist and that we do not have, and the
//! extractor probes every one of them and reports any the reference accepts.
//! That turns "we might be missing something" into a machine check, at the cost
//! of one process per candidate. A contributor who learns of a name from the
//! reference's *user documentation* (Tier A material, §1.7.3 step 4) adds it
//! there.
//!
//! # How to change it
//!
//! Add to [`SUSPECTED`] when a candidate comes up. Do not add a name to
//! `vaco-core`'s table on the strength of this check alone — that is the
//! table owner's decision, and this crate reports, it does not edit.

use std::time::Duration;

use crate::extract::{Depth, FieldDivergence, TableReport};
use crate::refbin::Reference;
use crate::run::{Invocation, capture_stdout};

/// Names we do not have and that plausibly exist. Probed every deep run.
///
/// Sourced from general video-industry vocabulary, not from the reference's
/// source. Each is either confirmed absent (and stays here as a negative
/// assertion) or found present (and is reported as a gap in our table).
pub const SUSPECTED: [&str; 12] = [
    "qsif", "sif", "4sif", "16sif", "hd2160", "uhd", "8k", "dci2k", "dci4k", "sxga-", "wsvga",
    "fhd",
];

/// Ask the oracle what `expr` resolves to, as `width,height` or `num/den`.
///
/// `d=0.04` keeps the synthesised source to a single frame's worth of work; the
/// probe is about the header, not the pixels.
fn probe(reference: &Reference, option: &str, name: &str, entries: &str) -> Result<String, String> {
    let inv = Invocation::new(
        &reference.ffprobe,
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:{option}={name}:d=0.04"),
            "-show_entries",
            entries,
            "-of",
            "csv=p=0",
        ],
    )
    .with_timeout(Duration::from_secs(20));
    Ok(capture_stdout(&inv)?.trim().to_owned())
}

/// Ask the oracle what a size abbreviation resolves to.
///
/// # Errors
/// The reference rejected the name, or the probe failed.
pub fn probe_size(reference: &Reference, name: &str) -> Result<(u32, u32), String> {
    let out = probe(reference, "s", name, "stream=width,height")?;
    let (w, h) = out
        .split_once(',')
        .ok_or_else(|| format!("unparseable geometry `{out}`"))?;
    Ok((
        w.trim().parse().map_err(|_| format!("bad width `{w}`"))?,
        h.trim().parse().map_err(|_| format!("bad height `{h}`"))?,
    ))
}

/// Check our frame-size abbreviations against the oracle.
#[must_use]
pub fn check(reference: &Reference, depth: Depth) -> TableReport {
    let mut report = TableReport {
        table: "frame-sizes".to_owned(),
        oracle: format!(
            "{} -f lavfi -i color=s=<name>:d=0.04 -show_entries stream=width,height \
             -of csv=p=0",
            reference.ffprobe.display()
        ),
        ..TableReport::default()
    };
    report.notes.push(
        "ONE-DIRECTIONAL: the reference exposes no listing of size abbreviations, \
         so names it knows and we lack cannot be enumerated; the SUSPECTED \
         candidate list is probed instead."
            .to_owned(),
    );
    if depth == Depth::Listings {
        report
            .notes
            .push("skipped: one process per abbreviation, so this runs under --deep".to_owned());
        return report;
    }

    let ours: Vec<&str> = vaco_core::parse::image_size_names().collect();
    report.ours_count = ours.len();
    let mut confirmed = 0_usize;

    for name in ours {
        let Some((our_w, our_h)) = vaco_core::parse::image_size(name) else {
            report.fields.push(FieldDivergence {
                entity: name.to_owned(),
                field: "resolvable".to_owned(),
                ours: "<listed but not resolvable>".to_owned(),
                theirs: "-".to_owned(),
            });
            continue;
        };
        match probe_size(reference, name) {
            Ok((w, h)) => {
                confirmed += 1;
                if (w, h) != (our_w, our_h) {
                    report.fields.push(FieldDivergence {
                        entity: name.to_owned(),
                        field: "size".to_owned(),
                        ours: format!("{our_w}x{our_h}"),
                        theirs: format!("{w}x{h}"),
                    });
                }
            }
            Err(_) => report.only_ours.push(name.to_owned()),
        }
    }
    report.theirs_count = confirmed;

    for candidate in SUSPECTED {
        if vaco_core::parse::image_size(candidate).is_some() {
            continue;
        }
        if let Ok((w, h)) = probe_size(reference, candidate) {
            report.only_theirs.push(format!("{candidate} = {w}x{h}"));
        }
    }
    report.fields.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::SUSPECTED;

    #[test]
    fn every_name_we_list_resolves() {
        for name in vaco_core::parse::image_size_names() {
            assert!(
                vaco_core::parse::image_size(name).is_some(),
                "`{name}` is listed but does not parse; the extractor would report \
                 a phantom divergence"
            );
        }
    }

    #[test]
    fn suspected_names_are_actually_absent_from_our_table() {
        // If one of these lands in `vaco-core`, remove it here — probing for a
        // name we already have wastes a process and reports nothing.
        for candidate in SUSPECTED {
            assert!(
                vaco_core::parse::image_size(candidate).is_none(),
                "`{candidate}` is in our table now; drop it from SUSPECTED"
            );
        }
    }

    #[test]
    fn our_table_is_non_trivial() {
        let n = vaco_core::parse::image_size_names().count();
        assert!(n > 30, "only {n} abbreviations");
    }
}
