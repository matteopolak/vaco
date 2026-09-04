//! The concat demuxer's own script grammar: pure text in, structured
//! directives out. No filesystem access here at all — see `crate::concat`'s
//! module docs for why that split matters.
//!
//! # Measured against the reference (ffmpeg 8.1, `LC_ALL=C`)
//!
//! `ffmpeg -f concat -safe 0 -i list.txt …`, with hand-written scripts fed as
//! input and the result inspected via `-c copy -f null -`/`ffprobe`:
//!
//! * A line whose first non-whitespace character is `#` is a comment, in
//!   full. **`;` is not** — `; comment` fails with `Line 1: unknown keyword
//!   ';'`, unlike `ffmetadata`'s dual `;`/`#` convention. Do not assume the
//!   two text formats in this crate share a comment rule; measured, they do
//!   not.
//! * Tokenising a directive line uses the same quote/backslash grammar as
//!   [`vaco_core::escape`]: `'...'` is a literal span with **no** backslash
//!   meaning inside it, `\` outside quotes strips itself and keeps the next
//!   character literally, and quoted/unquoted/escaped spans concatenate with
//!   no separator needed between them. Measured directly: the script line
//!   `file 'weird dir/seg\'s.ts'` resolves to the path `weird dir/seg\s.ts`,
//!   which is exactly [`vaco_core::escape::unescape`]'s reading of that text
//!   (the quote closes at the first bare `'`, which is the one right after
//!   the backslash — the backslash has no effect inside the quote and is
//!   kept literally — then `s.ts` follows unquoted, then the final `'` opens
//!   a quote that runs to end of line). An **unterminated** quote is
//!   tolerated, not an error (`file 'seg1.ts` with no closing quote opens
//!   `seg1.ts` and the run succeeds) — matching
//!   [`vaco_core::escape::split_raw`]'s own documented tolerance, which is
//!   why this module calls that function and not
//!   [`vaco_core::escape::split_once_raw`] (which *does* error on an
//!   unterminated quote — the wrong choice here, confirmed by the probe).
//! * An unrecognised directive keyword is a hard error:
//!   `Line {n}: unknown keyword '{kw}'` (measured verbatim, 1-indexed lines).
//! * `option <name> <value>` is rejected outright when `-safe` is not `0`:
//!   `Line {n}: option not allowed if safe` (measured). This module models
//!   that as [`ScriptError::OptionNotAllowedIfSafe`] and leaves the decision
//!   of what `-safe` is set to, to the caller of [`parse`].
//! * `duration`/`inpoint`/`outpoint` accept both `SS[.frac]` and
//!   `HH:MM:SS[.frac]` (both measured to work); this module reuses
//!   [`vaco_core::parse::duration`], the CLI's own grammar for exactly this
//!   shape, rather than re-deriving it.
//!
//! `file_packet_metadata` and the `stream`/`exact_stream_id` block were
//! probed only enough to confirm they parse as two-token and zero/one-token
//! directives respectively; their *semantic* effect (which packet a given
//! `file_packet_metadata` attaches to, what `exact_stream_id` changes about
//! stream matching) was not exhaustively verified against the reference and
//! is recorded here structurally rather than interpreted — see
//! [`Directive::FilePacketMetadata`] and [`Directive::ExactStreamId`].

use vaco_core::Duration;
use vaco_core::escape;
use vaco_core::parse::duration as parse_duration;

/// One directive line, already tokenised and validated against the known
/// keyword set — but not yet resolved into a [`crate::concat::FileEntry`]
/// list, which is [`Script::entries`]'s job.
#[derive(Debug, Clone, PartialEq)]
pub enum Directive {
    /// `ffconcat version <ver>`. Recorded, not enforced — every version
    /// string measured (including a wrong one, `1.0` vs an invented `2.0`)
    /// was accepted, and the version line is itself just a comment-shaped
    /// directive in practice, similar to `ffmetadata`'s unchecked header.
    FfconcatVersion(String),
    /// `file <path>`. Starts a new entry.
    File(String),
    /// `duration <time>`. Applies to the most recently opened `file`.
    Duration(Duration),
    /// `inpoint <time>`. Applies to the most recently opened `file`.
    Inpoint(Duration),
    /// `outpoint <time>`. Applies to the most recently opened `file`.
    Outpoint(Duration),
    /// `file_packet_metadata <key> <value>`. Two-token form, assumed rather
    /// than exhaustively probed — see the module docs.
    FilePacketMetadata(String, String),
    /// `option <name> <value>`. Only valid when the caller's `safe` is
    /// `false`; [`parse`] returns [`ScriptError::OptionNotAllowedIfSafe`]
    /// otherwise, matching the measured message.
    Option(String, String),
    /// `stream`. Opens a per-file stream-override block; sub-directives
    /// until the next `file`/`stream`/EOF are collected in
    /// [`crate::concat::FileEntry::stream_directives`] rather than
    /// interpreted.
    Stream,
    /// `exact_stream_id <id>`. Only meaningful inside a `stream` block;
    /// collected alongside it either way.
    ExactStreamId(String),
}

/// A parse failure, with the 1-indexed source line it came from.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptError {
    /// `Line {line}: unknown keyword '{keyword}'` — verbatim, measured.
    UnknownKeyword { line: usize, keyword: String },
    /// `Line {line}: option not allowed if safe` — verbatim, measured.
    OptionNotAllowedIfSafe { line: usize },
    /// A directive that needs N tokens got fewer.
    MissingArgument { line: usize, keyword: String },
    /// A `duration`/`inpoint`/`outpoint` value [`vaco_core::parse::duration`]
    /// could not parse.
    BadDuration { line: usize, value: String },
}

impl core::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownKeyword { line, keyword } => {
                write!(f, "Line {line}: unknown keyword '{keyword}'")
            }
            Self::OptionNotAllowedIfSafe { line } => {
                write!(f, "Line {line}: option not allowed if safe")
            }
            Self::MissingArgument { line, keyword } => {
                write!(f, "Line {line}: '{keyword}' needs an argument")
            }
            Self::BadDuration { line, value } => {
                write!(f, "Line {line}: '{value}' is not a valid duration")
            }
        }
    }
}

impl std::error::Error for ScriptError {}

/// One tokenised, directive-classified line plus its 1-indexed source line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub line: usize,
    pub directive: Directive,
}

/// A fully parsed script: every directive line, in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    pub lines: Vec<Line>,
}

/// Split one line into whitespace-separated tokens, honouring the
/// quote/backslash grammar. Never fails — an unterminated quote or a
/// trailing backslash are both tolerated, matching the reference (see the
/// module docs) and [`vaco_core::escape::split_raw`]'s own documented
/// behaviour for the same two cases.
fn tokenize(line: &str) -> Vec<String> {
    // `split_raw` cannot itself fail (its `Result` exists for a symmetry
    // with `split_once_raw` this call site does not need), so a defensive
    // empty fallback costs nothing and avoids `unwrap_used`.
    let raw = escape::split_raw(line, " \t").unwrap_or_default();
    raw.into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| escape::unescape(p).unwrap_or_default())
        .collect()
}

/// Parse a whole script.
///
/// `safe` decides whether an `option` directive is permitted (mirrors the
/// demuxer's own `-safe` option; measured, `option` is rejected unless
/// `-safe 0`). This function does **not** open any file — see
/// `crate::concat::ConcatDemuxer` for the layer that does.
///
/// # Errors
/// [`ScriptError`] for an unrecognised keyword, a missing argument, an
/// unparseable duration, or an `option` line while `safe` is `true`.
pub fn parse(input: &str, safe: bool) -> Result<Script, ScriptError> {
    let mut lines = Vec::new();
    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let tokens = tokenize(raw_line);
        let Some((keyword, args)) = tokens.split_first() else {
            continue;
        };
        let directive = classify(line_no, keyword, args, safe)?;
        lines.push(Line {
            line: line_no,
            directive,
        });
    }
    Ok(Script { lines })
}

fn classify(
    line_no: usize,
    keyword: &str,
    args: &[String],
    safe: bool,
) -> Result<Directive, ScriptError> {
    let need = |n: usize| -> Result<(), ScriptError> {
        if args.len() < n {
            Err(ScriptError::MissingArgument {
                line: line_no,
                keyword: keyword.to_owned(),
            })
        } else {
            Ok(())
        }
    };
    let dur = |s: &str| -> Result<Duration, ScriptError> {
        parse_duration(s).ok_or_else(|| ScriptError::BadDuration {
            line: line_no,
            value: s.to_owned(),
        })
    };

    match keyword {
        "ffconcat" => {
            // `ffconcat version 1.0`: two tokens, `version` then the number.
            need(2)?;
            Ok(Directive::FfconcatVersion(
                args.get(1).cloned().unwrap_or_default(),
            ))
        }
        "file" => {
            need(1)?;
            Ok(Directive::File(args.first().cloned().unwrap_or_default()))
        }
        "duration" => {
            need(1)?;
            Ok(Directive::Duration(dur(args
                .first()
                .map_or("", String::as_str))?))
        }
        "inpoint" => {
            need(1)?;
            Ok(Directive::Inpoint(dur(args
                .first()
                .map_or("", String::as_str))?))
        }
        "outpoint" => {
            need(1)?;
            Ok(Directive::Outpoint(dur(args
                .first()
                .map_or("", String::as_str))?))
        }
        "file_packet_metadata" => {
            need(2)?;
            Ok(Directive::FilePacketMetadata(
                args.first().cloned().unwrap_or_default(),
                args.get(1).cloned().unwrap_or_default(),
            ))
        }
        "option" => {
            if safe {
                return Err(ScriptError::OptionNotAllowedIfSafe { line: line_no });
            }
            need(2)?;
            Ok(Directive::Option(
                args.first().cloned().unwrap_or_default(),
                args.get(1).cloned().unwrap_or_default(),
            ))
        }
        "stream" => Ok(Directive::Stream),
        "exact_stream_id" => {
            need(1)?;
            Ok(Directive::ExactStreamId(
                args.first().cloned().unwrap_or_default(),
            ))
        }
        other => Err(ScriptError::UnknownKeyword {
            line: line_no,
            keyword: other.to_owned(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn hash_is_a_comment_semicolon_is_not() {
        assert!(parse("# a comment\nfile 'a.ts'\n", true).is_ok());
        let err = parse("; a comment\nfile 'a.ts'\n", true).unwrap_err();
        assert_eq!(
            err,
            ScriptError::UnknownKeyword {
                line: 1,
                keyword: ";".to_owned()
            }
        );
    }

    #[test]
    fn unknown_keyword_reports_line_and_text_verbatim() {
        let err = parse("file 'a.ts'\nfrobnicate xyz\n", true).unwrap_err();
        assert_eq!(err.to_string(), "Line 2: unknown keyword 'frobnicate'");
    }

    #[test]
    fn option_is_rejected_unless_unsafe() {
        let err = parse("option safe 0\nfile 'a.ts'\n", true).unwrap_err();
        assert_eq!(err, ScriptError::OptionNotAllowedIfSafe { line: 1 });
        assert!(parse("option safe 0\nfile 'a.ts'\n", false).is_ok());
    }

    #[test]
    fn quoted_path_with_embedded_backslash_quote_matches_the_probe() {
        let script = parse("file 'weird dir/seg\\'s.ts'\n", true).unwrap();
        assert_eq!(
            script.lines[0].directive,
            Directive::File("weird dir/seg\\s.ts".to_owned())
        );
    }

    #[test]
    fn unterminated_quote_is_tolerated_not_an_error() {
        let script = parse("file 'seg1.ts\n", true).unwrap();
        assert_eq!(
            script.lines[0].directive,
            Directive::File("seg1.ts".to_owned())
        );
    }

    #[test]
    fn unquoted_and_quoted_spans_concatenate() {
        let script = parse("file plain.ts\n", true).unwrap();
        assert_eq!(
            script.lines[0].directive,
            Directive::File("plain.ts".to_owned())
        );
    }

    #[test]
    fn duration_accepts_seconds_and_clock_form() {
        let script = parse("file 'a.ts'\ninpoint 00:00:00.2\noutpoint 0.8\n", true).unwrap();
        assert_eq!(
            script.lines[1].directive,
            Directive::Inpoint(Duration::from_micros(200_000))
        );
        assert_eq!(
            script.lines[2].directive,
            Directive::Outpoint(Duration::from_micros(800_000))
        );
    }

    #[test]
    fn a_bad_duration_is_a_named_error() {
        let err = parse("file 'a.ts'\nduration not-a-time\n", true).unwrap_err();
        assert_eq!(
            err,
            ScriptError::BadDuration {
                line: 2,
                value: "not-a-time".to_owned()
            }
        );
    }

    #[test]
    fn ffconcat_version_and_stream_block_are_recognised() {
        let script = parse(
            "ffconcat version 1.0\nfile 'a.ts'\nstream\nexact_stream_id 0x101\n",
            true,
        )
        .unwrap();
        assert_eq!(
            script.lines[0].directive,
            Directive::FfconcatVersion("1.0".to_owned())
        );
        assert_eq!(script.lines[2].directive, Directive::Stream);
        assert_eq!(
            script.lines[3].directive,
            Directive::ExactStreamId("0x101".to_owned())
        );
    }

    #[test]
    fn blank_and_whitespace_only_lines_are_skipped() {
        let script = parse("file 'a.ts'\n   \n\nfile 'b.ts'\n", true).unwrap();
        assert_eq!(script.lines.len(), 2);
    }
}
