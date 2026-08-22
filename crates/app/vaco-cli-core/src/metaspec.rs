//! Metadata specifiers — a *different* grammar occupying the same syntactic
//! slot as a stream specifier.
//!
//! `-metadata:s:v:0 title=x` and `-c:v:0 copy` both put text after a colon, but
//! only the second is a stream specifier. `-metadata`, `-map_metadata` and
//! friends take `g`, `s[:<stream_spec>]`, `c:<n>` or `p:<n>` instead. This is
//! precisely why the lexer must not parse specifiers: it does not yet know
//! which grammar the option wants. The descriptor's [`SpecKind`] decides.
//!
//! [`SpecKind`]: crate::table::SpecKind
//!
//! # Probed shape (ffmpeg 8.1)
//!
//! ```text
//! -metadata:        accepted -> global
//! -metadata:g       accepted -> global
//! -metadata:g:0     accepted -> global      (everything after `g` is ignored)
//! -metadata:gg      accepted -> global      (likewise)
//! -metadata:s       accepted -> all streams
//! -metadata:s:v:0   accepted -> stream spec `v:0`
//! -metadata:sv      rejected: "Invalid metadata specifier v."
//! -metadata:c       accepted -> chapter 0   (atoi of the empty string)
//! -metadata:c:x     accepted -> chapter 0   (atoi of "x")
//! -metadata:c:0:1   accepted -> chapter 0   (atoi stops at the colon)
//! -metadata:x       rejected: "Invalid metadata type x."
//! ```
//!
//! Two reference quirks preserved verbatim: `g` swallows any tail, and `c`/`p`
//! run `atoi`, which never fails — `c:x` is chapter 0, not an error.

use core::fmt;

use thiserror::Error;

use crate::num::strtol_base0;
use crate::spec::StreamSpecifier;

/// Which metadata scope an option's specifier names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MetadataSpecifier {
    /// The empty specifier, or `g`. Container-level metadata.
    #[default]
    Global,
    /// `s` or `s:<stream_spec>`.
    Stream(StreamSpecifier),
    /// `c:<n>`, `n` read with `atoi` semantics.
    Chapter(i64),
    /// `p:<n>`, likewise.
    Program(i64),
}

/// Why a metadata specifier was rejected. Text is the reference's.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MetaSpecError {
    #[error("Invalid metadata type {spec}.")]
    InvalidType { spec: String },

    /// Something other than a colon followed `s`.
    #[error("Invalid metadata specifier {rest}.")]
    InvalidSpecifier { rest: String },

    /// The stream specifier after `s:` did not parse. The reference reports the
    /// inner failure, so this variant carries it.
    #[error(transparent)]
    Stream(#[from] crate::error::SpecError),
}

impl MetadataSpecifier {
    /// Parse a metadata specifier.
    ///
    /// # Errors
    /// [`MetaSpecError`], with the reference's message text.
    pub fn parse(s: &str) -> Result<Self, MetaSpecError> {
        let Some(first) = s.as_bytes().first().copied() else {
            return Ok(Self::Global);
        };
        let rest = s.get(1..).unwrap_or("");
        match first {
            // D17: `g` ignores anything that follows it. `-metadata:gg` and
            // `-metadata:g:0` are both accepted and both mean "global". A sane
            // grammar would reject the tail; the reference does not look at it,
            // and a command line that relies on that must keep working.
            b'g' => Ok(Self::Global),
            b's' => match rest.strip_prefix(':') {
                Some(inner) => Ok(Self::Stream(StreamSpecifier::parse(inner)?)),
                None if rest.is_empty() => Ok(Self::Stream(StreamSpecifier::all())),
                None => Err(MetaSpecError::InvalidSpecifier {
                    rest: rest.to_owned(),
                }),
            },
            // D17: `atoi`, not a parse. `c` alone is chapter 0, `c:x` is chapter
            // 0, `c:0:1` is chapter 0. None of them is an error at this stage;
            // the failure, if any, is "Invalid chapter index 0" much later.
            b'c' => Ok(Self::Chapter(atoi(rest.strip_prefix(':').unwrap_or(rest)))),
            b'p' => Ok(Self::Program(atoi(rest.strip_prefix(':').unwrap_or(rest)))),
            _ => Err(MetaSpecError::InvalidType { spec: s.to_owned() }),
        }
    }
}

impl fmt::Display for MetadataSpecifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("g"),
            Self::Stream(s) if s.is_empty() => f.write_str("s"),
            Self::Stream(s) => write!(f, "s:{s}"),
            Self::Chapter(n) => write!(f, "c:{n}"),
            Self::Program(n) => write!(f, "p:{n}"),
        }
    }
}

impl core::str::FromStr for MetadataSpecifier {
    type Err = MetaSpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A never-failing integer read: `strtol` base 0, zero when nothing parses.
///
/// Base 0, not base 10 — verified: `-metadata:c:010` reports "Invalid chapter
/// index 8" and `-metadata:c:0x10` reports 16. Negative and whitespace-prefixed
/// forms are accepted too (`c:-3` → -3, `c: 5` → 5).
fn atoi(s: &str) -> i64 {
    strtol_base0(s).value
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn reference_accepted_forms() {
        assert_eq!(
            MetadataSpecifier::parse("").unwrap(),
            MetadataSpecifier::Global
        );
        assert_eq!(
            MetadataSpecifier::parse("g").unwrap(),
            MetadataSpecifier::Global
        );
        assert_eq!(
            MetadataSpecifier::parse("g:0").unwrap(),
            MetadataSpecifier::Global
        );
        assert_eq!(
            MetadataSpecifier::parse("gg").unwrap(),
            MetadataSpecifier::Global
        );
        assert_eq!(
            MetadataSpecifier::parse("s").unwrap(),
            MetadataSpecifier::Stream(StreamSpecifier::all())
        );
        assert_eq!(
            MetadataSpecifier::parse("s:").unwrap(),
            MetadataSpecifier::Stream(StreamSpecifier::all())
        );
        assert_eq!(
            MetadataSpecifier::parse("s:v:0").unwrap(),
            MetadataSpecifier::Stream(StreamSpecifier::parse("v:0").unwrap())
        );
        assert_eq!(
            MetadataSpecifier::parse("c:0").unwrap(),
            MetadataSpecifier::Chapter(0)
        );
        assert_eq!(
            MetadataSpecifier::parse("c").unwrap(),
            MetadataSpecifier::Chapter(0)
        );
        assert_eq!(
            MetadataSpecifier::parse("c:x").unwrap(),
            MetadataSpecifier::Chapter(0)
        );
        assert_eq!(
            MetadataSpecifier::parse("c:0:1").unwrap(),
            MetadataSpecifier::Chapter(0)
        );
        assert_eq!(
            MetadataSpecifier::parse("p:0").unwrap(),
            MetadataSpecifier::Program(0)
        );
    }

    #[test]
    fn reference_rejected_forms() {
        assert_eq!(
            MetadataSpecifier::parse("x"),
            Err(MetaSpecError::InvalidType { spec: "x".into() })
        );
        assert_eq!(
            MetadataSpecifier::parse("sv"),
            Err(MetaSpecError::InvalidSpecifier { rest: "v".into() })
        );
    }

    #[test]
    fn chapter_index_is_strtol_base_zero() {
        // Verified against the reference's "Invalid chapter index N" message.
        assert_eq!(
            MetadataSpecifier::parse("c:010").unwrap(),
            MetadataSpecifier::Chapter(8)
        );
        assert_eq!(
            MetadataSpecifier::parse("c:0x10").unwrap(),
            MetadataSpecifier::Chapter(16)
        );
        assert_eq!(
            MetadataSpecifier::parse("c:-3").unwrap(),
            MetadataSpecifier::Chapter(-3)
        );
        assert_eq!(
            MetadataSpecifier::parse("c: 5").unwrap(),
            MetadataSpecifier::Chapter(5)
        );
        assert_eq!(
            MetadataSpecifier::parse("p:010").unwrap(),
            MetadataSpecifier::Program(8)
        );
    }

    #[test]
    fn display_round_trips() {
        for s in ["g", "s", "s:v:0", "c:3", "p:2"] {
            let parsed = MetadataSpecifier::parse(s).unwrap();
            assert_eq!(
                MetadataSpecifier::parse(&parsed.to_string()).unwrap(),
                parsed
            );
        }
    }
}
