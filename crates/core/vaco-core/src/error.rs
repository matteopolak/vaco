//! The error taxonomy.
//!
//! Deliberately a closed enum rather than a boxed trait object: callers need to
//! distinguish "this input is malformed" (skip the packet, keep going) from
//! "this file is truncated" (stop) from "this feature is not implemented"
//! (report to the user), and a string cannot be matched on.

use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The bitstream, container or option value is malformed.
    ///
    /// This is the single most common error in a media tool and is usually
    /// recoverable: a decoder skips the packet and resynchronises.
    InvalidData(&'static str),

    /// Input ended before a complete unit could be read.
    UnexpectedEof,

    /// End of stream, reached normally. Not a failure.
    Eof,

    /// The operation needs more input before it can produce output.
    ///
    /// Part of the send/receive contract, not a failure — see
    /// `vaco_codec_core::Decoder`.
    NeedMoreInput,

    /// Output is available and must be drained before more input is accepted.
    OutputPending,

    /// A real feature that exists in the specification and that we have not
    /// implemented. Distinct from `InvalidData`: the input is fine, we are not.
    Unsupported(&'static str),

    /// An allocation would exceed the budget for this operation.
    ///
    /// Attacker-controlled sizes are the main memory-safety-adjacent risk in a
    /// safe-Rust media stack, so exceeding a budget is a first-class error
    /// rather than an abort. See `vaco-limits`.
    LimitExceeded {
        limit: &'static str,
        requested: u64,
        cap: u64,
    },

    /// A named option was not recognised, or its value did not parse.
    Option { name: String, detail: String },

    /// Underlying I/O failure.
    Io(std::io::Error),

    /// The requested seek target is not reachable in this stream.
    NotSeekable,

    /// The operation was cancelled through a [`crate::CancelToken`].
    ///
    /// Its own variant rather than `Io(ErrorKind::Interrupted)`, which is what
    /// this used to be. `Interrupted` is the one kind the standard library
    /// tells you to retry, and cancellation is precisely the signal that must
    /// not be retried — see [`crate::cancel`].
    Cancelled,
}

impl Error {
    /// Whether a component may reasonably continue after this error.
    ///
    /// Used by the demux and decode loops to decide between skipping a packet
    /// and aborting the stream.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(self, Self::InvalidData(_) | Self::Unsupported(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(w) => write!(f, "invalid data: {w}"),
            Self::UnexpectedEof => f.write_str("unexpected end of input"),
            Self::Eof => f.write_str("end of stream"),
            Self::NeedMoreInput => f.write_str("more input required"),
            Self::OutputPending => f.write_str("output must be drained first"),
            Self::Unsupported(w) => write!(f, "unsupported: {w}"),
            Self::LimitExceeded {
                limit,
                requested,
                cap,
            } => {
                write!(
                    f,
                    "{limit} limit exceeded: requested {requested}, cap {cap}"
                )
            }
            Self::Option { name, detail } => write!(f, "option `{name}`: {detail}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::NotSeekable => f.write_str("stream is not seekable"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::UnexpectedEof
        } else {
            Self::Io(e)
        }
    }
}

/// Expression parse failures surface as option errors.
///
/// This impl lives here rather than in `vaco-expr` because it was that crate's
/// only reason to depend on `vaco-core`, and that single edge blocked
/// `vaco-core` from using the evaluator at all — which it needs, since the
/// reference's ratio grammar is expression-backed. The orphan rule permits the
/// impl on either side; putting it on the `Error` side leaves `vaco-expr` a leaf
/// with no Vaco dependencies but `vaco-time`.
impl From<vaco_expr::ParseError> for Error {
    fn from(e: vaco_expr::ParseError) -> Self {
        Self::Option {
            name: "expr".to_owned(),
            detail: e.to_string(),
        }
    }
}

#[cfg(test)]
mod size_experiment {
    //! Measures the cost of `Error`'s two `String`-carrying fields
    //! (`Option { name, detail }`) for the D21 "box the rare payload"
    //! question (`planning/00-decisions.md` D21, D20's "measure it, land it
    //! separately" instruction).
    //!
    //! **Result: attempted, not landed, own item.** `Error::Option { name,
    //! detail }` is a public struct-variant matched by name at 128 call
    //! sites across 45 files (`grep -rn 'Error::Option'`), most in crates
    //! this agent does not own (h264, hevc, aac, sched, cli, muxers —
    //! everything with a `set_option`). Boxing it to shrink the enum is a
    //! real, mechanical, workspace-wide edit, not a local one, and
    //! `AGENT-CONSTRAINTS.md`'s scope rule is explicit: a change outside an
    //! agent's owned crates is reported, not worked around, even when D20
    //! licenses it in principle. So this only measures the shape's cost in
    //! isolation, on the actual `Error` type (safe: `size_of` reads no other
    //! crate's code), and reports the number for whoever does have the
    //! cross-crate mandate.
    use super::Error;

    // The hypothetical: `Option(Box<{ name: String, detail: String }>)` --
    // a `Box` is one pointer, 8 bytes, so the enum's size would then be set
    // by its next-largest variant, `LimitExceeded` at 32 (plus
    // discriminant/padding). A local stand-in enum (not `Error` itself, for
    // exactly the reason in the module doc above) with the same variant
    // shapes, `Option` boxed, confirms the arithmetic rather than asserting
    // it from memory. Variants are otherwise unused by design -- this type
    // exists only for `size_of`.
    #[allow(dead_code, reason = "exists only for size_of:: -- see the module doc")]
    enum ErrorShapeWithBoxedOption {
        InvalidData(&'static str),
        UnexpectedEof,
        Eof,
        NeedMoreInput,
        OutputPending,
        Unsupported(&'static str),
        LimitExceeded {
            limit: &'static str,
            requested: u64,
            cap: u64,
        },
        Option(Box<(String, String)>),
        Io(std::io::Error),
        NotSeekable,
        Cancelled,
    }

    #[test]
    fn current_size_is_dominated_by_the_two_string_variant() {
        // `Option { name: String, detail: String }` is 2 * 24 = 48 bytes of
        // payload before the discriminant; every other variant is at most
        // `LimitExceeded`'s 16 (&'static str) + 8 + 8 = 32 bytes. A closed
        // enum's size is its largest variant plus a discriminant (rounded to
        // alignment), so `Option` alone sets `size_of::<Error>()` -- and
        // therefore `size_of::<Result<T, Error>>()` for any `T` no larger
        // than it, on the success path of every fallible call in the
        // workspace, whether or not that call ever errors.
        let actual = size_of::<Error>();
        assert_eq!(
            actual, 48,
            "Error's size moved (was 48, dominated by the two-String Option \
             variant) -- if this shrank, the boxing question below may \
             already be moot; if it grew, re-check which variant is now \
             largest before re-deriving the 48 above"
        );

        let hypothetical = size_of::<ErrorShapeWithBoxedOption>();
        assert!(
            hypothetical < actual,
            "boxing Option should shrink the enum below its current {actual}; \
             got {hypothetical}"
        );
        assert_eq!(
            hypothetical, 40,
            "expected LimitExceeded (32 bytes) plus discriminant/padding to \
             become the new largest variant at 40; got {hypothetical} -- \
             re-check which variant is now largest"
        );
        // ~17% smaller (48 -> 40). Real, but this is the *shape's* cost in
        // isolation -- it says nothing about whether shrinking `Result<T,
        // Error>` by 16 bytes end to end is inside the noise floor for any
        // real workload, which is why this stays a measured shape fact and
        // not a landed change: that would need the same interleaved-A/B,
        // ffmpeg-relative protocol as everything else in
        // `planning/PERF-PROGRAMME.md` SS2, run on a binary that actually
        // has the boxed type wired through every call site.
    }
}
