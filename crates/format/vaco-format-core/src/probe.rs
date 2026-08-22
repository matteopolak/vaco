//! Format detection.

/// A prefix of the input, plus whatever the caller knows about its origin.
#[derive(Debug, Clone, Copy)]
pub struct ProbeData<'a> {
    pub buf: &'a [u8],
    pub filename: Option<&'a str>,
    pub mime_type: Option<&'a str>,
}

/// How confident a demuxer is that the input is its format.
///
/// `ffprobe` reports this value as `probe_score`, so it is part of the
/// byte-identical output contract (D5) and its exact scale matters, not just its
/// ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ProbeScore(pub u8);

impl ProbeScore {
    pub const NONE: Self = Self(0);
    /// Filename extension matched, content did not confirm.
    pub const EXTENSION: Self = Self(50);
    /// Content matched with some ambiguity remaining.
    pub const CONTENT: Self = Self(75);
    /// An unambiguous signature was found.
    pub const MAX: Self = Self(100);
}
