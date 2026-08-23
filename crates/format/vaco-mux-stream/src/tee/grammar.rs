//! `tee`'s URL grammar: `[opt=val:opt2=val2]path|[opt=val]path2|path3`.
//!
//! # Measured against the reference (ffmpeg 8.1, `LC_ALL=C`)
//!
//! * `|` separates outputs. Escaped as `\|` to put a literal pipe in a path
//!   (measured: `out/weird\|pipe.mpg` opens the file `out/weird|pipe.mpg`).
//! * `[...]` at the start of one output segment holds `:`-separated
//!   `key=value` options for that output; everything after the closing `]`
//!   is the path. A segment with no `[...]` is a bare path.
//! * `select=v` / `select=a` filter that output to one media type (measured:
//!   `[select=v]v.mpg|[select=a:f=wav]a.wav` correctly split a video+audio
//!   input into a video-only and an audio-only file). `select='a:0'`
//!   (quoted) demonstrates the option value's own escaping needs quotes to
//!   carry a literal `:`, since `:` is this level's own separator — the same
//!   quote-then-escape relationship [`vaco_core::escape`] documents for the
//!   option-value grammar generally.
//! * `f=<name>` overrides the target muxer's format name.
//! * `onfail=ignore` lets the whole `tee` open succeed when this one output
//!   fails (measured: `Slave muxer #0 failed: ..., continuing with 1/2
//!   slaves.`, exit 0); without it, one failing output aborts the entire
//!   open (`Slave muxer #0 failed, aborting.`, exit 254). The only other
//!   value observed accepted was the default, `abort`.
//! * `bsfs=<name>` or `bsfs/<v|a|s|d>=<name1>/<name2>` (measured to be
//!   accepted without complaint; the per-media-type form is this crate's
//!   own reading of the reference's option name, not independently
//!   probed against actual filtering behaviour — see `crate::tee`'s module
//!   docs for the honest accounting of what is and is not wired up).
//!
//! This module reuses [`vaco_core::escape`] wholesale rather than writing a
//! second quote/backslash scanner: probing showed the same
//! quote-is-literal, backslash-escapes-the-next-character grammar at every
//! level (`|`, then `[...]`, then `:`, then `=`), which is exactly what
//! [`vaco_core::escape::split_raw`]/[`split_once_raw`] already implement and
//! already property-test.

use vaco_core::Result;
use vaco_core::escape::{self, EscapeError};

/// One `key=value` pair from a bracketed option list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeOption {
    pub key: String,
    pub value: String,
}

/// One `|`-separated output: its bracketed options (if any) and its path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TeeOutput {
    pub options: Vec<TeeOption>,
    pub path: String,
}

impl TeeOutput {
    /// The value of the first option named `key`, if any.
    #[must_use]
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|o| o.key == key)
            .map(|o| o.value.as_str())
    }
}

/// A malformed `tee` URL: an option segment with no `=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarError {
    pub output_index: usize,
    pub detail: String,
}

impl core::fmt::Display for GrammarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "tee output {}: {}", self.output_index, self.detail)
    }
}

impl std::error::Error for GrammarError {}

impl From<EscapeError> for GrammarError {
    fn from(e: EscapeError) -> Self {
        Self {
            output_index: 0,
            detail: e.to_string(),
        }
    }
}

/// Parse a full `tee` URL into its outputs, in order.
///
/// # Errors
/// [`GrammarError`] when a bracketed option segment holds a piece with no
/// `=`, or an option list/URL has an unterminated quote or trailing
/// backslash that [`vaco_core::escape`] cannot resolve into a piece (see
/// that module's docs — in practice this is tolerant, not strict, matching
/// the reference).
pub fn parse(url: &str) -> Result<Vec<TeeOutput>, GrammarError> {
    let pieces = escape::split_raw(url, "|").unwrap_or_default();
    pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| {
            parse_one(piece).map_err(|mut e| {
                e.output_index = i;
                e
            })
        })
        .collect()
}

fn parse_one(piece: &str) -> Result<TeeOutput, GrammarError> {
    let Some(rest) = piece.strip_prefix('[') else {
        return Ok(TeeOutput {
            options: Vec::new(),
            path: escape::unescape(piece)?,
        });
    };
    let Some((opts_str, path)) = escape::split_once_raw(rest, "]")? else {
        // No closing bracket at all: the whole thing is a path starting
        // with a literal `[`, which is what the reference falls back to for
        // a segment that never closes its bracket (a bracket only has
        // meaning once it closes).
        return Ok(TeeOutput {
            options: Vec::new(),
            path: escape::unescape(piece)?,
        });
    };
    let mut options = Vec::new();
    for opt in escape::split_raw(opts_str, ":").unwrap_or_default() {
        let Some((k, v)) = escape::split_once_raw(opt, "=")? else {
            return Err(GrammarError {
                output_index: 0,
                detail: format!("option '{opt}' has no '='"),
            });
        };
        options.push(TeeOption {
            key: escape::unescape(k)?,
            value: escape::unescape(v)?,
        });
    }
    Ok(TeeOutput {
        options,
        path: escape::unescape(path)?,
    })
}

/// Render one [`TeeOutput`] back into the URL grammar, for the round-trip
/// property test and for a caller that builds a `tee` URL programmatically.
#[must_use]
pub fn format_output(output: &TeeOutput) -> String {
    let mut s = String::new();
    if !output.options.is_empty() {
        s.push('[');
        for (i, opt) in output.options.iter().enumerate() {
            if i > 0 {
                s.push(':');
            }
            s.push_str(&escape::escape(&opt.key, ":]=|[", escape::Mode::Backslash));
            s.push('=');
            s.push_str(&escape::escape(
                &opt.value,
                ":]=|[",
                escape::Mode::Backslash,
            ));
        }
        s.push(']');
    }
    s.push_str(&escape::escape(
        &output.path,
        ":]=|[",
        escape::Mode::Backslash,
    ));
    s
}

/// Render a full list of outputs back into one `tee` URL.
#[must_use]
pub fn format(outputs: &[TeeOutput]) -> String {
    outputs
        .iter()
        .map(format_output)
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn splits_on_pipe() {
        let outs = parse("out/a.mpg|out/b.ts").unwrap();
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].path, "out/a.mpg");
        assert_eq!(outs[1].path, "out/b.ts");
    }

    #[test]
    fn parses_bracketed_options_and_the_path_after_them() {
        let outs = parse("[f=mpegts]out/b.ts").unwrap();
        assert_eq!(outs[0].option("f"), Some("mpegts"));
        assert_eq!(outs[0].path, "out/b.ts");
    }

    #[test]
    fn select_and_f_together() {
        let outs = parse("[select=v]out/v.mpg|[select=a:f=wav]out/a.wav").unwrap();
        assert_eq!(outs[0].option("select"), Some("v"));
        assert_eq!(outs[1].option("select"), Some("a"));
        assert_eq!(outs[1].option("f"), Some("wav"));
    }

    #[test]
    fn a_quoted_option_value_can_carry_a_literal_colon() {
        let outs = parse("[select='a:0']out/a.wav").unwrap();
        assert_eq!(outs[0].option("select"), Some("a:0"));
    }

    #[test]
    fn a_backslash_escapes_a_literal_pipe_in_a_path() {
        let outs = parse("out/weird\\|pipe.mpg").unwrap();
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].path, "out/weird|pipe.mpg");
    }

    #[test]
    fn onfail_and_bsfs_options_parse() {
        let outs = parse("[onfail=ignore]out/a.mpg").unwrap();
        assert_eq!(outs[0].option("onfail"), Some("ignore"));
        let outs = parse("[bsfs/v=noise]out/b.mpg").unwrap();
        assert_eq!(outs[0].option("bsfs/v"), Some("noise"));
    }

    #[test]
    fn an_option_with_no_equals_is_an_error() {
        assert!(parse("[nope]out/a.mpg").is_err());
    }

    #[test]
    fn round_trips_through_format() {
        let outs = parse("[select=v:f=mpegts]out/a.mpg|out/b.ts").unwrap();
        let rendered = format(&outs);
        let reparsed = parse(&rendered).unwrap();
        assert_eq!(outs, reparsed);
    }

    proptest! {
        /// Any path (drawn from a restricted but still awkward alphabet)
        /// survives format -> parse.
        #[test]
        fn single_bare_output_round_trips(path in "[a-zA-Z0-9/_.: \\[\\]|\\\\']{0,20}") {
            let output = TeeOutput { options: Vec::new(), path: path.clone() };
            let rendered = format_output(&output);
            let reparsed = parse(&rendered).unwrap();
            prop_assert_eq!(reparsed.len(), 1);
            prop_assert_eq!(&reparsed[0].path, &path);
        }

        /// A single key=value option plus a path round-trips for any values
        /// drawn from the same awkward alphabet.
        #[test]
        fn option_and_path_round_trip(
            key in "[a-zA-Z0-9_/]{1,8}",
            value in "[a-zA-Z0-9/_.: \\[\\]|\\\\']{0,15}",
            path in "[a-zA-Z0-9/_.]{1,15}",
        ) {
            let output = TeeOutput {
                options: vec![TeeOption { key: key.clone(), value: value.clone() }],
                path: path.clone(),
            };
            let rendered = format_output(&output);
            let reparsed = parse(&rendered).unwrap();
            prop_assert_eq!(reparsed.len(), 1);
            prop_assert_eq!(reparsed[0].option(&key), Some(value.as_str()));
            prop_assert_eq!(&reparsed[0].path, &path);
        }
    }
}
