//! The option descriptor table: what an option is called, whether it takes a
//! value, and — the part that matters — **where it is allowed to appear**.
//!
//! # The scope model
//!
//! This is the design decision the rest of the crate is built on, so it is
//! stated here rather than left implicit.
//!
//! Every option sits in exactly one of three scopes:
//!
//! | Scope | Flag | Where it binds |
//! |---|---|---|
//! | **Global** | [`OptFlags::GLOBAL`] | The whole run. Position is irrelevant: `vaco -y -i a out` and `vaco -i a -y out` are the same command. |
//! | **Per-file** | [`OptFlags::PER_FILE`] | The **next** file mentioned. `-t 10 -i a -i b` limits `a` only. |
//! | **Per-stream** | `PER_FILE \| PER_STREAM` | A per-file option that additionally carries a stream specifier, so one file can hold several values of it. |
//!
//! Per-file options additionally carry [`OptFlags::INPUT`] and/or
//! [`OptFlags::OUTPUT`], which decide which *kind* of file they may bind to.
//! Getting that wrong is the reference's most user-visible diagnostic:
//!
//! ```text
//! Option shortest (finish encoding within shortest input) cannot be applied to
//! input url in.mkv -- you are trying to apply an input option to an output file
//! or vice versa. Move this option before the file it belongs to.
//! ```
//!
//! Two consequences worth knowing before reading the parser:
//!
//! * **Scope is checked after grouping, never during.** A misplaced per-file
//!   option is a *file-opening* failure, not a *splitting* failure, and the two
//!   phases print different follow-up lines and exit with different statuses.
//!   See [`crate::error::Phase`].
//! * **Per-stream is not a fourth scope.** It is a per-file option whose value
//!   is a list of (specifier, value) pairs. Resolution against a concrete stream
//!   happens once the file's streams are known, and the **last** matching
//!   occurrence wins regardless of how specific it is — `-c:a:1 flac -c:a copy`
//!   gives stream `a:1` `copy`, not `flac`. Verified against ffmpeg 8.1.
//!
//! # Where this table comes from
//!
//! The names, argument names, scopes and specifier kinds were extracted
//! mechanically from `ffmpeg -h full` / `ffprobe -h full` and then each option's
//! input/output-ness was **re-derived by probing** — running it before and after
//! `-i` and looking for the mismatch diagnostic. The extraction and probe
//! scripts are described in `docs/app/vaco-cli-core.md`.
//!
//! The help strings are **not** the reference's. D9 permits reproducing option
//! *names* (interface facts) but not help *text*, so every string here was
//! written independently. That means `-h` output cannot be byte-identical, which
//! is a known and accepted consequence.

use crate::error::CliError;

/// What an option is allowed to do and where it may appear.
///
/// A hand-rolled bit set rather than `bitflags`: this crate has no external
/// dependencies and adding one would need a D10 review for eleven lines of code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OptFlags(u32);

/// The raw bit values.
///
/// A separate module because `BitOr` cannot be `const` on stable, and the
/// tables need `A | B` inside a `static`. Public API uses [`OptFlags`].
mod bit {
    #![allow(
        unreachable_pub,
        reason = "an internal constant table; the public spelling is OptFlags"
    )]
    /// Consumes the following argv entry as its value.
    pub const HAS_ARG: u32 = 1 << 0;
    /// Applies to the whole run; never binds to a file.
    pub const GLOBAL: u32 = 1 << 1;
    /// Binds to the next file.
    pub const PER_FILE: u32 = 1 << 2;
    /// May bind to an input file.
    pub const INPUT: u32 = 1 << 3;
    /// May bind to an output file.
    pub const OUTPUT: u32 = 1 << 4;
    /// Takes a stream specifier and stores one value per matching stream.
    pub const PER_STREAM: u32 = 1 << 5;
    /// Takes a specifier that is *not* a stream specifier.
    pub const TAKES_SPEC: u32 = 1 << 6;
    /// Hidden from plain `-h`.
    pub const EXPERT: u32 = 1 << 7;
    /// Prints something and exits.
    pub const EXIT: u32 = 1 << 8;
    /// Its value opens a new input file group.
    pub const OPENS_INPUT: u32 = 1 << 9;
    pub const VIDEO: u32 = 1 << 10;
    pub const AUDIO: u32 = 1 << 11;
    pub const SUBTITLE: u32 = 1 << 12;
    pub const DATA: u32 = 1 << 13;
}

impl OptFlags {
    pub const NONE: Self = Self(0);
    /// Consumes the following argv entry as its value.
    pub const HAS_ARG: Self = Self(bit::HAS_ARG);
    /// Applies to the whole run; never binds to a file.
    pub const GLOBAL: Self = Self(bit::GLOBAL);
    /// Binds to the next file.
    pub const PER_FILE: Self = Self(bit::PER_FILE);
    /// May bind to an input file.
    pub const INPUT: Self = Self(bit::INPUT);
    /// May bind to an output file.
    pub const OUTPUT: Self = Self(bit::OUTPUT);
    /// Takes a stream specifier and stores one value per matching stream.
    pub const PER_STREAM: Self = Self(bit::PER_STREAM);
    /// Takes a specifier that is *not* a stream specifier — a metadata
    /// specifier, today. Distinguished in the reference's own help output,
    /// which writes `[:<spec>]` here and `[:<stream_spec>]` for `PER_STREAM`.
    pub const TAKES_SPEC: Self = Self(bit::TAKES_SPEC);
    /// Hidden from plain `-h`; shown by `-h long` and `-h full`.
    pub const EXPERT: Self = Self(bit::EXPERT);
    /// Prints something and exits without processing any file.
    pub const EXIT: Self = Self(bit::EXIT);
    /// Its value opens a new input file group.
    pub const OPENS_INPUT: Self = Self(bit::OPENS_INPUT);
    /// Help-grouping only.
    pub const VIDEO: Self = Self(bit::VIDEO);
    pub const AUDIO: Self = Self(bit::AUDIO);
    pub const SUBTITLE: Self = Self(bit::SUBTITLE);
    pub const DATA: Self = Self(bit::DATA);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl core::ops::BitOr for OptFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Which grammar an option's `:suffix` is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    /// No specifier is meaningful. The reference still *accepts* one and throws
    /// it away — see [`OptDesc::spec_kind`].
    None,
    /// [`crate::spec::StreamSpecifier`].
    Stream,
    /// [`crate::metaspec::MetadataSpecifier`].
    Metadata,
}

/// One option's static description.
///
/// `PartialEq`/`Eq` compare by name, which is unique within a table, so a
/// descriptor reference can be compared without comparing help text.
#[derive(Debug, Clone, Copy, Eq)]
pub struct OptDesc {
    pub name: &'static str,
    /// The placeholder `-h` prints for the value, when there is one.
    pub argname: Option<&'static str>,
    pub flags: OptFlags,
    /// Written for Vaco, not transcribed from the reference (D9).
    pub help: &'static str,
    /// Non-`None` when this name is a spelling of another option with a
    /// specifier baked in: `-vf x` is `-filter:v x`, `-ab x` is `-b:a x`.
    pub alias_of: Option<(&'static str, &'static str)>,
}

impl PartialEq for OptDesc {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl OptDesc {
    #[must_use]
    pub const fn takes_value(&self) -> bool {
        self.flags.contains(OptFlags::HAS_ARG)
    }

    /// Which specifier grammar applies to this option's `:suffix`.
    ///
    /// D17: options that return [`SpecKind::None`] still *accept* a suffix. The
    /// reference splits the name at the first colon, looks up the part before
    /// it, and simply never reads the rest for a non-specifier option — so
    /// `-y:vv`, `-t:zzz 1` and `-vn:v` are all accepted silently. That is not a
    /// sane grammar, but rejecting them would break working command lines, so
    /// the suffix is parsed only when this says it means something.
    #[must_use]
    pub const fn spec_kind(&self) -> SpecKind {
        if self.flags.contains(OptFlags::PER_STREAM) {
            SpecKind::Stream
        } else if self.flags.contains(OptFlags::TAKES_SPEC) {
            // Every `[:<spec>]` option in ffmpeg 8.1 either takes a metadata
            // specifier (`-metadata`, `-map_metadata`) or a stream specifier
            // used for selection rather than for per-stream storage
            // (`-dump_attachment`, `-program`, `-stream_group`). The consumer
            // decides; the lexer keeps the text.
            SpecKind::Metadata
        } else {
            SpecKind::None
        }
    }

    /// Whether this option may legally bind to a file of the given kind.
    #[must_use]
    pub const fn allowed_on(&self, output: bool) -> bool {
        if output {
            self.flags.contains(OptFlags::OUTPUT)
        } else {
            self.flags.contains(OptFlags::INPUT)
        }
    }
}

/// One tool's complete option set.
#[derive(Debug, Clone, Copy)]
pub struct OptTable {
    pub tool: &'static str,
    pub options: &'static [OptDesc],
    /// What a bare, non-option argument means for this tool. `vaco` treats it
    /// as an output file; `vaco-probe` treats it as the input.
    pub positional: Positional,
}

/// What a bare argument opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Positional {
    /// ffmpeg: a bare argument is an output file.
    OutputFile,
    /// ffprobe / ffplay: a bare argument is the input.
    InputFile,
}

/// The outcome of looking a lexed name up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// Found, possibly through the `no` prefix.
    Found {
        desc: &'static OptDesc,
        negated: bool,
    },
    /// Not in this table. It may still be an `AVOption` on a component that has
    /// not been chosen yet, which is what [`crate::split::AvOptionOracle`] is
    /// for.
    Unknown,
}

impl OptTable {
    /// Exact-name lookup, no `no` prefix.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&'static OptDesc> {
        self.options.iter().find(|o| o.name == name)
    }

    /// Lookup including the `no` prefix.
    ///
    /// The reference negates only its **own** booleans, never an `AVOption`, and
    /// only when the un-prefixed name is not itself an option. `-nostats` is
    /// `stats=false`; `-noqwerty` is unrecognised.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Lookup {
        if let Some(desc) = self.find(name) {
            return Lookup::Found {
                desc,
                negated: false,
            };
        }
        if let Some(base) = name.strip_prefix("no")
            && let Some(desc) = self.find(base)
            && !desc.takes_value()
        {
            return Lookup::Found {
                desc,
                negated: true,
            };
        }
        Lookup::Unknown
    }

    /// Every option, for the help system.
    pub fn iter(&self) -> impl Iterator<Item = &'static OptDesc> {
        self.options.iter()
    }

    /// Build the reference's scope-mismatch diagnostic for `desc` on a file of
    /// the given kind.
    #[must_use]
    pub fn wrong_side(desc: &'static OptDesc, output: bool, url: std::ffi::OsString) -> CliError {
        CliError::WrongSide {
            name: desc.name.to_owned(),
            help: desc.help,
            output,
            url,
        }
    }
}

const fn o(
    name: &'static str,
    argname: Option<&'static str>,
    flags: u32,
    help: &'static str,
) -> OptDesc {
    OptDesc {
        name,
        argname,
        flags: OptFlags(flags),
        help,
        alias_of: None,
    }
}

const fn alias(
    name: &'static str,
    argname: Option<&'static str>,
    flags: u32,
    help: &'static str,
    target: &'static str,
    spec: &'static str,
) -> OptDesc {
    OptDesc {
        name,
        argname,
        flags: OptFlags(flags),
        help,
        alias_of: Some((target, spec)),
    }
}

/// The `vaco` (ffmpeg-equivalent) option table.
#[must_use]
pub const fn ffmpeg() -> OptTable {
    OptTable {
        tool: "vaco",
        options: FFMPEG_OPTIONS,
        positional: Positional::OutputFile,
    }
}

/// The `vaco-probe` (ffprobe-equivalent) option table.
#[must_use]
pub const fn ffprobe() -> OptTable {
    OptTable {
        tool: "vaco-probe",
        options: FFPROBE_OPTIONS,
        positional: Positional::InputFile,
    }
}

include!("tables/ffmpeg.rs");
include!("tables/ffprobe.rs");

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        for table in [ffmpeg(), ffprobe()] {
            let mut names: Vec<_> = table.options.iter().map(|o| o.name).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(before, names.len(), "duplicate name in {}", table.tool);
        }
    }

    #[test]
    fn every_option_has_exactly_one_scope() {
        for table in [ffmpeg(), ffprobe()] {
            for o in table.options {
                let global = o.flags.contains(OptFlags::GLOBAL);
                let per_file = o.flags.contains(OptFlags::PER_FILE);
                assert!(
                    global ^ per_file,
                    "{}/{} must be exactly one of GLOBAL and PER_FILE",
                    table.tool,
                    o.name
                );
                if per_file {
                    assert!(
                        o.flags.intersects(OptFlags::INPUT | OptFlags::OUTPUT),
                        "{}/{} is per-file but neither INPUT nor OUTPUT",
                        table.tool,
                        o.name
                    );
                }
                if global {
                    assert!(
                        !o.flags.intersects(OptFlags::INPUT | OptFlags::OUTPUT),
                        "{}/{} is global but carries a side",
                        table.tool,
                        o.name
                    );
                }
            }
        }
    }

    #[test]
    fn per_stream_implies_per_file() {
        for o in ffmpeg().options {
            if o.flags.contains(OptFlags::PER_STREAM) {
                assert!(o.flags.contains(OptFlags::PER_FILE), "{}", o.name);
            }
        }
    }

    #[test]
    fn every_option_has_help() {
        for table in [ffmpeg(), ffprobe()] {
            for o in table.options {
                assert!(!o.help.is_empty(), "{}/{} has no help", table.tool, o.name);
            }
        }
    }

    #[test]
    fn spot_check_against_the_reference() {
        let t = ffmpeg();
        // Scope, from the probe transcript.
        let re = t.find("re").unwrap();
        assert!(re.flags.contains(OptFlags::INPUT) && !re.flags.contains(OptFlags::OUTPUT));
        let shortest = t.find("shortest").unwrap();
        assert!(
            shortest.flags.contains(OptFlags::OUTPUT) && !shortest.flags.contains(OptFlags::INPUT)
        );
        let ss = t.find("ss").unwrap();
        assert!(ss.flags.contains(OptFlags::INPUT) && ss.flags.contains(OptFlags::OUTPUT));
        assert!(t.find("y").unwrap().flags.contains(OptFlags::GLOBAL));
        // Specifier kinds.
        assert_eq!(t.find("c").unwrap().spec_kind(), SpecKind::Stream);
        assert_eq!(t.find("metadata").unwrap().spec_kind(), SpecKind::Metadata);
        assert_eq!(t.find("f").unwrap().spec_kind(), SpecKind::None);
        // Argument-taking, from the `Missing argument` probe.
        assert!(t.find("thread_queue_size").unwrap().takes_value());
        assert!(!t.find("autorotate").unwrap().takes_value());
        assert!(!t.find("shortest").unwrap().takes_value());
        assert!(t.find("shortest_buf_duration").unwrap().takes_value());
    }

    #[test]
    fn no_prefix_negation() {
        let t = ffmpeg();
        assert!(matches!(
            t.resolve("nostats"),
            Lookup::Found { negated: true, .. }
        ));
        assert!(matches!(
            t.resolve("novn"),
            Lookup::Found { negated: true, .. }
        ));
        // `-not` is not `-t` negated, because `-t` takes a value.
        assert_eq!(t.resolve("not"), Lookup::Unknown);
        assert_eq!(t.resolve("noqwerty"), Lookup::Unknown);
        // An option whose own name starts with `no` resolves to itself.
        assert!(matches!(
            t.resolve("n"),
            Lookup::Found { negated: false, .. }
        ));
    }

    #[test]
    fn double_dash_help_is_its_own_entry_not_a_prefix_rule() {
        // D17: `--help` works but `--y` does not. The reference has a literal
        // `-help` entry (reached as `--help`); there is no general `--` prefix.
        let t = ffmpeg();
        assert!(t.find("-help").is_some());
        assert!(t.find("-y").is_none());
    }

    #[test]
    fn ffprobe_positional_is_the_input() {
        assert_eq!(ffprobe().positional, Positional::InputFile);
        assert_eq!(ffmpeg().positional, Positional::OutputFile);
    }
}
