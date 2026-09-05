//! The command-line machinery shared by `vaco`, `vaco-probe` and `vaco-play`.
//!
//! This crate turns an argument vector into validated, scoped options. It does
//! not run a pipeline, open a file, or touch a codec: its job is to decide what
//! a command line means, matching the reference's stateful option groups.
//!
//! # Why this is not `clap`
//!
//! The option universe depends on the selected components, and option names
//! carry stream-specifier sublanguages. Values also use two grammars: `-ac 1*2`
//! is invalid while `-crf 1*2` evaluates to 2. See [`value`].
//!
//! # Pipeline
//!
//! ```text
//! argv ──▶ lex ──▶ split ──▶ validate ──▶ resolve ──▶ apply through `vaco-opts`
//! ```
//!
//! [`table`] defines scope, [`spec`] parses stream specifiers, [`split`] groups
//! per-file options, and [`value`] owns the numeric grammars.
//!
//! # Provenance
//!
//! The interface was derived clean-room from published documentation and
//! black-box probes of ffmpeg 8.1 (D7/D15), never source; the method is recorded
//! in `docs/app/vaco-cli-core.md`. Option names are reproduced as interface
//! facts, while help text is Vaco-authored, so `-h` is not byte-identical (D9).
//!
//! D17 deviations are retained deliberately, each marked at its implementation
//! site with `// D17:` and summarized in `docs/app/vaco-cli-core.md`. They cover
//! permissive specifiers, exact diagnostic text, and the two numeric parsers;
//! changing one requires a fresh reference measurement.

#![forbid(unsafe_code)]

// The derive macro this crate uses for `tables/ffmpeg.rs`/`tables/ffprobe.rs`
// expands to `::vaco_cli_core::…` paths, matching `vaco_opts`'s own
// self-reference precedent; this makes those resolve inside this crate too.
#[allow(unused_extern_crates)]
extern crate self as vaco_cli_core;

pub mod error;
pub mod help;
pub mod lex;
pub mod loglevel;
pub mod map;
pub mod metaspec;
pub mod num;
pub mod spec;
pub mod split;
pub mod stream;
pub mod table;
pub mod value;

pub use error::{CliError, Phase, Result, SpecError};
pub use help::{
    HelpLevel, KindTopic, Topic, parse_topic, render_options_help, render_schema_block,
};
pub use lex::{NameToken, Token, classify};
pub use map::{FileMap, MapSpec};
pub use metaspec::{MetaSpecError, MetadataSpecifier};
pub use spec::{GroupRef, ParseMode, SpecMediaKind, StreamSpecifier};
pub use split::{
    AcceptUnknown, AvOptionOracle, CommandLine, GroupKind, OptionGroup, ParsedOption,
    RejectUnknown, split, split_with,
};
pub use stream::{Disposition, GroupInfo, MatchCtx, ProgramInfo, StreamInfo};
pub use table::{ArgFlags, Lookup, OptDesc, OptTable, Positional, SpecKind, ffmpeg, ffprobe};
pub use value::{
    Expression, NumberLimits, OptionConstants, ValueKind, eval_checked, eval_once, eval_option,
    parse_number, strtod,
};
