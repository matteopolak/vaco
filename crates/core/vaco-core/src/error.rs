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
