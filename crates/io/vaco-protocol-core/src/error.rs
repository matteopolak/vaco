//! The protocol layer's errors.
//!
//! Separate from [`vaco_core::Error`] because the interesting cases here —
//! "that scheme is not on the whitelist", "nesting went too deep" — carry the
//! offending name, and the core taxonomy is a closed enum this crate cannot
//! extend. Conversion into it is lossy by design: a caller that wants the
//! detail matches on [`ProtocolError`], and a caller that just wants to fail
//! gets a `?`.

/// Result alias for the protocol layer.
pub type Result<T, E = ProtocolError> = std::result::Result<T, E>;

/// What can go wrong dispatching or opening a URL.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProtocolError {
    /// No protocol is registered under this scheme.
    Unknown { scheme: String },

    /// The scheme is registered, but this open is not permitted.
    ///
    /// **This is the security boundary.** It is a distinct variant, not an
    /// `Unsupported`, so that a caller can tell "we cannot do that" from "we
    /// refuse to do that" — and so that a log line about a denied open reads as
    /// what it is.
    Denied { scheme: String, reason: DenyReason },

    /// The protocol does not implement this operation.
    Unsupported {
        scheme: &'static str,
        operation: &'static str,
    },

    /// The URL is registered and permitted but malformed for this protocol.
    Malformed {
        scheme: &'static str,
        detail: &'static str,
    },

    /// The transport failed.
    Io(vaco_core::Error),
}

/// Why an open was refused. Mirrors the four rules in the whitelist gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// W1: the scheme is on the blacklist, which always wins.
    Blacklisted,
    /// W2/W3: a whitelist is in force and the scheme is on neither it nor the
    /// parent protocol's default grants.
    NotWhitelisted,
    /// W4: nesting exceeded the recursion limit.
    TooDeep,
    /// The path escapes the root the caller confined this open to (rule U2).
    OutsideRoot,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Blacklisted => "blacklisted",
            Self::NotWhitelisted => "not on the protocol whitelist",
            Self::TooDeep => "nested protocol recursion limit reached",
            Self::OutsideRoot => "path escapes the permitted root",
        })
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { scheme } => write!(f, "unknown protocol `{scheme}`"),
            Self::Denied { scheme, reason } => {
                write!(f, "protocol `{scheme}` refused: {reason}")
            }
            Self::Unsupported { scheme, operation } => {
                write!(f, "protocol `{scheme}` does not support {operation}")
            }
            Self::Malformed { scheme, detail } => {
                write!(f, "malformed `{scheme}` url: {detail}")
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<vaco_core::Error> for ProtocolError {
    fn from(e: vaco_core::Error) -> Self {
        Self::Io(e)
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(vaco_core::Error::from(e))
    }
}

impl From<ProtocolError> for vaco_core::Error {
    fn from(e: ProtocolError) -> Self {
        match e {
            ProtocolError::Io(inner) => inner,
            ProtocolError::Unknown { .. } => Self::Unsupported("unknown protocol"),
            ProtocolError::Denied { .. } => Self::Unsupported("protocol refused by the whitelist"),
            ProtocolError::Unsupported { .. } => Self::Unsupported("protocol operation"),
            ProtocolError::Malformed { .. } => Self::InvalidData("malformed url"),
        }
    }
}
