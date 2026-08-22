//! The error taxonomy, and the reference's exact message text.
//!
//! Error text is **observable output** (D6), so every `Display` impl here is a
//! recorded transcription of what ffmpeg 8.1 prints, not a rewording. Where the
//! reference's message is defective — a missing newline, an empty interpolation
//! — the defect is reproduced and annotated with a `// D17:` comment.
//!
//! Messages are formatted with `String::from_utf8_lossy` semantics for the
//! non-UTF-8 case: the raw bytes are kept in the error so a caller that wants
//! byte-identical stderr can write them itself (see [`CliError::raw_operand`]).

use std::ffi::OsString;

use thiserror::Error;

/// Everything the stream-specifier grammar can reject.
///
/// The strings are the reference's, verbatim. `SpecError::TrailingGarbage` is
/// by far the most common: the reference's parser consumes what it can and then
/// complains about whatever is left, so the *remainder* — not the whole
/// specifier — is what appears in the message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SpecError {
    #[error("Trailing garbage at the end of a stream specifier: {rest}")]
    TrailingGarbage { rest: String },

    #[error("Stream type specified multiple times")]
    DuplicateType,

    // D17: the reference emits this one WITHOUT a trailing newline, so the next
    // log line runs into it: "…stream specifierError parsing options for output
    // file -.". Callers that reproduce stderr byte for byte must suppress the
    // newline for this variant only; `Display` here carries no newline either
    // way, so the property is a printing decision, not a message decision. Do
    // not "fix" the reference by adding punctuation.
    #[error("Cannot combine multiple program/group designators in a single stream specifier")]
    MultipleProgramOrGroup,

    #[error("Multiple disposition specifiers")]
    MultipleDisposition,

    #[error("Expected program ID, got: {rest}")]
    ExpectedProgramId { rest: String },

    #[error("Expected stream group idx/ID, got: {rest}")]
    ExpectedGroupRef { rest: String },

    #[error("Expected stream ID, got: {rest}")]
    ExpectedStreamId { rest: String },

    /// The reference reaches this after its expression evaluator has already
    /// printed two lines of its own; we print only the final, stable one.
    #[error("Invalid disposition specifier")]
    InvalidDisposition { text: String },
}

/// Everything the command line itself can reject.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CliError {
    /// An option name present in no table and claimed by no component.
    ///
    /// The reference prints the name *including* any specifier suffix:
    /// `-foo:v bar` yields `Unrecognized option 'foo:v'.`
    #[error("Unrecognized option '{}'.", .name.to_string_lossy())]
    UnrecognizedOption { name: OsString },

    /// The reference likewise includes the specifier: `Missing argument for
    /// option 'c:v'.`
    #[error("Missing argument for option '{name}'.")]
    MissingArgument { name: String },

    #[error(transparent)]
    Spec(#[from] SpecError),

    /// `-map` runs the specifier parser in "consume what you can" mode and then
    /// checks the remainder itself, which is why its message differs by one
    /// word from [`SpecError::TrailingGarbage`]: "after" rather than "at the
    /// end of a".
    #[error("Trailing garbage after stream specifier: {rest}")]
    MapTrailingGarbage { rest: String },

    /// `-map` wraps a specifier failure in a line of its own, naming the text
    /// it handed to the specifier parser — including the leading colon, if the
    /// user wrote one. The reference prints the inner failure first and this
    /// line second, so [`CliError::inner_spec`] hands the inner one back.
    #[error("Invalid stream specifier: {text}")]
    InvalidStreamSpecifier { text: String, inner: SpecError },

    /// Emitted alongside the underlying failure, as the reference does.
    #[error("Failed to set value '{}' for option '{option}': Invalid argument", .value.to_string_lossy())]
    OptionValueRejected { option: String, value: OsString },

    /// A per-file option that landed on the wrong side of its file.
    ///
    /// The reference's wording is long and specific; scripts grep for it.
    #[error(
        "Option {name} ({help}) cannot be applied to {} url {} -- you are trying to apply an input option to an output file or vice versa. Move this option before the file it belongs to.",
        if *.output { "output" } else { "input" },
        .url.to_string_lossy()
    )]
    WrongSide {
        name: String,
        help: &'static str,
        output: bool,
        url: OsString,
    },

    /// `-/opt path` could not read `path`.
    #[error("Error reading the value for option '{option}' from file: {}", .path.to_string_lossy())]
    IndirectionFailed { option: String, path: OsString },

    /// A typed value that the option's grammar rejected. The reference's text
    /// varies per grammar (`Invalid duration for option t: …`), so the grammar
    /// name is carried rather than baked in.
    #[error("Invalid {kind} for option {option}: {}", .value.to_string_lossy())]
    InvalidValue {
        option: String,
        kind: &'static str,
        value: OsString,
    },

    /// An option name or specifier that is not valid UTF-8.
    ///
    /// Deliberate divergence, documented in `docs/app/vaco-cli-core.md`: the
    /// reference is byte-oriented and would carry the raw bytes into whatever
    /// lookup follows (always failing, since no option name contains them).
    /// We reject at the boundary instead, keeping the raw bytes for printing.
    #[error("Unrecognized option '{}'.", .name.to_string_lossy())]
    NonUtf8OptionName { name: OsString },
}

impl CliError {
    /// The raw, possibly non-UTF-8 operand this error is about.
    ///
    /// `Display` renders it lossily. A caller that wants byte-identical stderr
    /// writes these bytes instead of the rendered form.
    /// The specifier failure underlying a `-map` rejection, which the reference
    /// prints on the line *before* this error's own text.
    #[must_use]
    pub const fn inner_spec(&self) -> Option<&SpecError> {
        match self {
            Self::Spec(e) | Self::InvalidStreamSpecifier { inner: e, .. } => Some(e),
            _ => None,
        }
    }

    #[must_use]
    pub fn raw_operand(&self) -> Option<&OsString> {
        match self {
            Self::UnrecognizedOption { name } | Self::NonUtf8OptionName { name } => Some(name),
            Self::OptionValueRejected { value, .. } | Self::InvalidValue { value, .. } => {
                Some(value)
            }
            Self::WrongSide { url, .. } => Some(url),
            Self::IndirectionFailed { path, .. } => Some(path),
            Self::MissingArgument { .. }
            | Self::Spec(_)
            | Self::InvalidStreamSpecifier { .. }
            | Self::MapTrailingGarbage { .. } => None,
        }
    }

    /// The second line the reference prints after a split-phase failure.
    ///
    /// The split phase and the file-opening phase have different follow-up
    /// lines; this names which one applies so the binary can reproduce the pair.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        match self {
            Self::UnrecognizedOption { .. } | Self::NonUtf8OptionName { .. } => {
                Phase::SplitNotFound
            }
            Self::MissingArgument { .. } => Phase::SplitInvalid,
            Self::Spec(_)
            | Self::InvalidStreamSpecifier { .. }
            | Self::MapTrailingGarbage { .. }
            | Self::OptionValueRejected { .. }
            | Self::WrongSide { .. }
            | Self::IndirectionFailed { .. }
            | Self::InvalidValue { .. } => Phase::OpenFile,
        }
    }
}

/// Which of the reference's two failure phases an error belongs to.
///
/// Observable, because each phase prints a distinct follow-up line and exits
/// with a distinct status:
///
/// | Phase | Follow-up line | Exit status |
/// |---|---|---|
/// | `SplitNotFound` | `Error splitting the argument list: Option not found` | 8 |
/// | `SplitInvalid` | `Error splitting the argument list: Invalid argument` | 234 |
/// | `OpenFile` | `Error parsing options for output file X.` | 234 |
///
/// 8 and 234 are `AVERROR_OPTION_NOT_FOUND` and `AVERROR(EINVAL)` truncated to
/// a process exit status by `exit(3)`; they are not chosen, they fall out of
/// returning a negative errno from `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    SplitNotFound,
    SplitInvalid,
    OpenFile,
}

impl Phase {
    /// The process exit status the reference produces for this phase.
    #[must_use]
    pub const fn exit_status(self) -> i32 {
        match self {
            Self::SplitNotFound => 8,
            Self::SplitInvalid | Self::OpenFile => 234,
        }
    }
}

/// `Result` with this crate's error.
pub type Result<T> = core::result::Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_match_the_reference_transcripts() {
        assert_eq!(
            SpecError::TrailingGarbage { rest: "vv".into() }.to_string(),
            "Trailing garbage at the end of a stream specifier: vv"
        );
        assert_eq!(
            SpecError::ExpectedProgramId { rest: "x".into() }.to_string(),
            "Expected program ID, got: x"
        );
        assert_eq!(
            SpecError::ExpectedGroupRef {
                rest: String::new()
            }
            .to_string(),
            "Expected stream group idx/ID, got: "
        );
        assert_eq!(
            SpecError::DuplicateType.to_string(),
            "Stream type specified multiple times"
        );
        assert_eq!(
            CliError::MissingArgument { name: "c:v".into() }.to_string(),
            "Missing argument for option 'c:v'."
        );
        assert_eq!(
            CliError::UnrecognizedOption {
                name: "foo:v".into()
            }
            .to_string(),
            "Unrecognized option 'foo:v'."
        );
        assert_eq!(
            CliError::MapTrailingGarbage {
                rest: ",1:a".into()
            }
            .to_string(),
            "Trailing garbage after stream specifier: ,1:a"
        );
    }

    #[test]
    fn exit_statuses_are_the_reference_ones() {
        assert_eq!(
            CliError::UnrecognizedOption { name: "z".into() }
                .phase()
                .exit_status(),
            8
        );
        assert_eq!(
            CliError::MissingArgument { name: "i".into() }
                .phase()
                .exit_status(),
            234
        );
    }
}
