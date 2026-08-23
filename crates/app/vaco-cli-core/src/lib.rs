//! The command-line machinery shared by `vaco`, `vaco-probe` and `vaco-play`.
//!
//! This crate turns an argument vector into a validated, scoped option
//! structure. It does not run a pipeline, open a file or touch a codec — its
//! whole job is to decide, exactly as the reference does, **what a command line
//! means**.
//!
//! # Why this is not `clap`
//!
//! `clap` models a flag set. The reference's command line is a *positional,
//! stateful stream of option groups over an option universe that is not known
//! until components are chosen*. Concretely:
//!
//! * `-b:v 1M` before `-i` and after `-i` are different options on different
//!   objects.
//! * `-c:v:0`, `-map 0:a:1?`, `-metadata:s:v title=x` embed a sub-language in
//!   the *name* token.
//! * `-crf 20` is valid only because some encoder declares `crf`; the parser
//!   must accept unknown names and audit them later.
//! * `-ac 1*2` is an error and `-crf 1*2` is 2 — the CLI has **two** numeric
//!   value grammars, and which one an option uses is not a property of its
//!   type. See [`value`].
//! * `-nostats` negates a boolean; `-noqwerty` is an error.
//! * `-/filter:v graph.txt` reads the value out of a file.
//!
//! Every one of those is survivable in isolation. Together they are a different
//! machine, and it is written out here rather than bent around a general parser.
//!
//! # The pipeline
//!
//! ```text
//! argv ──▶ [lex]      one entry at a time: option, positional, or `--`
//!      ──▶ [split]    global set + ordered per-file groups        (this crate)
//!      ──▶ [validate] every per-file option on the right side     (this crate)
//!      ──▶ [resolve]  per-stream values against a file's streams  (this crate)
//!      ──▶ [apply]    component options through `vaco-opts`       (the binary)
//! ```
//!
//! # Where to start reading
//!
//! * [`table`] — the **scope model**. Read this first.
//! * [`spec`] — the stream specifier grammar, which is stranger than the manual
//!   suggests.
//! * [`split`] — the grouping pass.
//! * [`value`] — the two numeric grammars, and the option dialect of the
//!   expression language.
//!
//! # Provenance
//!
//! Written clean-room (D7/D15). The command-line grammar is an *interface*, so
//! it was derived by black-box probing of ffmpeg 8.1 and from published
//! documentation, never from source. Every acceptance and rejection asserted in
//! this crate's tests was observed from the shipped binary; the method is
//! recorded in `docs/app/vaco-cli-core.md`.
//!
//! Per D9, option **names** are interface facts and are reproduced; help **text**
//! is not, and every help string here was written for Vaco. `-h` output
//! consequently cannot be byte-identical, which is an accepted project-level
//! consequence, not an oversight.
//!
//! # D17 deviations reproduced deliberately
//!
//! Each is annotated at its site with a `// D17:` comment. The list, so a
//! reviewer can find them:
//!
//! | Where | The reference does | We do the same because |
//! |---|---|---|
//! | [`table::OptDesc::spec_kind`] | accepts `-y:vv`, `-t:zzz` — a specifier on an option that has none, never validated | rejecting them would break working command lines |
//! | [`metaspec`] | `-metadata:gg` and `-metadata:g:0` mean "global"; the tail is never read | same |
//! | [`metaspec`] | `-metadata:c:x` is chapter 0, not an error | same |
//! | [`map`] | `-map [v` needs no closing bracket | same |
//! | [`error::SpecError::MultipleProgramOrGroup`] | prints without a trailing newline, so the next log line runs into it | stderr is compared byte for byte (D6) |
//! | [`spec`] | prints `Parsed 'usable only'` at error level on success | see the note below |
//! | [`value`] | accepts `-ac ""` as zero while rejecting `-ac " "` | C sets `endptr = nptr`, so only the first has an empty tail |
//! | [`value`] | prints an `int64` bound as `9223372036854775808`, one too high | the bound goes through a `double` before `%f` |
//! | [`value`] | `-crf max(1,2)` is a parse error | `max` is a *constant* on the option path and shadows the builtin |
//!
//! That last one is a message the reference emits *on the success path* at
//! `AV_LOG_ERROR` whenever a specifier contains `u`. It is left to the binary
//! to reproduce, since this crate does not log; [`spec::StreamSpecifier::usable`]
//! is the trigger.

#![forbid(unsafe_code)]

pub mod error;
pub mod help;
pub mod lex;
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
