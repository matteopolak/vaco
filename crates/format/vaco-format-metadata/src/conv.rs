//! `MetadataConv`: one container's key-name table, and the driver that
//! applies it.
//!
//! A container's native tag vocabulary rarely matches the canonical
//! [`crate::keys`] spelling — `ID3v2` calls the title frame `TIT2`, `QuickTime`
//! calls it `©nam` — and the reference reads and writes both directions:
//! `-metadata title=…` written into an MP3 becomes a `TIT2` frame, and
//! reading that file back reports `title` again. `MetadataConv` is the table
//! a container states that mapping with; this module is only the shape of
//! the table and the two-way lookup, not any particular container's rows —
//! see the crate doc for why those stay in the container's own crate.

use std::borrow::Cow;

/// One `(generic, native)` pair.
///
/// Case-insensitive on both sides, matching the reference's own tag lookup —
/// measured on `ID3v2` (`TIT2` is always upper-case in the spec, but a reader
/// tolerant of case is cheaper to write than fragile) and on Vorbis comment
/// field names, which RFC-defined "case-insensitive ASCII" outright.
#[derive(Debug, Clone, Copy)]
pub struct ConvEntry {
    pub generic: &'static str,
    pub native: &'static str,
}

/// A container's whole key-name table.
///
/// A newtype around a static slice rather than a `HashMap`: every table this
/// project will ever build is small (a few dozen rows at most) and known at
/// compile time, so a linear scan is both simpler and — for a table this
/// size — not measurably slower than hashing.
#[derive(Debug, Clone, Copy)]
pub struct MetadataConv(pub &'static [ConvEntry]);

/// Which way [`MetadataConv::convert`] maps a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Canonical (`title`) to container-native (`TIT2`).
    ToNative,
    /// Container-native to canonical.
    ToGeneric,
}

impl MetadataConv {
    /// An empty table: every key passes through unmapped.
    pub const EMPTY: Self = Self(&[]);

    /// The native spelling for a canonical key, or `None` if this table does
    /// not name one.
    #[must_use]
    pub fn to_native(&self, generic: &str) -> Option<&'static str> {
        self.0
            .iter()
            .find(|e| e.generic.eq_ignore_ascii_case(generic))
            .map(|e| e.native)
    }

    /// The canonical key for a native spelling, or `None` if this table does
    /// not name one.
    #[must_use]
    pub fn to_generic(&self, native: &str) -> Option<&'static str> {
        self.0
            .iter()
            .find(|e| e.native.eq_ignore_ascii_case(native))
            .map(|e| e.generic)
    }

    /// Map one key through the table in `direction`, unchanged if the table
    /// does not name it.
    ///
    /// **An unmapped key passes through verbatim.** That is the reference's
    /// own behaviour — `-metadata some_made_up_key=x` survives a remux
    /// unchanged in every container measured — and it is what makes this
    /// driver additive: a table growing a new row can only start mapping a
    /// key that used to pass through, never break one that already had
    /// nowhere to go.
    #[must_use]
    pub fn map_key<'a>(&self, key: &'a str, direction: Direction) -> Cow<'a, str> {
        let mapped = match direction {
            Direction::ToNative => self.to_native(key),
            Direction::ToGeneric => self.to_generic(key),
        };
        match mapped {
            Some(m) => Cow::Borrowed(m),
            None => Cow::Borrowed(key),
        }
    }

    /// Map every key in `entries` through the table in `direction`, values
    /// untouched. Order is preserved and duplicate keys are not merged,
    /// matching [`vaco_format_core::Chapter::metadata`]'s own convention.
    #[must_use]
    pub fn convert(&self, entries: &[(String, String)], direction: Direction) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(k, v)| (self.map_key(k, direction).into_owned(), v.clone()))
            .collect()
    }
}

impl Default for MetadataConv {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: MetadataConv = MetadataConv(&[
        ConvEntry {
            generic: "title",
            native: "TIT2",
        },
        ConvEntry {
            generic: "artist",
            native: "TPE1",
        },
    ]);

    #[test]
    fn maps_both_directions() {
        assert_eq!(TABLE.to_native("title"), Some("TIT2"));
        assert_eq!(TABLE.to_native("TITLE"), Some("TIT2"), "case-insensitive");
        assert_eq!(TABLE.to_generic("TIT2"), Some("title"));
        assert_eq!(TABLE.to_generic("tit2"), Some("title"), "case-insensitive");
    }

    #[test]
    fn an_unmapped_key_passes_through() {
        assert_eq!(TABLE.to_native("comment"), None);
        assert_eq!(
            TABLE.map_key("comment", Direction::ToNative),
            Cow::Borrowed("comment")
        );
    }

    #[test]
    fn convert_maps_keys_and_leaves_values_and_order() {
        let entries = vec![
            ("title".to_owned(), "T".to_owned()),
            ("comment".to_owned(), "C".to_owned()),
            ("artist".to_owned(), "A".to_owned()),
        ];
        let native = TABLE.convert(&entries, Direction::ToNative);
        assert_eq!(
            native,
            vec![
                ("TIT2".to_owned(), "T".to_owned()),
                ("comment".to_owned(), "C".to_owned()),
                ("TPE1".to_owned(), "A".to_owned()),
            ]
        );
        let back = TABLE.convert(&native, Direction::ToGeneric);
        assert_eq!(back, entries);
    }

    #[test]
    fn empty_table_is_the_identity() {
        let entries = vec![("x".to_owned(), "y".to_owned())];
        assert_eq!(
            MetadataConv::EMPTY.convert(&entries, Direction::ToNative),
            entries
        );
    }
}
