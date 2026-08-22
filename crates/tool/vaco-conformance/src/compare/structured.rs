//! C6 — structured diff (plan 13 §1.2).
//!
//! # What it is
//!
//! Both outputs are parsed into a section tree and diffed field by field; every
//! difference is matched against the divergence allowlist, and anything
//! unmatched fails.
//!
//! # What it is not
//!
//! C6 is **weaker than C0 by construction** and must never be used to launder a
//! failing C0 case. The manifest loader refuses to downgrade a case from C0 to
//! C6 without an allowlist entry ([`crate::manifest`]), because that is exactly
//! how a differential harness stops proving anything.
//!
//! # How it works
//!
//! [`parse_sections`] reads the `default` writer's shape — `[SECTION]`,
//! `key=value` lines, `[/SECTION]`, arbitrarily nested — which is the shape
//! `-show_pixel_formats` and the other listing outputs use. Sections are keyed
//! by `(name, ordinal)` so that a file with three streams diffs stream 0
//! against stream 0, and each carries its parent's key so a caller can
//! reassemble the tree.
//!
//! Sections come out in **closing** order, so a nested one precedes its parent.
//! That is why [`Section::parent`] exists rather than callers tracking "the most
//! recent section of path X": with nesting, the most recent one has not been
//! emitted yet.
//!
//! A section present on one side only is reported as a whole-section
//! difference, not as a field storm: the first useful sentence is "we are
//! missing `[STREAM.2]`", not two hundred field lines.
//!
//! # How to change it
//!
//! Other writers (`json`, `xml`, `ini`, `flat`, `compact`, `csv`) each need a
//! parser here. They are deliberately not stubbed: an empty parser that returns
//! no sections would make every case pass.

use std::collections::BTreeMap;

use crate::case::{Case, Verdict};
use crate::compare::{DiffReport, FieldDiff, Pair};
use crate::divergence::Allowlist;

/// A parsed section: its path, and its scalar fields in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    /// Dotted path, e.g. `PIXEL_FORMAT.COMPONENT`.
    pub path: String,
    /// Which occurrence of that path this is, zero-based.
    pub ordinal: usize,
    /// Fields in file order. A repeated key keeps the last value, matching how
    /// the writers emit.
    pub fields: BTreeMap<String, String>,
    /// The enclosing section's [`Section::key`], if there is one.
    pub parent: Option<String>,
}

impl Section {
    /// The key a section is matched by across the two sides.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}[{}]", self.path, self.ordinal)
    }
}

/// Parse the `default` writer's section syntax.
///
/// Unknown lines are ignored rather than rejected: the reference writes a
/// handful of unstructured lines (progress, warnings that escaped `-loglevel`)
/// and a parse error there would be a harness bug reported as a conformance
/// failure, which is the worst kind.
#[must_use]
pub fn parse_sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    // Open sections, innermost last. Fields land in the innermost one, and a
    // section is emitted when its closing tag arrives — so a nested section
    // never steals its parent's trailing fields.
    let mut stack: Vec<Section> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[/") && t.ends_with(']') {
            if let Some(section) = stack.pop() {
                out.push(section);
            }
            continue;
        }
        if let Some(name) = t
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .filter(|n| !n.is_empty())
        {
            let (path, parent) = match stack.last() {
                Some(parent) => (format!("{}.{name}", parent.path), Some(parent.key())),
                None => (name.to_owned(), None),
            };
            let ordinal = counts.entry(path.clone()).or_insert(0);
            stack.push(Section {
                path,
                ordinal: *ordinal,
                fields: BTreeMap::new(),
                parent,
            });
            *ordinal += 1;
            continue;
        }
        if let Some((k, v)) = t.split_once('=')
            && let Some(section) = stack.last_mut()
        {
            section
                .fields
                .insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    // An unterminated section is still evidence; emit it rather than losing it.
    while let Some(section) = stack.pop() {
        out.push(section);
    }
    out
}

/// Diff two section trees field by field, consulting the allowlist.
#[must_use]
pub fn compare(case: &Case, pair: &Pair<'_>, allow: &Allowlist, writer: &str) -> Verdict {
    let mode = case.compare.mode_name();
    if writer != "default" {
        return Verdict::Skipped(crate::case::SkipReason::ModeUnimplemented(
            "structured-diff for writers other than `default`",
        ));
    }
    let ours_text = case.normalise.apply_output(&pair.ours.stdout_text());
    let theirs_text = case.normalise.apply_output(&pair.theirs.stdout_text());
    let ours = index(&parse_sections(&ours_text));
    let theirs = index(&parse_sections(&theirs_text));
    let suite = case.id.suite();

    let mut report = DiffReport {
        mode,
        ..DiffReport::default()
    };

    for (key, theirs_section) in &theirs {
        let Some(ours_section) = ours.get(key) else {
            report.fields.push(FieldDiff {
                section: Some(theirs_section.path.clone()),
                field: "<whole section>".to_owned(),
                ours: "<missing>".to_owned(),
                theirs: format!("{} fields", theirs_section.fields.len()),
            });
            continue;
        };
        diff_fields(suite, ours_section, theirs_section, allow, &mut report);
    }
    for (key, ours_section) in &ours {
        if !theirs.contains_key(key) {
            report.fields.push(FieldDiff {
                section: Some(ours_section.path.clone()),
                field: "<whole section>".to_owned(),
                ours: format!("{} fields", ours_section.fields.len()),
                theirs: "<missing>".to_owned(),
            });
        }
    }

    if report.fields.is_empty() {
        if report.allowed.is_empty() {
            return Verdict::Agree;
        }
        report.allowed.sort();
        report.allowed.dedup();
        return Verdict::AllowedDivergence(report.allowed);
    }
    report.summary = format!("{} unexplained field difference(s)", report.fields.len());
    Verdict::Divergence(report)
}

fn diff_fields(
    suite: &str,
    ours: &Section,
    theirs: &Section,
    allow: &Allowlist,
    report: &mut DiffReport,
) {
    let section = Some(theirs.path.clone());
    let mut keys: Vec<&String> = ours.fields.keys().chain(theirs.fields.keys()).collect();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        let a = ours.fields.get(key).map(String::as_str).unwrap_or_default();
        let b = theirs
            .fields
            .get(key)
            .map(String::as_str)
            .unwrap_or_default();
        if a == b {
            continue;
        }
        if let Some(entry) = allow.match_field(suite, section.as_deref(), key, a, b) {
            report.allowed.push(entry.id.clone());
            continue;
        }
        report.fields.push(FieldDiff {
            section: section.clone(),
            field: key.clone(),
            ours: a.to_owned(),
            theirs: b.to_owned(),
        });
    }
}

fn index(sections: &[Section]) -> BTreeMap<String, Section> {
    sections.iter().map(|s| (s.key(), s.clone())).collect()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::parse_sections;
    use crate::case::{Compare, Verdict};
    use crate::compare::Pair;
    use crate::compare::tests::{case, obs};
    use crate::divergence::Allowlist;

    const SAMPLE: &str = "\
[PIXEL_FORMAT]
name=yuv420p
nb_components=3
[COMPONENT]
index=1
bit_depth=8
[/COMPONENT]
[COMPONENT]
index=2
bit_depth=8
[/COMPONENT]
[/PIXEL_FORMAT]
";

    #[test]
    fn a_nested_section_records_its_parent() {
        let s = parse_sections(SAMPLE);
        let comps: Vec<_> = s
            .iter()
            .filter(|x| x.path == "PIXEL_FORMAT.COMPONENT")
            .collect();
        assert_eq!(comps.len(), 2);
        // Sections come out in CLOSING order, so both components precede their
        // parent. Only the recorded parent key can reattach them.
        assert_eq!(comps[0].parent.as_deref(), Some("PIXEL_FORMAT[0]"));
        assert_eq!(comps[1].parent.as_deref(), Some("PIXEL_FORMAT[0]"));
        let top = s.iter().find(|x| x.path == "PIXEL_FORMAT").expect("top");
        assert_eq!(top.parent, None);
        assert!(
            s.iter().position(|x| x.path == "PIXEL_FORMAT.COMPONENT")
                < s.iter().position(|x| x.path == "PIXEL_FORMAT"),
            "closing order is the property the parent link exists to survive"
        );
    }

    #[test]
    fn nested_sections_are_parsed_with_ordinals() {
        let s = parse_sections(SAMPLE);
        let top: Vec<_> = s.iter().filter(|x| x.path == "PIXEL_FORMAT").collect();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].fields["name"], "yuv420p");
        let comps: Vec<_> = s
            .iter()
            .filter(|x| x.path == "PIXEL_FORMAT.COMPONENT")
            .collect();
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].ordinal, 0);
        assert_eq!(comps[1].fields["index"], "2");
    }

    #[test]
    fn unstructured_noise_does_not_break_the_parse() {
        let s = parse_sections("frame= 12 fps=0.0\n[FORMAT]\nformat_name=mov\n[/FORMAT]\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].fields["format_name"], "mov");
    }

    fn empty_allowlist() -> Allowlist {
        Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads")
    }

    #[test]
    fn identical_trees_agree() {
        let c = case(Compare::StructuredDiff {
            writer: "default".to_owned(),
        });
        let a = obs(SAMPLE, Some(0));
        let b = obs(SAMPLE, Some(0));
        let v = super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &empty_allowlist(),
            "default",
        );
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn a_field_difference_is_reported_with_both_values() {
        let c = case(Compare::StructuredDiff {
            writer: "default".to_owned(),
        });
        let a = obs(SAMPLE, Some(0));
        let b = obs(
            &SAMPLE.replace("nb_components=3", "nb_components=4"),
            Some(0),
        );
        match super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &empty_allowlist(),
            "default",
        ) {
            Verdict::Divergence(r) => {
                assert_eq!(r.fields.len(), 1);
                assert_eq!(r.fields[0].field, "nb_components");
                assert_eq!(r.fields[0].ours, "3");
                assert_eq!(r.fields[0].theirs, "4");
            }
            other => panic!("expected a divergence, got {}", other.label()),
        }
    }

    #[test]
    fn a_missing_section_is_one_line_not_a_field_storm() {
        let c = case(Compare::StructuredDiff {
            writer: "default".to_owned(),
        });
        let a = obs("[PIXEL_FORMAT]\nname=yuv420p\n[/PIXEL_FORMAT]\n", Some(0));
        let b = obs(SAMPLE, Some(0));
        match super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &empty_allowlist(),
            "default",
        ) {
            Verdict::Divergence(r) => {
                let whole: Vec<_> = r
                    .fields
                    .iter()
                    .filter(|f| f.field == "<whole section>")
                    .collect();
                assert_eq!(whole.len(), 2, "two COMPONENT sections are missing");
            }
            other => panic!("expected a divergence, got {}", other.label()),
        }
    }

    #[test]
    fn an_allowlisted_difference_becomes_an_allowed_verdict() {
        let register = format!(
            "schema = 1\n[caps]\nidentification = 5\n\n[[divergence]]\n\
             id = \"DIV-0001\"\ntitle = \"long name prose\"\ncategory = \"identification\"\n\
             scope = {{ suite = \"t\", section = \"PIXEL_FORMAT\", field = \"name\" }}\n\
             rule = {{ kind = \"value-differs\" }}\n\
             justification = \"{}\"\n\
             opened = 2026-08-01\nreview_by = 2099-01-01\nowner = \"@o\"\n\
             approved_by = [\"@o\"]\nissue = \"vaco#1\"\n",
            "Prose authored by the reference; we author our own and match the machine name exactly."
        );
        let allow = Allowlist::parse(&register, "2026-08-21").expect("loads");
        let c = case(Compare::StructuredDiff {
            writer: "default".to_owned(),
        });
        let a = obs("[PIXEL_FORMAT]\nname=ours\n[/PIXEL_FORMAT]\n", Some(0));
        let b = obs("[PIXEL_FORMAT]\nname=theirs\n[/PIXEL_FORMAT]\n", Some(0));
        match super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &allow,
            "default",
        ) {
            Verdict::AllowedDivergence(ids) => assert_eq!(ids.len(), 1),
            other => panic!("expected an allowed divergence, got {}", other.label()),
        }
        assert_eq!(allow.entries()[0].hits(), 1);
    }

    #[test]
    fn an_unimplemented_writer_skips_rather_than_passes() {
        let c = case(Compare::StructuredDiff {
            writer: "json".to_owned(),
        });
        let a = obs("{}", Some(0));
        let b = obs("{\"a\":1}", Some(0));
        let v = super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &empty_allowlist(),
            "json",
        );
        assert_eq!(v.label(), "skipped");
    }
}
