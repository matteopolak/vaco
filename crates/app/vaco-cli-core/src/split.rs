//! Splitting argv into a global option set plus ordered per-file groups.
//!
//! This is the whole scope model, executed. Read [`crate::table`] first for the
//! model itself; this module is the machine.
//!
//! ```text
//! pending = []
//! for each entry:
//!     option?  -> global   : hoist, position irrelevant
//!              -> opens an input : close a group with `pending`, kind = Input
//!              -> otherwise      : push onto `pending`
//!     bare?    -> close a group with `pending`, kind = the tool's positional kind
//! leftover `pending` is DISCARDED
//! ```
//!
//! Four behaviours that surprise people, all verified against ffmpeg 8.1:
//!
//! 1. **Trailing per-file options are silently dropped**, not an error.
//!    `ffmpeg -i a -f null - -c:v libx264` exits 0 and ignores the `-c:v`. They
//!    are kept in [`CommandLine::orphaned`] so a caller can warn; the reference
//!    does not.
//! 2. **Output may precede input.** `ffmpeg -y out.mkv -i in.mkv` is accepted;
//!    ordering between groups is not constrained at this stage.
//! 3. **A value is never re-lexed.** `-metadata title=-y` works, and so does
//!    `-i -weird.mkv`.
//! 4. **Unknown options are not necessarily errors.** `-crf 20` is valid
//!    because *some* encoder declares a `crf` option; `-qwerty 3` is not,
//!    because none does. The reference decides this at split time by consulting
//!    every `AVOption` class in the process. We take that decision as an injected
//!    [`AvOptionOracle`] rather than pretending to know, so `vaco-cli` can hand
//!    us the registry and get the reference's timing.

use std::ffi::{OsStr, OsString};

use crate::error::{CliError, Result};
use crate::lex::{Token, classify};
use crate::metaspec::MetadataSpecifier;
use crate::spec::StreamSpecifier;
use crate::stream::MatchCtx;
use crate::table::{Lookup, OptDesc, OptFlags, OptTable, Positional, SpecKind};

/// Decides whether an option name the tool's own table does not have could
/// still be a component `AVOption`.
///
/// The reference searches every codec, format, scaler and resampler class at
/// split time, so `-crf 20` before any encoder is chosen is accepted while
/// `-qwerty 3` is rejected on the spot. That search needs the component
/// registry, which sits above this crate, so it is injected.
pub trait AvOptionOracle {
    /// Whether *any* component declares an option by this name.
    fn knows(&self, name: &str) -> bool;
}

/// Accepts every unknown name, deferring the decision to a later audit.
///
/// The default. It moves the "Unrecognized option" failure from split time to
/// audit time, which is a *timing* divergence from the reference and is
/// documented as such. Pass a registry-backed oracle to remove it.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptUnknown;

impl AvOptionOracle for AcceptUnknown {
    fn knows(&self, _name: &str) -> bool {
        true
    }
}

/// Rejects every unknown name at split time. Useful in tests and for a tool
/// with no component options at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct RejectUnknown;

impl AvOptionOracle for RejectUnknown {
    fn knows(&self, _name: &str) -> bool {
        false
    }
}

/// One occurrence of one option on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOption {
    /// The name as written, without the dash and without the specifier. For a
    /// `no`-negated boolean this is the *base* name: `-nostats` yields `stats`.
    pub name: String,
    /// `None` for a deferred component option.
    pub desc: Option<&'static OptDesc>,
    /// Set by the `no` prefix. Only ever true for a table option with no value.
    pub negated: bool,
    /// The raw specifier text, unparsed. `None` when there was no colon.
    pub spec: Option<String>,
    /// The following argv entry, verbatim. `None` for a valueless option.
    pub value: Option<OsString>,
    /// Whether the value names a file holding the real value (`-/opt path`).
    pub file_indirect: bool,
    /// Position of the name token in the original argv.
    pub argv_index: usize,
}

impl ParsedOption {
    /// The effective (target name, specifier text) after alias rewriting.
    ///
    /// `-vf x` resolves to `("filter", Some("v"))`. When an alias bakes in a
    /// specifier, the user's own suffix is discarded — which is what the
    /// reference does, since an alias is never itself a per-stream option.
    #[must_use]
    pub fn resolved(&self) -> (&str, Option<&str>) {
        match self.desc.and_then(|d| d.alias_of) {
            Some((target, "")) => (target, self.spec.as_deref()),
            Some((target, spec)) => (target, Some(spec)),
            None => (self.name.as_str(), self.spec.as_deref()),
        }
    }

    /// Parse the specifier under the grammar this option's descriptor names.
    ///
    /// Returns `Ok(None)` when the option takes no specifier — including the
    /// case where the user wrote one anyway, which the reference accepts and
    /// ignores.
    ///
    /// # Errors
    /// The specifier grammar's error, unchanged.
    pub fn stream_spec(&self) -> Result<Option<StreamSpecifier>> {
        let Some(desc) = self.desc else {
            // A deferred component option is applied per stream, so its
            // specifier is a stream specifier.
            return match self.spec.as_deref() {
                Some(s) => Ok(Some(StreamSpecifier::parse(s)?)),
                None => Ok(None),
            };
        };
        let (_, spec) = self.resolved();
        match (desc.spec_kind(), spec) {
            (SpecKind::Stream, Some(s)) => Ok(Some(StreamSpecifier::parse(s)?)),
            _ => Ok(None),
        }
    }

    /// Parse the specifier as a metadata specifier, for the options that take
    /// one.
    ///
    /// # Errors
    /// [`crate::metaspec::MetaSpecError`], wrapped.
    pub fn metadata_spec(
        &self,
    ) -> core::result::Result<Option<MetadataSpecifier>, crate::metaspec::MetaSpecError> {
        let Some(desc) = self.desc else {
            return Ok(None);
        };
        match (desc.spec_kind(), self.spec.as_deref()) {
            (SpecKind::Metadata, Some(s)) => Ok(Some(MetadataSpecifier::parse(s)?)),
            (SpecKind::Metadata, None) => Ok(Some(MetadataSpecifier::Global)),
            _ => Ok(None),
        }
    }

    /// The value, requiring it to be UTF-8.
    ///
    /// # Errors
    /// [`CliError::InvalidValue`] when it is not, carrying the raw bytes.
    pub fn value_str(&self, kind: &'static str) -> Result<&str> {
        let Some(v) = self.value.as_ref() else {
            return Err(CliError::MissingArgument {
                name: self.name.clone(),
            });
        };
        v.to_str().ok_or_else(|| CliError::InvalidValue {
            option: self.name.clone(),
            kind,
            value: v.clone(),
        })
    }

    /// Read the value from the file it names, for `-/opt path`.
    ///
    /// Not done during splitting: splitting stays a pure function of argv so it
    /// can be fuzzed without touching the filesystem.
    ///
    /// # Errors
    /// [`CliError::IndirectionFailed`] if the file cannot be read.
    pub fn read_indirect(&self) -> Result<Vec<u8>> {
        let path = self.value.clone().unwrap_or_default();
        std::fs::read(&path).map_err(|_| CliError::IndirectionFailed {
            option: self.name.clone(),
            path,
        })
    }
}

/// Whether a group is an input file or an output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Input,
    Output,
}

/// One file and the options bound to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionGroup {
    pub kind: GroupKind,
    /// Position among groups of the same kind. This is the `input_file_id` a
    /// `-map` refers to.
    pub index: u32,
    /// The URL, verbatim and never transcoded.
    pub url: OsString,
    pub opts: Vec<ParsedOption>,
    /// Where the URL appeared in argv.
    pub argv_index: usize,
}

impl OptionGroup {
    /// The last occurrence of a file-level option, which is the one that wins.
    #[must_use]
    pub fn last(&self, name: &str) -> Option<&ParsedOption> {
        self.opts.iter().rev().find(|o| o.resolved().0 == name)
    }

    /// The value of a per-stream option for one stream.
    ///
    /// **Last match wins**, regardless of how specific the specifier is. That is
    /// the reference's rule and it surprises people:
    /// `-c:a:1 flac -c:a copy` gives stream `a:1` `copy`.
    ///
    /// A malformed specifier on any occurrence of `name` is an error, even when
    /// a later occurrence would have won — the reference parses them all.
    ///
    /// # Errors
    /// The specifier grammar's error.
    pub fn stream_option(
        &self,
        name: &str,
        ctx: &MatchCtx<'_>,
        stream: u32,
    ) -> Result<Option<&ParsedOption>> {
        let mut winner = None;
        for opt in &self.opts {
            if opt.resolved().0 != name {
                continue;
            }
            let spec = opt.stream_spec()?;
            let hit = match spec {
                Some(s) => s.matches(ctx, stream),
                None => true,
            };
            if hit {
                winner = Some(opt);
            }
        }
        Ok(winner)
    }
}

/// argv, split.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    /// Global options in argv order, wherever they were written.
    pub global: Vec<ParsedOption>,
    /// Input and output groups interleaved, in argv order.
    pub groups: Vec<OptionGroup>,
    /// Per-file options with no file after them.
    ///
    /// The reference discards these silently. They are surfaced here because a
    /// silently ignored `-c:v libx264` is a common and expensive mistake, and a
    /// caller may want to warn — but nothing in this crate treats them as an
    /// error, so the acceptance set is unchanged.
    pub orphaned: Vec<ParsedOption>,
}

impl CommandLine {
    /// Groups of one kind, in order.
    pub fn of_kind(&self, kind: GroupKind) -> impl Iterator<Item = &OptionGroup> {
        self.groups.iter().filter(move |g| g.kind == kind)
    }

    /// The last occurrence of a global option.
    #[must_use]
    pub fn last_global(&self, name: &str) -> Option<&ParsedOption> {
        self.global.iter().rev().find(|o| o.resolved().0 == name)
    }

    /// Check that every per-file option is on the right side of its file.
    ///
    /// Inputs are checked before outputs, matching the order the reference
    /// opens files in and therefore the order it reports failures in.
    ///
    /// # Errors
    /// [`CliError::WrongSide`] for the first offender.
    pub fn validate(&self) -> Result<()> {
        for want in [GroupKind::Input, GroupKind::Output] {
            for group in self.of_kind(want) {
                for opt in &group.opts {
                    let Some(desc) = opt.desc else { continue };
                    if !desc.allowed_on(group.kind == GroupKind::Output) {
                        return Err(OptTable::wrong_side(
                            desc,
                            group.kind == GroupKind::Output,
                            group.url.clone(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Split argv, deferring every unknown option name.
///
/// `argv` must **not** include the program name.
///
/// # Errors
/// [`CliError::MissingArgument`] and [`CliError::NonUtf8OptionName`].
pub fn split<S: AsRef<OsStr>>(table: &OptTable, argv: &[S]) -> Result<CommandLine> {
    split_with(table, argv, &AcceptUnknown)
}

/// Split argv, asking `oracle` about names the table does not have.
///
/// # Errors
/// [`CliError::MissingArgument`], [`CliError::UnrecognizedOption`],
/// [`CliError::NonUtf8OptionName`].
pub fn split_with<S: AsRef<OsStr>>(
    table: &OptTable,
    argv: &[S],
    oracle: &dyn AvOptionOracle,
) -> Result<CommandLine> {
    let mut out = CommandLine::default();
    let mut pending: Vec<ParsedOption> = Vec::new();
    let mut inputs = 0u32;
    let mut outputs = 0u32;
    let mut forced = false;
    let mut i = 0usize;

    while let Some(entry) = argv.get(i) {
        let arg = entry.as_ref();
        match classify(arg, forced) {
            Token::ForcePositional => {
                forced = true;
                i += 1;
                continue;
            }
            Token::Positional(url) => {
                forced = false;
                let kind = match table.positional {
                    Positional::OutputFile => GroupKind::Output,
                    Positional::InputFile => GroupKind::Input,
                };
                let index = bump(&mut inputs, &mut outputs, kind);
                out.groups.push(OptionGroup {
                    kind,
                    index,
                    url: url.to_owned(),
                    opts: core::mem::take(&mut pending),
                    argv_index: i,
                });
            }
            Token::NonUtf8Option(name) => {
                return Err(CliError::NonUtf8OptionName {
                    name: name.to_owned(),
                });
            }
            Token::Option(tok) => {
                let (desc, negated) = match table.resolve(tok.name) {
                    Lookup::Found { desc, negated } => (Some(desc), negated),
                    Lookup::Unknown => {
                        if !oracle.knows(tok.name) {
                            return Err(CliError::UnrecognizedOption {
                                name: OsString::from(tok.display_name()),
                            });
                        }
                        (None, false)
                    }
                };

                // A deferred option always takes a value: nothing knows whether
                // it is a flag, and the reference assumes it is not.
                let wants_value = desc.is_none_or(OptDesc::takes_value);
                let value = if wants_value {
                    let Some(v) = argv.get(i + 1) else {
                        return Err(CliError::MissingArgument {
                            name: tok.display_name(),
                        });
                    };
                    i += 1;
                    Some(v.as_ref().to_owned())
                } else {
                    None
                };

                let name = desc.map_or_else(|| tok.name.to_owned(), |d| d.name.to_owned());
                let parsed = ParsedOption {
                    name,
                    desc,
                    negated,
                    spec: tok.spec.map(str::to_owned),
                    value,
                    file_indirect: tok.file_indirect,
                    argv_index: i,
                };

                match desc {
                    Some(d) if d.flags.contains(OptFlags::GLOBAL) => out.global.push(parsed),
                    Some(d) if d.flags.contains(OptFlags::OPENS_INPUT) => {
                        let index = bump(&mut inputs, &mut outputs, GroupKind::Input);
                        out.groups.push(OptionGroup {
                            kind: GroupKind::Input,
                            index,
                            url: parsed.value.clone().unwrap_or_default(),
                            opts: core::mem::take(&mut pending),
                            argv_index: i,
                        });
                    }
                    _ => pending.push(parsed),
                }
            }
        }
        i += 1;
    }

    out.orphaned = pending;
    Ok(out)
}

fn bump(inputs: &mut u32, outputs: &mut u32, kind: GroupKind) -> u32 {
    let counter = match kind {
        GroupKind::Input => inputs,
        GroupKind::Output => outputs,
    };
    let index = *counter;
    *counter = counter.saturating_add(1);
    index
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use crate::stream::StreamInfo;
    use crate::table::{ffmpeg, ffprobe};
    use vaco_core::MediaType;

    fn ff(args: &[&str]) -> CommandLine {
        split(&ffmpeg(), args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    #[test]
    fn a_simple_transcode() {
        let cl = ff(&["-i", "in.mkv", "-c:v", "libx264", "out.mp4"]);
        assert_eq!(cl.groups.len(), 2);
        let input = cl.groups.first().unwrap();
        assert_eq!(input.kind, GroupKind::Input);
        assert_eq!(input.index, 0);
        assert_eq!(input.url, OsString::from("in.mkv"));
        assert!(input.opts.is_empty());
        let output = cl.groups.get(1).unwrap();
        assert_eq!(output.kind, GroupKind::Output);
        assert_eq!(output.url, OsString::from("out.mp4"));
        assert_eq!(output.opts.len(), 1);
        let c = output.opts.first().unwrap();
        assert_eq!(c.name, "c");
        assert_eq!(c.spec.as_deref(), Some("v"));
        assert_eq!(c.value, Some(OsString::from("libx264")));
    }

    #[test]
    fn per_file_options_bind_to_the_next_file() {
        let cl = ff(&["-t", "10", "-i", "a.mkv", "-i", "b.mkv", "out.mkv"]);
        assert_eq!(cl.groups.first().unwrap().opts.len(), 1);
        assert!(cl.groups.get(1).unwrap().opts.is_empty());
    }

    #[test]
    fn global_options_are_hoisted_from_anywhere() {
        for args in [
            ["-y", "-i", "a.mkv", "out.mkv"],
            ["-i", "a.mkv", "-y", "out.mkv"],
        ] {
            let cl = ff(&args);
            assert_eq!(cl.global.len(), 1, "{args:?}");
            assert_eq!(cl.global.first().unwrap().name, "y");
            for g in &cl.groups {
                assert!(g.opts.is_empty(), "{args:?}");
            }
        }
    }

    #[test]
    fn trailing_per_file_options_are_orphaned_not_fatal() {
        // Verified: ffmpeg 8.1 exits 0 and ignores these.
        let cl = ff(&["-i", "a.mkv", "-f", "null", "-", "-c:v", "libx264"]);
        assert_eq!(cl.orphaned.len(), 1);
        assert_eq!(cl.orphaned.first().unwrap().name, "c");
        assert!(cl.validate().is_ok());
    }

    #[test]
    fn output_may_precede_input() {
        let cl = ff(&["-y", "out.mkv", "-i", "in.mkv"]);
        assert_eq!(cl.groups.first().unwrap().kind, GroupKind::Output);
        assert_eq!(cl.groups.get(1).unwrap().kind, GroupKind::Input);
        assert!(cl.validate().is_ok());
    }

    #[test]
    fn group_indices_count_within_a_kind() {
        let cl = ff(&["-i", "a", "-i", "b", "o1", "o2", "-i", "c"]);
        let inputs: Vec<_> = cl.of_kind(GroupKind::Input).map(|g| g.index).collect();
        let outputs: Vec<_> = cl.of_kind(GroupKind::Output).map(|g| g.index).collect();
        assert_eq!(inputs, vec![0, 1, 2]);
        assert_eq!(outputs, vec![0, 1]);
    }

    #[test]
    fn values_are_never_relexed() {
        let cl = ff(&["-i", "a.mkv", "-metadata", "title=-y", "out.mkv"]);
        assert!(cl.global.is_empty());
        let m = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert_eq!(m.value, Some(OsString::from("title=-y")));
    }

    #[test]
    fn a_leading_dash_filename_still_works_after_i() {
        let cl = ff(&["-i", "-weird.mkv", "out.mkv"]);
        assert_eq!(cl.groups.first().unwrap().url, OsString::from("-weird.mkv"));
    }

    #[test]
    fn double_dash_forces_exactly_one_positional() {
        // `-i a -f null -- x1.mkv -f null -` makes x1.mkv an output and leaves
        // the following `-f null -` as a second output. Verified.
        let cl = ff(&["-i", "a", "-f", "null", "--", "x1.mkv", "-f", "null", "-"]);
        let outs: Vec<_> = cl
            .of_kind(GroupKind::Output)
            .map(|g| g.url.clone())
            .collect();
        assert_eq!(outs, vec![OsString::from("x1.mkv"), OsString::from("-")]);
        // `-f null` before the `--` bound to x1.mkv; the second bound to `-`.
        assert_eq!(cl.of_kind(GroupKind::Output).next().unwrap().opts.len(), 1);
    }

    #[test]
    fn a_trailing_double_dash_is_ignored() {
        let cl = ff(&["-i", "a", "-f", "null", "-", "--"]);
        assert_eq!(cl.of_kind(GroupKind::Output).count(), 1);
    }

    #[test]
    fn missing_argument() {
        assert_eq!(
            split(&ffmpeg(), &["-i"]),
            Err(CliError::MissingArgument { name: "i".into() })
        );
        assert_eq!(
            split(&ffmpeg(), &["-i", "a.mkv", "-c:v"]),
            Err(CliError::MissingArgument { name: "c:v".into() })
        );
    }

    #[test]
    fn unknown_options_depend_on_the_oracle() {
        // With no oracle, unknown names are deferred.
        let cl = ff(&["-i", "a", "-qwerty", "3", "out.mkv"]);
        let d = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert!(d.desc.is_none());
        assert_eq!(d.value, Some(OsString::from("3")));
        // With a rejecting oracle, they fail at split time, as the reference does.
        assert_eq!(
            split_with(
                &ffmpeg(),
                &["-i", "a", "-qwerty", "3", "out.mkv"],
                &RejectUnknown
            ),
            Err(CliError::UnrecognizedOption {
                name: OsString::from("qwerty")
            })
        );
        // ...and the name in the message keeps the specifier.
        assert_eq!(
            split_with(
                &ffmpeg(),
                &["-i", "a", "-foo:v", "bar", "out.mkv"],
                &RejectUnknown
            ),
            Err(CliError::UnrecognizedOption {
                name: OsString::from("foo:v")
            })
        );
    }

    #[test]
    fn wrong_side_is_diagnosed_after_grouping() {
        let cl = ff(&["-shortest", "-i", "a.mkv", "-f", "null", "-"]);
        let err = cl.validate().unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with("Option shortest ("), "{text}");
        assert!(
            text.contains("cannot be applied to input url a.mkv"),
            "{text}"
        );
        assert!(
            text.contains("Move this option before the file it belongs to."),
            "{text}"
        );

        let cl = ff(&["-i", "a.mkv", "-re", "-f", "null", "-"]);
        assert!(
            cl.validate()
                .unwrap_err()
                .to_string()
                .contains("cannot be applied to output url -")
        );
    }

    #[test]
    fn options_legal_on_both_sides_pass_validation() {
        for args in [
            vec!["-ss", "1", "-i", "a.mkv", "-f", "null", "-"],
            vec!["-i", "a.mkv", "-ss", "1", "-f", "null", "-"],
        ] {
            assert!(ff(&args).validate().is_ok(), "{args:?}");
        }
    }

    #[test]
    fn negation() {
        let cl = ff(&["-nostats", "-i", "a", "out.mkv"]);
        let s = cl.global.first().unwrap();
        assert_eq!(s.name, "stats");
        assert!(s.negated);
        assert!(s.value.is_none());
    }

    #[test]
    fn alias_rewriting() {
        let cl = ff(&["-i", "a", "-vf", "null", "out.mkv"]);
        let o = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert_eq!(o.resolved(), ("filter", Some("v")));
        let cl = ff(&["-i", "a", "-vcodec", "copy", "out.mkv"]);
        let o = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert_eq!(o.resolved(), ("c", Some("v")));
    }

    #[test]
    fn a_specifier_on_a_non_specifier_option_is_accepted_and_ignored() {
        // D17: `-y:vv` and `-t:zzz 1` are both accepted by the reference. The
        // suffix is stored but never parsed, so no grammar error can arise.
        let cl = ff(&["-y:vv", "-i", "a", "-t:zzz", "1", "out.mkv"]);
        assert_eq!(cl.global.first().unwrap().spec.as_deref(), Some("vv"));
        assert!(
            cl.groups
                .get(1)
                .unwrap()
                .opts
                .first()
                .unwrap()
                .stream_spec()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_specifier_on_a_per_stream_option_is_parsed() {
        let cl = ff(&["-i", "a", "-c:zzz", "copy", "out.mkv"]);
        let o = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert!(o.stream_spec().is_err());
    }

    #[test]
    fn last_match_wins_for_per_stream_options() {
        let streams = vec![
            StreamInfo {
                index: 0,
                media_type: Some(MediaType::Video),
                codec_known: true,
                width: 4,
                height: 4,
                ..StreamInfo::default()
            },
            StreamInfo {
                index: 1,
                media_type: Some(MediaType::Audio),
                codec_known: true,
                sample_rate: 48_000,
                ..StreamInfo::default()
            },
            StreamInfo {
                index: 2,
                media_type: Some(MediaType::Audio),
                codec_known: true,
                sample_rate: 48_000,
                ..StreamInfo::default()
            },
        ];
        let ctx = MatchCtx::streams(&streams);

        // `-c:a:1 flac -c:a copy` gives stream 2 `copy`: last match wins even
        // though `a:1` is the more specific specifier. Verified.
        let cl = ff(&["-i", "a", "-c:a:1", "flac", "-c:a", "copy", "out.mkv"]);
        let out = cl.groups.get(1).unwrap();
        let picked = out.stream_option("c", &ctx, 2).unwrap().unwrap();
        assert_eq!(picked.value, Some(OsString::from("copy")));

        // Reversed, the specific one wins because it is last.
        let cl = ff(&["-i", "a", "-c:a", "copy", "-c:a:1", "flac", "out.mkv"]);
        let out = cl.groups.get(1).unwrap();
        assert_eq!(
            out.stream_option("c", &ctx, 2).unwrap().unwrap().value,
            Some(OsString::from("flac"))
        );
        // ...and stream 1 still gets `copy`.
        assert_eq!(
            out.stream_option("c", &ctx, 1).unwrap().unwrap().value,
            Some(OsString::from("copy"))
        );
        // The video stream matches neither.
        assert!(out.stream_option("c", &ctx, 0).unwrap().is_none());
    }

    #[test]
    fn an_unspecified_per_stream_option_matches_every_stream() {
        let streams = vec![StreamInfo {
            media_type: Some(MediaType::Video),
            codec_known: true,
            width: 4,
            height: 4,
            ..StreamInfo::default()
        }];
        let ctx = MatchCtx::streams(&streams);
        let cl = ff(&["-i", "a", "-c", "copy", "out.mkv"]);
        let out = cl.groups.get(1).unwrap();
        assert!(out.stream_option("c", &ctx, 0).unwrap().is_some());
    }

    #[test]
    fn ffprobe_treats_a_bare_argument_as_the_input() {
        let cl = split(&ffprobe(), &["-show_format", "in.mkv"]).unwrap();
        assert_eq!(cl.groups.len(), 1);
        assert_eq!(cl.groups.first().unwrap().kind, GroupKind::Input);
        assert_eq!(cl.global.len(), 1);
        // `-i` opens an input group too.
        let cl = split(&ffprobe(), &["-i", "in.mkv"]).unwrap();
        assert_eq!(cl.groups.first().unwrap().url, OsString::from("in.mkv"));
    }

    #[test]
    fn metadata_specifiers_route_to_their_own_grammar() {
        let cl = ff(&["-i", "a", "-metadata:s:v:0", "title=x", "out.mkv"]);
        let o = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert!(o.stream_spec().unwrap().is_none());
        assert_eq!(
            o.metadata_spec().unwrap(),
            Some(MetadataSpecifier::Stream(
                StreamSpecifier::parse("v:0").unwrap()
            ))
        );
        // No specifier at all means global.
        let cl = ff(&["-i", "a", "-metadata", "title=x", "out.mkv"]);
        let o = cl.groups.get(1).unwrap().opts.first().unwrap();
        assert_eq!(o.metadata_spec().unwrap(), Some(MetadataSpecifier::Global));
    }

    #[test]
    fn non_utf8_option_name_is_rejected_with_the_bytes_kept() {
        let args: Vec<OsString> = vec![bad_option(), "-i".into(), "a".into()];
        let err = split(&ffmpeg(), &args).unwrap_err();
        assert!(matches!(err, CliError::NonUtf8OptionName { .. }));
        assert_eq!(err.raw_operand(), Some(&bad_option()));
    }

    #[test]
    fn non_utf8_values_and_urls_survive_verbatim() {
        let weird = bad_value();
        let args: Vec<OsString> = vec![
            "-i".into(),
            weird.clone(),
            "-metadata".into(),
            weird.clone(),
            "out.mkv".into(),
        ];
        let cl = split(&ffmpeg(), &args).unwrap();
        assert_eq!(cl.groups.first().unwrap().url, weird);
        assert_eq!(
            cl.groups.get(1).unwrap().opts.first().unwrap().value,
            Some(weird)
        );
    }

    #[cfg(unix)]
    fn bad_option() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'-', 0xff, 0xfe])
    }
    #[cfg(unix)]
    fn bad_value() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'k', b'=', 0xff, 0xfe])
    }
    #[cfg(windows)]
    fn bad_option() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'-'), 0xd800])
    }
    #[cfg(windows)]
    fn bad_value() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'k'), 0xd800])
    }
}
