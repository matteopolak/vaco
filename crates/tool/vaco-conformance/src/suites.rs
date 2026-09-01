//! `suites.toml` — codec conformance-suite integration beyond `vaco-corpus`'s
//! own fetcher (work package QA-09, `#181`).
//!
//! # What it is
//!
//! `vaco-corpus` (X-05/QA-04, `#180`/`#175`) owns *fetching and storing*
//! conformance assets, addressed by content hash, in its own
//! `vaco-media.lock`. This module is the layer above that: which named
//! *suite* (a codec's Argon streams, the VP8/VP9 test vectors, `PngSuite`,
//! flac-test-files, JVT/JCT-VC) a codec's conformance run should draw from,
//! and what comparison mode (§1.2) that suite's cases should use.
//!
//! ```text
//! suites.toml           vaco-media.lock (vaco-corpus)
//!  [[suite]]              [[entry]]
//!   name = "pngsuite" ──▶  suite = "pngsuite"   (N entries, matched by name)
//!   codec = "png"
//!   mode = "raw-exact"
//! ```
//!
//! [`resolve`] joins the two: for each declared suite, every `vaco-media.lock`
//! entry whose own `suite` field matches. A suite with zero matching entries
//! is not an error — it is exactly [`ResolvedSuite::is_empty`], which lets a
//! caller report "declared but nothing fetched yet" distinctly from a typo'd
//! suite name (see `tests` below for both cases asserted).
//!
//! # What this does not do
//!
//! Turn a resolved suite into a running conformance case — that needs a
//! decoder to point at each asset and a comparison mode's full machinery
//! ([`crate::compare`]), which is per-codec work outside a generic joiner's
//! job. This module answers "what do we have, and what is it for", which is
//! the part QA-09 asks this crate specifically to add.

use vaco_corpus::{LockEntry, MediaLock};

use crate::toml::{self, Value};

/// One row of `suites.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteEntry {
    /// Matches a `vaco-media.lock` entry's own `suite` field.
    pub name: String,
    /// The codec/format this suite exercises (`"png"`, `"vp8"`, `"flac"`, …).
    pub codec: String,
    /// A comparison-mode hint (§1.2's mode names: `"raw-exact"`,
    /// `"quality-band"`, …) — advisory for whoever authors the actual cases,
    /// not itself a [`crate::case::Compare`] (constructing one needs
    /// per-case captures/tolerances this catalogue does not carry).
    pub mode: String,
    /// Free-text status, used for the two suites with no fetchable entries
    /// yet (Argon, JVT/JCT-VC — see `vaco-corpus`'s own `vaco-media.lock`
    /// header for why).
    pub note: String,
}

#[derive(Debug)]
pub enum SuitesError {
    Parse(toml::TomlError),
    MissingField { index: usize, field: &'static str },
}

impl std::fmt::Display for SuitesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "suites.toml: {e}"),
            Self::MissingField { index, field } => {
                write!(f, "suites.toml: suite #{index} is missing `{field}`")
            }
        }
    }
}

impl std::error::Error for SuitesError {}

/// `suites.toml`, embedded at compile time from this crate's own directory —
/// the same pattern `vaco-corpus` uses for its `vaco-media.lock`, so neither
/// crate has to guess a path relative to wherever the binary runs from.
pub const EMBEDDED_SUITES: &str = include_str!("../suites.toml");

/// Parse a `suites.toml` document.
///
/// # Errors
/// A syntax error, or a `[[suite]]` missing `name`/`codec`/`mode`.
pub fn parse(src: &str) -> Result<Vec<SuiteEntry>, SuitesError> {
    let root = toml::parse(src).map_err(SuitesError::Parse)?;
    let rows = root
        .get("suite")
        .and_then(Value::as_array)
        .unwrap_or(&[]);

    let mut out = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        let table = row.as_table().ok_or(SuitesError::MissingField {
            index: idx,
            field: "(not a table)",
        })?;
        let name = field_str(table, idx, "name")?;
        let codec = field_str(table, idx, "codec")?;
        let mode = field_str(table, idx, "mode")?;
        let note = table
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        out.push(SuiteEntry { name, codec, mode, note });
    }
    Ok(out)
}

fn field_str(table: &toml::Table, index: usize, field: &'static str) -> Result<String, SuitesError> {
    table
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(SuitesError::MissingField { index, field })
}

/// Parse this crate's own embedded `suites.toml`.
///
/// # Panics
/// Never in practice — this is this crate's own committed data, parsed by
/// `tests` below on every run; a failure here means a broken release, not
/// caller error. Same reasoning as `vaco_corpus::embedded_catalogue`.
#[expect(
    clippy::expect_used,
    reason = "the embedded catalogue is this crate's own committed data, not caller input"
)]
#[must_use]
pub fn embedded_suites() -> Vec<SuiteEntry> {
    parse(EMBEDDED_SUITES).expect("vaco-conformance's own suites.toml must parse")
}

/// A declared suite joined against `vaco-media.lock`'s matching entries.
#[derive(Debug, Clone)]
pub struct ResolvedSuite<'a> {
    pub suite: SuiteEntry,
    pub entries: Vec<&'a LockEntry>,
}

impl ResolvedSuite<'_> {
    /// True when nothing in `vaco-media.lock` is tagged with this suite's
    /// name yet — a real, expected state for a suite that is declared but
    /// not yet backed by any corpus entry (Argon, JVT/JCT-VC today), not a
    /// parse error.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many of this suite's entries are actually fetchable today (have
    /// a url + hash on record), versus documented gaps.
    #[must_use]
    pub fn fetchable_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_fetchable()).count()
    }
}

/// Join `suites` against every entry in `lock` by matching
/// `SuiteEntry::name` to `LockEntry::suite`.
#[must_use]
pub fn resolve<'a>(suites: &[SuiteEntry], lock: &'a MediaLock) -> Vec<ResolvedSuite<'a>> {
    suites
        .iter()
        .map(|suite| ResolvedSuite {
            suite: suite.clone(),
            entries: lock.suite(&suite.name).collect(),
        })
        .collect()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{SuitesError, embedded_suites, parse, resolve};
    use vaco_corpus::embedded_catalogue;

    #[test]
    fn the_embedded_suites_file_parses_and_is_non_empty() {
        let suites = embedded_suites();
        assert!(!suites.is_empty());
    }

    #[test]
    fn every_suite_names_a_codec_and_a_mode() {
        for s in embedded_suites() {
            assert!(!s.codec.is_empty(), "{}: empty codec", s.name);
            assert!(!s.mode.is_empty(), "{}: empty mode", s.name);
        }
    }

    #[test]
    fn resolving_against_the_real_corpus_lock_finds_the_fetchable_suites() {
        let suites = embedded_suites();
        let lock = embedded_catalogue();
        let resolved = resolve(&suites, &lock);

        let pngsuite = resolved
            .iter()
            .find(|r| r.suite.name == "pngsuite")
            .expect("suites.toml declares pngsuite");
        assert!(!pngsuite.is_empty());
        assert!(pngsuite.fetchable_count() >= 1);
    }

    #[test]
    fn a_declared_suite_with_no_fetchable_entries_is_empty_not_an_error() {
        let suites = embedded_suites();
        let lock = embedded_catalogue();
        let resolved = resolve(&suites, &lock);

        let argon = resolved
            .iter()
            .find(|r| r.suite.name == "argon")
            .expect("suites.toml declares argon");
        // argon has a documented-gap row in vaco-media.lock (suite = "argon",
        // no url/sha256), so it resolves to a non-empty entry list whose
        // fetchable count is zero — the honest "declared, not sourced yet"
        // state this module's docs describe.
        assert!(!argon.is_empty());
        assert_eq!(argon.fetchable_count(), 0);
    }

    #[test]
    fn jvt_h264_and_jctvc_resolve_to_real_fetchable_entries() {
        // These were both `argon`-shaped documented gaps until 2026-09-01 --
        // see vaco-corpus's own `jvt_h264_and_jctvc_are_no_longer_documented_gaps`
        // for the corpus-side half of this; this is the suite-catalogue half.
        let suites = embedded_suites();
        let lock = embedded_catalogue();
        let resolved = resolve(&suites, &lock);
        for name in ["jvt-h264", "jctvc"] {
            let suite = resolved
                .iter()
                .find(|r| r.suite.name == name)
                .expect("suites.toml declares this suite");
            assert!(!suite.is_empty(), "{name}: expected entries");
            assert_eq!(
                suite.fetchable_count(),
                suite.entries.len(),
                "{name}: every registered entry should be fetchable"
            );
        }
    }

    #[test]
    fn missing_field_is_a_named_error() {
        let err = parse("[[suite]]\ncodec = \"x\"\nmode = \"raw-exact\"\n").unwrap_err();
        assert!(matches!(err, SuitesError::MissingField { .. }));
    }

    #[test]
    fn an_unknown_suite_name_resolves_empty_and_ungated() {
        let suites = vec![super::SuiteEntry {
            name: "does-not-exist-anywhere".to_owned(),
            codec: "x".to_owned(),
            mode: "raw-exact".to_owned(),
            note: String::new(),
        }];
        let lock = embedded_catalogue();
        let resolved = resolve(&suites, &lock);
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].is_empty());
    }
}
