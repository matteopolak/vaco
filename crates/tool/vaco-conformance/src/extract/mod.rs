//! Table extractors — differential checks on our static tables.
//!
//! # What it is
//!
//! The part of the harness that is useful before a single decoder exists.
//! `vaco-pixfmt`'s 268-format table and `vaco-core`'s colour, frame-size and
//! frame-rate tables are internally consistent but, until something compares
//! them to the reference, entirely unvalidated. These extractors do that
//! comparison, and they are the first thing in the project to hold a table to
//! an external standard.
//!
//! # How it works
//!
//! Each extractor asks the oracle a question whose answer is a *fact about
//! observable behaviour* — a listing it prints, or the geometry of a frame it
//! writes — parses it, and diffs it against our table field by field. Nothing
//! is stored: the expected values are recomputed on every run (§1.7.1 point 5),
//! which is what keeps the design clean-room compatible.
//!
//! | Extractor | Oracle | Direction |
//! |---|---|---|
//! | [`pixfmt`] | `ffprobe -show_pixel_formats`, `ffmpeg -pix_fmts` | both ways |
//! | [`pixfmt::probe_plane_geometry`] | `ffmpeg -f rawvideo` output size | ours → oracle |
//! | [`colors`] | `ffmpeg -colors` | both ways |
//! | [`sizes`] | `ffprobe -f lavfi -i color=s=<name>` | **ours → oracle only** |
//! | [`rates`] | `ffprobe -f lavfi -i color=r=<name>` | **ours → oracle only** |
//!
//! The one-directional cases are a real limitation and it is stated rather than
//! papered over: the reference has no listing command for frame-size or
//! frame-rate abbreviations, so the harness can prove that every name we accept
//! means what the reference thinks it means, but it cannot enumerate names the
//! reference knows and we lack. Recovering those would mean reading the source,
//! which is the one thing §1.7.2 forbids. The `SUSPECTED` list in each module is
//! probed on every deep run instead, so "we might be missing something" is at
//! least a machine check.
//!
//! # How to change it
//!
//! Add a module, give it a `check(&Reference) -> TableReport`, and add it to
//! [`run_all`]. The rule every extractor follows: **report precisely, decide
//! nothing.** An extractor never edits a table and never suppresses a
//! difference; suppression is the allowlist's job and it is governed.

pub mod colors;
pub mod pixfmt;
pub mod rates;
pub mod sizes;

use std::fmt;

use crate::divergence::{Allowlist, DivergenceId};
use crate::refbin::Reference;

/// One field-level disagreement about one entity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldDivergence {
    /// Which entity: a pixel-format name, a colour name, an abbreviation.
    pub entity: String,
    /// Which field of it.
    pub field: String,
    /// Our value.
    pub ours: String,
    /// The reference's value.
    pub theirs: String,
}

impl fmt::Display for FieldDivergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<20} {:<16} ours={:<28} theirs={}",
            self.entity, self.field, self.ours, self.theirs
        )
    }
}

/// The outcome of one extractor.
#[derive(Debug, Clone, Default)]
pub struct TableReport {
    /// Which table was checked.
    pub table: String,
    /// The exact command the oracle was asked, so a human can re-run it.
    pub oracle: String,
    /// How many entries we have.
    pub ours_count: usize,
    /// How many entries the reference has.
    pub theirs_count: usize,
    /// Entities we have and the reference does not.
    pub only_ours: Vec<String>,
    /// Entities the reference has and we do not.
    pub only_theirs: Vec<String>,
    /// Field-level disagreements on shared entities.
    pub fields: Vec<FieldDivergence>,
    /// Disagreements an allowlist entry admitted.
    pub allowed: Vec<DivergenceId>,
    /// Anything the extractor could not check, and why.
    pub notes: Vec<String>,
    /// Set when the extractor itself failed, as distinct from finding nothing.
    pub error: Option<String>,
}

impl TableReport {
    /// A report with nothing unexplained in it.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.error.is_none()
            && self.only_ours.is_empty()
            && self.only_theirs.is_empty()
            && self.fields.is_empty()
    }

    /// Total unexplained findings.
    #[must_use]
    pub fn finding_count(&self) -> usize {
        self.only_ours.len() + self.only_theirs.len() + self.fields.len()
    }

    /// Consult `allow` for every field divergence, moving the admitted ones out
    /// of `fields` and into `allowed`.
    pub fn apply_allowlist(&mut self, allow: &Allowlist, suite: &str) {
        let mut kept = Vec::new();
        for d in std::mem::take(&mut self.fields) {
            match allow.match_field(suite, Some(&d.entity), &d.field, &d.ours, &d.theirs) {
                Some(entry) => self.allowed.push(entry.id.clone()),
                None => kept.push(d),
            }
        }
        self.fields = kept;
        self.allowed.sort();
        self.allowed.dedup();
    }
}

impl fmt::Display for TableReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "── {} ──", self.table)?;
        writeln!(f, "   oracle: {}", self.oracle)?;
        if let Some(e) = &self.error {
            writeln!(f, "   ERROR: {e}")?;
            return Ok(());
        }
        writeln!(
            f,
            "   entries: ours {}, reference {}",
            self.ours_count, self.theirs_count
        )?;
        for note in &self.notes {
            writeln!(f, "   note: {note}")?;
        }
        if !self.only_ours.is_empty() {
            writeln!(f, "   only ours ({}):", self.only_ours.len())?;
            for n in &self.only_ours {
                writeln!(f, "     + {n}")?;
            }
        }
        if !self.only_theirs.is_empty() {
            writeln!(f, "   only reference ({}):", self.only_theirs.len())?;
            for n in &self.only_theirs {
                writeln!(f, "     - {n}")?;
            }
        }
        if !self.fields.is_empty() {
            writeln!(f, "   field divergences ({}):", self.fields.len())?;
            for d in &self.fields {
                writeln!(f, "     {d}")?;
            }
        }
        if !self.allowed.is_empty() {
            let ids: Vec<&str> = self.allowed.iter().map(|d| d.0.as_str()).collect();
            writeln!(f, "   allowed: {}", ids.join(", "))?;
        }
        if self.is_clean() {
            writeln!(f, "   clean")?;
        }
        Ok(())
    }
}

/// How much work the extractors do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Listing commands only. A few hundred milliseconds.
    Listings,
    /// Listings plus the per-format and per-abbreviation probes, which spawn
    /// one process each. Tens of seconds.
    Deep,
}

/// Run every extractor.
#[must_use]
pub fn run_all(reference: &Reference, allow: &Allowlist, depth: Depth) -> Vec<TableReport> {
    let mut out = vec![
        pixfmt::check_show_pixel_formats(reference),
        pixfmt::check_pix_fmts(reference),
        colors::check(reference),
        sizes::check(reference, depth),
        rates::check(reference, depth),
    ];
    if depth == Depth::Deep {
        out.push(pixfmt::probe_plane_geometry(reference));
    }
    for report in &mut out {
        let suite = format!("table-{}", report.table);
        report.apply_allowlist(allow, &suite);
    }
    out
}

/// Sort and de-duplicate a name list so reports are stable across runs.
pub(crate) fn tidy(v: &mut Vec<String>) {
    v.sort_unstable();
    v.dedup();
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{FieldDivergence, TableReport};
    use crate::divergence::Allowlist;

    #[test]
    fn a_clean_report_says_so() {
        let r = TableReport {
            table: "pixfmt".to_owned(),
            ..TableReport::default()
        };
        assert!(r.is_clean());
        assert!(r.to_string().contains("clean"));
    }

    #[test]
    fn findings_are_counted_across_all_three_kinds() {
        let r = TableReport {
            only_ours: vec!["a".into()],
            only_theirs: vec!["b".into(), "c".into()],
            fields: vec![FieldDivergence {
                entity: "yuv420p".into(),
                field: "planes".into(),
                ours: "3".into(),
                theirs: "4".into(),
            }],
            ..TableReport::default()
        };
        assert_eq!(r.finding_count(), 4);
        assert!(!r.is_clean());
    }

    #[test]
    fn the_allowlist_moves_admitted_findings_out_of_the_failure_set() {
        let register = format!(
            "schema = 1\n[caps]\nupstream-bug = 5\n\n[[divergence]]\n\
             id = \"DIV-0009\"\ntitle = \"t\"\ncategory = \"upstream-bug\"\n\
             scope = {{ suite = \"table-pixfmt\", section = \"bayer_bggr8\", field = \"rgb\" }}\n\
             rule = {{ kind = \"value-differs\" }}\n\
             justification = \"{}\"\n\
             opened = 2026-08-01\nreview_by = 2099-01-01\nowner = \"@o\"\n\
             approved_by = [\"@a\", \"@b\"]\nissue = \"vaco#2\"\n",
            "A colour-filter-array mosaic is not an RGB layout; we model it per the sensor \
             definition and file the difference upstream."
        );
        let allow = Allowlist::parse(&register, "2026-08-21").expect("loads");
        let mut r = TableReport {
            table: "pixfmt".to_owned(),
            fields: vec![
                FieldDivergence {
                    entity: "bayer_bggr8".into(),
                    field: "rgb".into(),
                    ours: "1".into(),
                    theirs: "0".into(),
                },
                FieldDivergence {
                    entity: "yuv420p".into(),
                    field: "rgb".into(),
                    ours: "1".into(),
                    theirs: "0".into(),
                },
            ],
            ..TableReport::default()
        };
        r.apply_allowlist(&allow, "table-pixfmt");
        assert_eq!(r.fields.len(), 1, "only the scoped entity is admitted");
        assert_eq!(r.allowed.len(), 1);
        assert_eq!(r.fields.first().map(|f| f.entity.as_str()), Some("yuv420p"));
    }
}
