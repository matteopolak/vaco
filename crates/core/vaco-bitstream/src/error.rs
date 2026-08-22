//! Bitstream errors.
//!
//! Deliberately few: the reader's model is a sticky flag checked once per syntax
//! structure, so most reads have no error to return at all.

/// What went wrong while reading a bitstream.
///
/// `Copy` and allocation-free — an error path in a parser must never itself
/// allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BitstreamError {
    /// A read went past the logical end of the buffer. The values it returned
    /// were zeros.
    #[error("read past the end of the bitstream")]
    Overrun,

    /// A variable-length code was structurally impossible — an Exp-Golomb prefix
    /// longer than the coding can express, most often.
    ///
    /// Distinct from [`BitstreamError::Overrun`] because it means "these bytes
    /// are not this format" rather than "there were not enough bytes".
    #[error("malformed variable-length code")]
    Malformed,

    /// A syntax element decoded to a value the caller declared out of range.
    /// Produced by `ue_max` and friends.
    #[error("value {value} exceeds the permitted maximum {max}")]
    ValueTooLarge {
        /// The decoded value.
        value: u64,
        /// The caller's inclusive ceiling.
        max: u64,
    },
}

impl From<BitstreamError> for vaco_core::Error {
    fn from(e: BitstreamError) -> Self {
        match e {
            BitstreamError::Overrun => Self::UnexpectedEof,
            BitstreamError::Malformed => Self::InvalidData("malformed variable-length code"),
            BitstreamError::ValueTooLarge { .. } => {
                Self::InvalidData("syntax element out of range")
            }
        }
    }
}

/// Shorthand for fallible bitstream operations.
pub type Result<T, E = BitstreamError> = std::result::Result<T, E>;
