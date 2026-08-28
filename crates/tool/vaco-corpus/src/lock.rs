//! `vaco-media.lock` — the manifest of every corpus asset this project knows
//! about, addressed by content hash.
//!
//! # What it is
//!
//! One `[[entry]]` per asset: where to fetch it, what it must hash to, how
//! big it is, its licence, and which fuzz/conformance targets consume it.
//! Plan 13 §2.5.2's example shape, adapted from BLAKE3 to the SHA-256
//! [`crate::store::ObjectId`] this crate actually uses (see `store.rs`'s
//! module docs for why).
//!
//! # How it works
//!
//! [`MediaLock::parse`]/[`MediaLock::render`] round-trip through
//! [`crate::toml_min`]. Nothing here touches the network or the object
//! store — this module is pure data plus (de)serialisation, which is what
//! makes it unit-testable without a filesystem or a socket.
//!
//! # How to change it
//!
//! Add a field to [`LockEntry`], then to both `parse` (reading it out of the
//! table) and `render` (writing it back) in the same commit — an asymmetric
//! change here is a lock file that silently drops a field on the next
//! rewrite.

use std::fmt::Write as _;

use crate::store::ObjectId;
use crate::toml_min::{self, TomlValue};

/// One asset the corpus knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// A short, stable name used to look the entry up (`"pngsuite-basn0g01"`).
    pub name: String,
    /// The suite this asset belongs to (`"pngsuite"`, `"vp8-test-vectors"`, …).
    pub suite: String,
    /// Canonical upstream URL. `None` for an entry that is documented but not
    /// yet fetchable (see `suites.toml`'s Argon/JCT-VC rows).
    pub url: Option<String>,
    /// Expected SHA-256, verified on every fetch.
    pub sha256: Option<ObjectId>,
    /// Size in bytes, as an independent sanity check ahead of the hash.
    pub size: Option<u64>,
    /// SPDX-ish licence tag, free text (these are third-party assets with
    /// their own licensing, not code we distribute).
    pub license: String,
    /// Where this entry's own facts (URL, hash, size) were recorded from.
    pub source: String,
    /// Fuzz targets / conformance suites this asset feeds.
    pub targets: Vec<String>,
}

impl LockEntry {
    fn from_table(table: &toml_min::Table) -> Option<Self> {
        let name = table.get("name")?.as_str()?.to_owned();
        let suite = table.get("suite")?.as_str()?.to_owned();
        let url = table.get("url").and_then(TomlValue::as_str).map(str::to_owned);
        let sha256 = table
            .get("sha256")
            .and_then(TomlValue::as_str)
            .and_then(ObjectId::parse);
        let size = table
            .get("size")
            .and_then(TomlValue::as_integer)
            .and_then(|n| u64::try_from(n).ok());
        let license = table
            .get("license")
            .and_then(TomlValue::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let source = table
            .get("source")
            .and_then(TomlValue::as_str)
            .unwrap_or("")
            .to_owned();
        let targets = table
            .get("targets")
            .and_then(TomlValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(TomlValue::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            name,
            suite,
            url,
            sha256,
            size,
            license,
            source,
            targets,
        })
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "[[entry]]");
        let _ = writeln!(out, "name = {}", toml_min::quote(&self.name));
        let _ = writeln!(out, "suite = {}", toml_min::quote(&self.suite));
        if let Some(url) = &self.url {
            let _ = writeln!(out, "url = {}", toml_min::quote(url));
        }
        if let Some(sha) = &self.sha256 {
            let _ = writeln!(out, "sha256 = {}", toml_min::quote(sha.as_str()));
        }
        if let Some(size) = self.size {
            let _ = writeln!(out, "size = {size}");
        }
        let _ = writeln!(out, "license = {}", toml_min::quote(&self.license));
        let _ = writeln!(out, "source = {}", toml_min::quote(&self.source));
        let _ = writeln!(out, "targets = {}", toml_min::quote_array(&self.targets));
        out.push('\n');
    }

    /// Whether this entry names something that can actually be fetched today.
    #[must_use]
    pub fn is_fetchable(&self) -> bool {
        self.url.is_some() && self.sha256.is_some()
    }
}

/// The parsed `vaco-media.lock`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaLock {
    pub schema: i64,
    pub entries: Vec<LockEntry>,
}

#[derive(Debug)]
pub enum LockError {
    Parse(toml_min::TomlError),
    MissingField { entry_index: usize, field: &'static str },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "vaco-media.lock: {e}"),
            Self::MissingField { entry_index, field } => write!(
                f,
                "vaco-media.lock: entry #{entry_index} is missing required field `{field}`"
            ),
        }
    }
}

impl std::error::Error for LockError {}

impl MediaLock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: 1,
            entries: Vec::new(),
        }
    }

    /// Parse a `vaco-media.lock` document.
    ///
    /// # Errors
    /// A syntax error, or an `[[entry]]` missing `name`/`suite`.
    pub fn parse(src: &str) -> Result<Self, LockError> {
        let doc = toml_min::parse(src).map_err(LockError::Parse)?;
        let schema = doc.top.get("schema").and_then(TomlValue::as_integer).unwrap_or(1);
        let mut entries = Vec::new();
        for (idx, table) in doc.section("entry").iter().enumerate() {
            let entry = LockEntry::from_table(table).ok_or(LockError::MissingField {
                entry_index: idx,
                field: "name/suite",
            })?;
            entries.push(entry);
        }
        Ok(Self { schema, entries })
    }

    /// Render back to the on-disk format. `parse(&lock.render())` round-trips.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "schema = {}", self.schema);
        out.push('\n');
        for entry in &self.entries {
            entry.render(&mut out);
        }
        out
    }

    /// Look an entry up by its short name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&LockEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Every entry belonging to one suite.
    pub fn suite(&self, suite: &str) -> impl Iterator<Item = &LockEntry> {
        self.entries.iter().filter(move |e| e.suite == suite)
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{LockEntry, MediaLock};
    use crate::store::ObjectId;

    fn sample() -> MediaLock {
        MediaLock {
            schema: 1,
            entries: vec![
                LockEntry {
                    name: "pngsuite-basn0g01".to_owned(),
                    suite: "pngsuite".to_owned(),
                    url: Some("http://www.schaik.com/pngsuite/basn0g01.png".to_owned()),
                    sha256: ObjectId::parse(
                        "c8b1364d7771dd2f5a1b2d7d633abcf3f48dafee608558ecd2e5fc98f61894cd",
                    ),
                    size: Some(164),
                    license: "public domain (PngSuite)".to_owned(),
                    source: "canonical upstream, probed 2026-08-28".to_owned(),
                    targets: vec!["png_decode".to_owned()],
                },
                LockEntry {
                    name: "jctvc-not-yet-sourced".to_owned(),
                    suite: "jctvc".to_owned(),
                    url: None,
                    sha256: None,
                    size: None,
                    license: "unknown".to_owned(),
                    source: "documented gap; no stable public single-file mirror found".to_owned(),
                    targets: vec![],
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_render_and_parse() {
        let lock = sample();
        let rendered = lock.render();
        let parsed = MediaLock::parse(&rendered).expect("parses");
        assert_eq!(parsed, lock);
    }

    #[test]
    fn find_and_suite_filter() {
        let lock = sample();
        assert!(lock.find("pngsuite-basn0g01").is_some());
        assert!(lock.find("does-not-exist").is_none());
        assert_eq!(lock.suite("pngsuite").count(), 1);
    }

    #[test]
    fn is_fetchable_distinguishes_documented_gaps() {
        let lock = sample();
        assert!(lock.find("pngsuite-basn0g01").expect("present").is_fetchable());
        assert!(!lock.find("jctvc-not-yet-sourced").expect("present").is_fetchable());
    }

    #[test]
    fn missing_required_field_is_an_error() {
        let err = MediaLock::parse("[[entry]]\nsuite = \"x\"\n").unwrap_err();
        assert!(matches!(err, super::LockError::MissingField { .. }));
    }

    #[test]
    fn empty_document_is_a_valid_empty_lock() {
        let lock = MediaLock::parse("schema = 1\n").expect("parses");
        assert_eq!(lock.entries.len(), 0);
    }
}
