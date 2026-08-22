//! What a container *can* do, as declared by its descriptor.
//!
//! These are the facts the generic machinery in this crate needs before it has
//! opened anything: whether the core may build an index, whether timestamps may
//! legitimately jump, whether a byte seek is meaningful. Every one of them
//! changes the behaviour of a generic path — [`crate::seek`] consults five of
//! them and [`crate::interleave`] consults three — which is the test for
//! whether a flag belongs here at all.
//!
//! The names are interface facts (D9): several are user-visible through
//! `vaco -formats`. The *values* are ours, because nothing outside this
//! workspace observes them.

bitflags::bitflags! {
    /// Container-level capability declarations.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
    pub struct FormatFlags: u32 {
        /// Needs no byte stream at all: a capture or playback device.
        const NOFILE          = 1 << 0;
        /// The filename must contain a `%d`-style number (image sequences).
        const NEEDNUMBER      = 1 << 1;
        /// Incomplete; requires `-strict experimental`.
        const EXPERIMENTAL    = 1 << 2;
        /// Print numeric stream ids: the container's own identifiers are
        /// meaningful to the user (MPEG-TS PIDs, MP4 track ids).
        const SHOW_IDS        = 1 << 3;
        /// Wants codec extradata out of band, in the header.
        const GLOBALHEADER    = 1 << 4;
        /// Carries no timestamps at all. Raw elementary streams.
        const NOTIMESTAMPS    = 1 << 5;
        /// The core may build an index from the packets it reads, and seek with
        /// it. Formats that index themselves do not need this.
        const GENERIC_INDEX   = 1 << 6;
        /// Timestamps may jump legitimately. Suppresses the monotonic-DTS
        /// repair in [`crate::time`] — discontinuity *policy* is the CLI's.
        const TS_DISCONT      = 1 << 7;
        /// Frame durations vary within a stream.
        const VARIABLE_FPS    = 1 << 8;
        /// Stores no picture dimensions.
        const NODIMENSIONS    = 1 << 9;
        /// A valid file may contain no streams.
        const NOSTREAMS       = 1 << 10;
        /// Binary search over byte positions will not work here.
        const NOBINSEARCH     = 1 << 11;
        /// The generic index seek will not work here.
        const NOGENSEARCH     = 1 << 12;
        /// A byte position is not a meaningful seek target.
        const NO_BYTE_SEEK    = 1 << 13;
        /// Non-decreasing DTS is acceptable; without it, strictly increasing is
        /// required on the mux side.
        const TS_NONSTRICT    = 1 << 14;
        /// Negative timestamps are representable, so `avoid_negative_ts auto`
        /// resolves to `disabled`.
        const TS_NEGATIVE     = 1 << 15;
        /// Every audio frame holds the same sample count.
        const FIXED_FRAMESIZE = 1 << 16;
        /// Seek targets are PTS, not DTS.
        const SEEK_TO_PTS     = 1 << 17;
    }
}

impl FormatFlags {
    /// Whether the generic index seek path ([`crate::seek`] S4) is permitted.
    #[must_use]
    pub const fn allows_index_seek(self) -> bool {
        !self.contains(Self::NOGENSEARCH)
    }

    /// Whether the binary-search seek path (S5) is permitted.
    ///
    /// A format whose timestamps jump cannot be bisected: the search invariant
    /// assumes timestamps increase with byte position, and `TS_DISCONT` is the
    /// declaration that they do not.
    #[must_use]
    pub const fn allows_binary_search(self) -> bool {
        !self.contains(Self::NOBINSEARCH) && !self.contains(Self::TS_DISCONT)
    }

    /// Whether a byte seek (S6) is meaningful.
    #[must_use]
    pub const fn allows_byte_seek(self) -> bool {
        !self.contains(Self::NO_BYTE_SEEK)
    }

    /// Whether the core may add index entries as packets go past.
    #[must_use]
    pub const fn builds_generic_index(self) -> bool {
        self.contains(Self::GENERIC_INDEX)
    }

    /// Whether per-stream DTS must be strictly increasing on the mux side.
    #[must_use]
    pub const fn requires_strict_dts(self) -> bool {
        !self.contains(Self::TS_NONSTRICT)
    }
}

/// The name each flag prints under, in bit order.
pub const FORMAT_FLAG_NAMES: &[(FormatFlags, &str)] = &[
    (FormatFlags::NOFILE, "nofile"),
    (FormatFlags::NEEDNUMBER, "neednumber"),
    (FormatFlags::EXPERIMENTAL, "experimental"),
    (FormatFlags::SHOW_IDS, "show_ids"),
    (FormatFlags::GLOBALHEADER, "globalheader"),
    (FormatFlags::NOTIMESTAMPS, "notimestamps"),
    (FormatFlags::GENERIC_INDEX, "generic_index"),
    (FormatFlags::TS_DISCONT, "ts_discont"),
    (FormatFlags::VARIABLE_FPS, "variable_fps"),
    (FormatFlags::NODIMENSIONS, "nodimensions"),
    (FormatFlags::NOSTREAMS, "nostreams"),
    (FormatFlags::NOBINSEARCH, "nobinsearch"),
    (FormatFlags::NOGENSEARCH, "nogensearch"),
    (FormatFlags::NO_BYTE_SEEK, "no_byte_seek"),
    (FormatFlags::TS_NONSTRICT, "ts_nonstrict"),
    (FormatFlags::TS_NEGATIVE, "ts_negative"),
    (FormatFlags::FIXED_FRAMESIZE, "fixed_framesize"),
    (FormatFlags::SEEK_TO_PTS, "seek_to_pts"),
];

impl core::fmt::Display for FormatFlags {
    /// `generic_index+ts_discont`, or `none`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut any = false;
        for &(flag, name) in FORMAT_FLAG_NAMES {
            if self.contains(flag) {
                if any {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                any = true;
            }
        }
        if any { Ok(()) } else { f.write_str("none") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discont_disables_binary_search() {
        assert!(FormatFlags::empty().allows_binary_search());
        assert!(!FormatFlags::TS_DISCONT.allows_binary_search());
        assert!(!FormatFlags::NOBINSEARCH.allows_binary_search());
    }

    #[test]
    fn display_is_stable() {
        assert_eq!(FormatFlags::empty().to_string(), "none");
        assert_eq!(
            FormatFlags::GENERIC_INDEX
                .union(FormatFlags::TS_DISCONT)
                .to_string(),
            "generic_index+ts_discont"
        );
    }

    #[test]
    fn every_flag_has_a_name() {
        let named = FORMAT_FLAG_NAMES
            .iter()
            .fold(FormatFlags::empty(), |a, &(f, _)| a.union(f));
        assert_eq!(named, FormatFlags::all());
    }
}
