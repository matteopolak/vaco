//! The `-map` value grammar.
//!
//! `-map` is where the specifier grammar meets the file list, and it is the
//! option users get wrong most often, so its acceptance set matters
//! disproportionately.
//!
//! ```text
//! map   ::= '[' label ']'                       (a filtergraph output pad)
//!         | [ '-' ] file_index [ ':' ] spec [ ':' ] [ '?' ]
//! ```
//!
//! Four things are not obvious and were established by probing ffmpeg 8.1:
//!
//! 1. **The file index is `strtol`, and consuming nothing is not an error.**
//!    `-map v` is file 0, specifier `v`; `-map a:0` is file 0, specifier `a:0`.
//!    So a specifier that happens to start with a letter needs no `0:` prefix.
//! 2. **One colon is eaten before the specifier and one after it.** That is why
//!    `-map 0::` and `-map 0:v::` are accepted while `-map 0:::` is not: the
//!    budget is exactly two, one on each side of the specifier parse.
//! 3. **`?` counts only as the very last character.** `-map 0:v??` is rejected
//!    whole, and `-map 0?:v` is rejected — the marker is not a general suffix.
//! 4. **The help text is stale.** It still advertises
//!    `[,sync_file_id[:stream_specifier]]`, but ffmpeg 8.1 rejects any comma:
//!    `-map 0:v,1` fails with "Trailing garbage after stream specifier: ,1".
//!    We follow the binary, not the help string.

use core::fmt;

use crate::error::CliError;
use crate::num::strtol_base0;
use crate::spec::{ParseMode, StreamSpecifier};

/// One parsed `-map` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapSpec {
    /// `[label]` — an output pad of a `-filter_complex` graph.
    ///
    /// The closing bracket is optional in the reference: `-map [v` names the
    /// label `v`, and `-map []` names the empty label.
    Label(String),
    /// The ordinary form.
    File(FileMap),
}

/// `[-]<file_index>[:<stream_spec>][?]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMap {
    /// A leading `-`: remove these streams from what earlier maps selected.
    pub negative: bool,
    /// The input file's position among input files.
    pub file_index: i64,
    pub spec: StreamSpecifier,
    /// A trailing `?`: matching nothing is not an error.
    pub allow_unused: bool,
}

impl MapSpec {
    /// Parse a `-map` value.
    ///
    /// # Errors
    /// [`CliError::MapTrailingGarbage`] for leftovers, or
    /// [`CliError::InvalidStreamSpecifier`] when the specifier itself is
    /// malformed — that variant carries the inner
    /// [`SpecError`](crate::error::SpecError), which the reference prints on the
    /// preceding line.
    pub fn parse(s: &str) -> Result<Self, CliError> {
        if let Some(after) = s.strip_prefix('[') {
            // D17: the closing bracket is never checked. `-map [v` is accepted
            // and names the label `v`. Do not "fix" this by requiring `]`.
            let label = after.strip_suffix(']').unwrap_or(after);
            return Ok(Self::Label(label.to_owned()));
        }

        let (negative, rest) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };

        let scanned = strtol_base0(rest);
        let file_index = scanned.value;
        // `consumed == 0` is fine: file index 0 and the whole string is the
        // specifier. `-map v` relies on this.
        let after_index = scanned.rest;
        let after_colon = after_index.strip_prefix(':').unwrap_or(after_index);

        let (spec, remainder) = StreamSpecifier::parse_prefix(after_colon, ParseMode::Prefix)
            .map_err(|inner| CliError::InvalidStreamSpecifier {
                // The reference names the text as it stood *before* the colon
                // was eaten: `-map 0:p:x` reports `:p:x`, `-map p:x` reports
                // `p:x`.
                text: after_index.to_owned(),
                inner,
            })?;

        // The second of the two tolerated colons.
        let remainder = remainder.strip_prefix(':').unwrap_or(remainder);

        let allow_unused = remainder == "?";
        let remainder = if allow_unused { "" } else { remainder };

        if !remainder.is_empty() {
            return Err(CliError::MapTrailingGarbage {
                rest: remainder.to_owned(),
            });
        }

        Ok(Self::File(FileMap {
            negative,
            file_index,
            spec,
            allow_unused,
        }))
    }
}

impl fmt::Display for MapSpec {
    /// Render a value that parses back to the same [`MapSpec`].
    ///
    /// One case needs care. A **negative file index** is reachable — `strtol`
    /// skips leading whitespace and accepts a sign, so `-map $'\f-9'` yields
    /// index `-9` with no negation marker — but writing it as `-9` would reparse
    /// as "negatively map file 9". A leading space disambiguates, and the
    /// reference accepts it for the same `strtol` reason: `-map ' -9'` reaches
    /// the same state (verified; it then fails validation with "Invalid input
    /// file index: -9.", which is a later stage, not the grammar).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Label(l) => write!(f, "[{l}]"),
            Self::File(m) => {
                if m.negative {
                    f.write_str("-")?;
                }
                if m.file_index < 0 {
                    f.write_str(" ")?;
                }
                write!(f, "{}", m.file_index)?;
                if m.spec.is_empty() {
                    if m.allow_unused {
                        f.write_str("?")?;
                    }
                } else {
                    write!(f, ":{}", m.spec)?;
                    // The `?` needs a separating colon after a non-empty
                    // specifier, because a terminal token refuses to be
                    // followed by anything: `0:s:u?` is rejected while
                    // `0:s:u:?` is accepted (verified). The map grammar's
                    // second tolerated colon is exactly what makes room for it.
                    if m.allow_unused {
                        f.write_str(":?")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl core::str::FromStr for MapSpec {
    type Err = CliError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use crate::error::SpecError;

    fn file(s: &str) -> FileMap {
        match MapSpec::parse(s) {
            Ok(MapSpec::File(m)) => m,
            other => panic!("{s:?} should be a file map, got {other:?}"),
        }
    }

    #[test]
    fn reference_accepts() {
        // Every one of these was accepted by ffmpeg 8.1.
        for s in [
            "0", "0:v", "0:a", "0:v:0", "0:a:1", "1:a", "-0:a", "-0", "0:a?", "0:s?", "0:9?",
            "-0:s?", "0:v:0?", "v", ":v", "0:", "0::", "", "-0:a?", "0:#1?", "+0:v", "1:v", "00:v",
            "1:", "v:0", "a:0", "a", "?", "0?", "0:v?", "-?", "0x1:v", "1v", "0:0", "0:1", "::",
            ":", "0:v:", "0:v::", "u", "0:u", "#1", "0:#1", "m:k", "-0:v?", "--0:v", "-v", ":0",
        ] {
            assert!(MapSpec::parse(s).is_ok(), "reference accepts {s:?}");
        }
    }

    #[test]
    fn reference_rejects() {
        for (s, rest) in [
            ("0,1", ",1"),
            ("0:v,1:a", ",1:a"),
            ("0:v,1", ",1"),
            (",0:v", ",0:v"),
            ("0:v,", ","),
            ("abc", "abc"),
            ("0abc", "abc"),
            ("0:v:x", "x"),
            ("0 :v", " :v"),
            (":::", ":"),
            ("0:::", ":"),
            ("::0", "0"),
            (":::0", ":0"),
            ("::v", "v"),
            (":::v", ":v"),
            ("0::v", "v"),
            ("0:::v", ":v"),
            ("::a:1", "a:1"),
            ("0:v??", "??"),
            ("?0:v", "?0:v"),
            ("0?:v", "?:v"),
            ("]v[", "]v["),
            ("0:v?,", "?,"),
        ] {
            assert_eq!(
                MapSpec::parse(s),
                Err(CliError::MapTrailingGarbage {
                    rest: rest.to_owned()
                }),
                "for {s:?}"
            );
        }
    }

    #[test]
    fn index_scan_lets_a_specifier_start_the_value() {
        assert_eq!(file("v").file_index, 0);
        assert_eq!(file("v").spec, StreamSpecifier::parse("v").unwrap());
        assert_eq!(file("a:0").spec, StreamSpecifier::parse("a:0").unwrap());
        assert_eq!(file("0x1:v").file_index, 1);
        assert_eq!(file("1v").file_index, 1);
        assert_eq!(file("1v").spec, StreamSpecifier::parse("v").unwrap());
    }

    #[test]
    fn negation_and_optionality() {
        assert!(file("-0:a").negative);
        assert!(!file("0:a").negative);
        assert!(file("0:a?").allow_unused);
        assert!(!file("0:a").allow_unused);
        // `--0:v` is a negative map of file index... `-0` == 0, from strtol.
        let m = file("--0:v");
        assert!(m.negative);
        assert_eq!(m.file_index, 0);
    }

    #[test]
    fn labels() {
        assert_eq!(MapSpec::parse("[v]").unwrap(), MapSpec::Label("v".into()));
        assert_eq!(MapSpec::parse("[v").unwrap(), MapSpec::Label("v".into()));
        assert_eq!(MapSpec::parse("[]").unwrap(), MapSpec::Label(String::new()));
    }

    #[test]
    fn the_two_colon_budget() {
        // one colon each side of the specifier; a third is garbage
        assert!(MapSpec::parse("0:").is_ok());
        assert!(MapSpec::parse("0::").is_ok());
        assert!(MapSpec::parse("0:::").is_err());
        assert!(MapSpec::parse("0:v:").is_ok());
        assert!(MapSpec::parse("0:v::").is_ok());
        assert!(MapSpec::parse("0:v:::").is_err());
    }

    #[test]
    fn specifier_errors_are_wrapped_with_the_map_level_line() {
        assert_eq!(
            MapSpec::parse("0:p:x"),
            Err(CliError::InvalidStreamSpecifier {
                text: ":p:x".into(),
                inner: SpecError::ExpectedProgramId { rest: "x".into() },
            })
        );
        // Without a file index the text has no leading colon.
        assert_eq!(
            MapSpec::parse("p:x"),
            Err(CliError::InvalidStreamSpecifier {
                text: "p:x".into(),
                inner: SpecError::ExpectedProgramId { rest: "x".into() },
            })
        );
        // The negation marker is stripped before the text is taken.
        assert_eq!(
            MapSpec::parse("-0:p:x"),
            Err(CliError::InvalidStreamSpecifier {
                text: ":p:x".into(),
                inner: SpecError::ExpectedProgramId { rest: "x".into() },
            })
        );
        // The inner failure is reachable for printing.
        let e = MapSpec::parse("0:v:v").unwrap_err();
        assert_eq!(e.inner_spec(), Some(&SpecError::DuplicateType));
        assert_eq!(e.to_string(), "Invalid stream specifier: :v:v");
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "0", "0:v", "-0:a:1?", "[v]", "1:m:k", " -9", "- -9", "\u{c}-9", "0?", "0:v?", "s:u:?",
            "0:s:u:?", "0:a:1:?", "0::?",
        ] {
            let parsed = MapSpec::parse(s).unwrap();
            assert_eq!(
                MapSpec::parse(&parsed.to_string()).unwrap(),
                parsed,
                "{s:?}"
            );
        }
    }

    #[test]
    fn a_negative_file_index_is_reachable_and_survives_rendering() {
        // `strtol` skips whitespace and takes a sign, so a form-feed followed
        // by `-9` is file index -9 with no negation marker. Found by fuzzing.
        let m = file("\u{c}-9");
        assert_eq!(m.file_index, -9);
        assert!(!m.negative);
        assert_eq!(MapSpec::File(m).to_string(), " -9");
        assert_eq!(file(" -9").file_index, -9);
        let n = file("-1");
        assert_eq!(n.file_index, 1);
        assert!(n.negative);
    }
}
