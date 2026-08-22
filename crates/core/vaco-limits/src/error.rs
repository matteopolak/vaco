//! The limit error taxonomy.
//!
//! Every variant is *correct behaviour* under hostile input: reaching a limit is
//! the system working, not failing. Fuzz targets treat these as success (plan 13
//! §2.2.4) and only a panic, hang or abort counts as a finding.

/// A budget, fuel or deadline limit was reached.
///
/// `Copy` and allocation-free, so returning one can never itself allocate — which
/// matters when the reason for the error is that allocation is already refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LimitError {
    /// A named cap was exceeded. `limit` is the field name in [`Limits`], so a
    /// user-facing message can point at the knob to raise.
    ///
    /// [`Limits`]: crate::Limits
    #[error("{limit} limit exceeded: requested {requested}, cap {cap}")]
    Exceeded {
        /// The [`Limits`](crate::Limits) field that was hit.
        limit: &'static str,
        /// What the caller asked for, in the unit of that field.
        requested: u64,
        /// The configured cap.
        cap: u64,
    },

    /// A size computation overflowed before any cap could be applied.
    ///
    /// `count * size_of::<T>()` on an attacker-supplied `count` is the classic
    /// case; it is an error rather than a wrap because a wrapped size would then
    /// pass the cap check.
    #[error("size computation overflowed")]
    Overflow,

    /// The budget allowed it but the allocator refused.
    #[error("allocation of {bytes} bytes failed")]
    AllocFailed {
        /// The size that could not be satisfied.
        bytes: u64,
    },

    /// An input-derived loop ran out of fuel.
    ///
    /// Deterministic: the same input always exhausts at the same point, which is
    /// what makes a fuzz finding replay and minimise cleanly.
    #[error("fuel exhausted after {spent} units")]
    FuelExhausted {
        /// Units consumed when the counter ran out.
        spent: u64,
    },

    /// The wall-clock deadline passed. Not reproducible — the fallback mechanism,
    /// never the primary one.
    #[error("deadline exceeded")]
    DeadlineExceeded,

    /// A stepping API reported no progress too many times in a row.
    #[error("no progress after {ticks} consecutive steps")]
    NoProgress {
        /// Consecutive no-progress steps observed.
        ticks: u32,
    },
}

impl From<LimitError> for vaco_core::Error {
    fn from(e: LimitError) -> Self {
        match e {
            LimitError::Exceeded {
                limit,
                requested,
                cap,
            } => Self::LimitExceeded {
                limit,
                requested,
                cap,
            },
            LimitError::Overflow => Self::LimitExceeded {
                limit: "size_computation",
                requested: u64::MAX,
                cap: u64::MAX,
            },
            LimitError::AllocFailed { bytes } => Self::LimitExceeded {
                limit: "allocator",
                requested: bytes,
                cap: bytes,
            },
            LimitError::FuelExhausted { spent } => Self::LimitExceeded {
                limit: "fuel",
                requested: spent,
                cap: spent,
            },
            LimitError::DeadlineExceeded => Self::LimitExceeded {
                limit: "deadline",
                requested: 0,
                cap: 0,
            },
            LimitError::NoProgress { ticks } => Self::LimitExceeded {
                limit: "progress",
                requested: u64::from(ticks),
                cap: u64::from(ticks),
            },
        }
    }
}

/// Shorthand for fallible limit operations.
pub type Result<T, E = LimitError> = std::result::Result<T, E>;
