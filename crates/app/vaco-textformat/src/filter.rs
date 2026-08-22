//! The `-show_entries` filter.
//!
//! Grammar (ffprobe(1)):
//!
//! ```text
//! SECTION_ENTRIES ::= SECTION_ENTRY[:SECTION_ENTRIES]
//! SECTION_ENTRY   ::= SECTION_NAME[=[FIELD[,FIELD…]]]
//! ```
//!
//! Three observed behaviours that the grammar does not spell out:
//!
//! * A name matches a section's **local** name as well as its unique name, and
//!   a local-name match selects *every* section carrying it. This is why
//!   `-show_entries stream=index` also opens the (empty) `programs` and
//!   `stream_groups` arrays: `stream` is the local name of `program_stream` and
//!   `stream_group_stream` too.
//! * `SECTION=` with an empty field list prints **nothing at all** for that
//!   section, not a bare header. (`planning/research/05-fftools-cli.md` §3.4
//!   says otherwise; it is wrong — see its correction header.)
//! * A section that is merely an *ancestor* of a selected one is still opened,
//!   with no fields of its own — which is where `[streams.stream.0]` with no
//!   entries, and the `xml` writer's `<stream >`, come from.

use crate::sections::{SECTIONS, SectionDesc, SectionId, desc};

/// One `SECTION[=FIELDS]` clause.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Entry {
    name: String,
    /// [`None`] for a bare section name: everything, recursively.
    fields: Option<Vec<String>>,
}

/// A parsed `-show_entries` argument, or "everything".
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EntryFilterSet {
    entries: Option<Vec<Entry>>,
}

impl EntryFilterSet {
    /// No filter: every section and every field the caller emits is printed.
    #[must_use]
    pub fn all() -> Self {
        Self { entries: None }
    }

    /// Whether any clause was given.
    #[must_use]
    pub fn is_unfiltered(&self) -> bool {
        self.entries.is_none()
    }

    /// Parse a `-show_entries` argument.
    ///
    /// Unknown section names are kept rather than rejected: ffprobe accepts
    /// them silently and simply never matches, and rejecting would make the
    /// filter's behaviour depend on our schema being complete.
    #[must_use]
    pub fn parse(spec: &str) -> Self {
        let mut entries = Vec::new();
        for clause in spec.split(':') {
            if clause.is_empty() {
                continue;
            }
            let entry = match clause.split_once('=') {
                None => Entry {
                    name: clause.to_owned(),
                    fields: None,
                },
                Some((name, list)) => Entry {
                    name: name.to_owned(),
                    fields: Some(
                        list.split(',')
                            .filter(|f| !f.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    ),
                },
            };
            entries.push(entry);
        }
        Self {
            entries: Some(entries),
        }
    }

    /// What the filter says about one section directly.
    fn direct(&self, d: &SectionDesc) -> Match<'_> {
        let Some(entries) = &self.entries else {
            return Match::Everything;
        };
        let mut fields: Vec<&str> = Vec::new();
        let mut seen = false;
        for e in entries {
            if e.name != d.name && e.name != d.unique_name {
                continue;
            }
            seen = true;
            match &e.fields {
                None => return Match::Everything,
                Some(list) => fields.extend(list.iter().map(String::as_str)),
            }
        }
        if !seen || fields.is_empty() {
            Match::No
        } else {
            Match::Fields(fields)
        }
    }

    /// Whether an ancestor selected the whole subtree.
    fn ancestor_selects_all(&self, stack: &[&'static SectionDesc]) -> bool {
        stack
            .iter()
            .any(|a| matches!(self.direct(a), Match::Everything))
    }

    /// Whether the section should be opened at all.
    ///
    /// `stack` is the enclosing sections, root first, **not** including `d`.
    #[must_use]
    pub fn section_visible(&self, stack: &[&'static SectionDesc], d: &'static SectionDesc) -> bool {
        if self.entries.is_none() {
            return true;
        }
        if self.ancestor_selects_all(stack) {
            return true;
        }
        if !matches!(self.direct(d), Match::No) {
            return true;
        }
        // Structural: keep the section if anything under it is wanted.
        has_selected_descendant(self, d, &mut vec![false; SECTIONS.len()])
    }

    /// Whether a field of the current section should be printed.
    ///
    /// `stack` includes `d` itself as its last entry.
    #[must_use]
    pub fn field_visible(
        &self,
        stack: &[&'static SectionDesc],
        d: &'static SectionDesc,
        key: &str,
    ) -> bool {
        if self.entries.is_none() {
            return true;
        }
        if self.ancestor_selects_all(stack) {
            return true;
        }
        match self.direct(d) {
            Match::Everything => true,
            Match::Fields(f) => f.contains(&key),
            Match::No => false,
        }
    }
}

enum Match<'a> {
    /// Not mentioned, or mentioned with an empty field list.
    No,
    /// Mentioned by bare name: this section and everything under it.
    Everything,
    /// Mentioned with an explicit field list.
    Fields(Vec<&'a str>),
}

fn has_selected_descendant(f: &EntryFilterSet, d: &SectionDesc, seen: &mut [bool]) -> bool {
    for child in d.children {
        let Some(slot) = seen.get_mut(child.0 as usize) else {
            continue;
        };
        if core::mem::replace(slot, true) {
            continue;
        }
        let c = desc(*child);
        if !matches!(f.direct(c), Match::No) || has_selected_descendant(f, c, seen) {
            return true;
        }
    }
    false
}

/// The root section every document starts from.
#[must_use]
pub fn root() -> &'static SectionDesc {
    desc(SectionId::ROOT)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use crate::sections::desc;

    fn d(id: SectionId) -> &'static SectionDesc {
        desc(id)
    }

    #[test]
    fn unfiltered_shows_everything() {
        let f = EntryFilterSet::all();
        assert!(f.section_visible(&[], d(SectionId::STREAM)));
        assert!(f.field_visible(&[d(SectionId::STREAM)], d(SectionId::STREAM), "index"));
    }

    #[test]
    fn field_list_restricts_fields_but_keeps_the_section() {
        let f = EntryFilterSet::parse("stream=index");
        let stack = [
            d(SectionId::ROOT),
            d(SectionId::STREAMS),
            d(SectionId::STREAM),
        ];
        assert!(f.section_visible(&stack[..2], d(SectionId::STREAM)));
        assert!(f.field_visible(&stack, d(SectionId::STREAM), "index"));
        assert!(!f.field_visible(&stack, d(SectionId::STREAM), "codec_name"));
    }

    #[test]
    fn local_name_match_selects_every_section_with_that_name() {
        let f = EntryFilterSet::parse("stream=index");
        // `programs` is kept because `program_stream`'s local name is `stream`.
        assert!(f.section_visible(&[d(SectionId::ROOT)], d(SectionId::PROGRAMS)));
        assert!(f.section_visible(&[d(SectionId::ROOT)], d(SectionId::STREAM_GROUPS)));
    }

    #[test]
    fn unique_name_match_selects_only_one() {
        let f = EntryFilterSet::parse("stream_tags=NASTY");
        assert!(!f.section_visible(&[d(SectionId::ROOT)], d(SectionId::PROGRAMS)));
        assert!(f.section_visible(&[d(SectionId::ROOT)], d(SectionId::STREAMS)));
    }

    #[test]
    fn ancestors_of_a_selection_are_opened_without_fields() {
        let f = EntryFilterSet::parse("stream_tags=NASTY");
        let stack = [d(SectionId::ROOT), d(SectionId::STREAMS)];
        assert!(f.section_visible(&stack, d(SectionId::STREAM)));
        let full = [
            d(SectionId::ROOT),
            d(SectionId::STREAMS),
            d(SectionId::STREAM),
        ];
        assert!(!f.field_visible(&full, d(SectionId::STREAM), "index"));
    }

    #[test]
    fn empty_field_list_hides_the_section_entirely() {
        let f = EntryFilterSet::parse("stream=");
        assert!(!f.section_visible(&[d(SectionId::ROOT)], d(SectionId::STREAMS)));
        assert!(!f.section_visible(
            &[d(SectionId::ROOT), d(SectionId::STREAMS)],
            d(SectionId::STREAM)
        ));
    }

    #[test]
    fn bare_name_takes_the_whole_subtree() {
        let f = EntryFilterSet::parse("stream");
        let stack = [
            d(SectionId::ROOT),
            d(SectionId::STREAMS),
            d(SectionId::STREAM),
        ];
        assert!(f.field_visible(&stack, d(SectionId::STREAM), "anything"));
        assert!(f.section_visible(&stack, d(SectionId::STREAM_TAGS)));
        let deeper = [
            d(SectionId::ROOT),
            d(SectionId::STREAMS),
            d(SectionId::STREAM),
            d(SectionId::STREAM_TAGS),
        ];
        assert!(f.field_visible(&deeper, d(SectionId::STREAM_TAGS), "LANGUAGE"));
    }

    #[test]
    fn several_clauses_merge() {
        let f = EntryFilterSet::parse("format=size:program_version=version");
        assert!(f.section_visible(&[d(SectionId::ROOT)], d(SectionId::FORMAT)));
        assert!(f.section_visible(&[d(SectionId::ROOT)], d(SectionId::PROGRAM_VERSION)));
        assert!(!f.section_visible(&[d(SectionId::ROOT)], d(SectionId::PACKETS)));
    }

    #[test]
    fn root_is_always_visible() {
        let f = EntryFilterSet::parse("format=size");
        assert!(f.section_visible(&[], root()));
    }
}
