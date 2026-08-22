//! Capability flags, and what the framework does with them.

use std::fmt;

bitflags::bitflags! {
    /// What an implementation can do. Consulted before it is instantiated.
    ///
    /// Deliberately smaller than the reference tool's set: flags that describe
    /// another project's internal plumbing are not modelled, and one flag that
    /// project does not have — [`Caps::PATENT_ENCUMBERED`] — is.
    ///
    /// Three of these are *checked*, not merely advertised. [`Caps::DELAY`] and
    /// [`Caps::SUBFRAMES`] bound what the send/receive machine will tolerate
    /// (see [`crate::Machine`] and [`crate::Validated`]), so declaring them
    /// wrongly fails a test rather than producing mysterious behaviour
    /// downstream; [`Caps::PATENT_ENCUMBERED`] is what CI asserts on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
    pub struct Caps: u32 {
        /// Buffers internally; must be drained with a `None` send at EOF.
        ///
        /// Enforced: a component without this flag that produces output while
        /// draining is a protocol violation.
        const DELAY              = 1 << 0;
        /// Can decode several frames concurrently (plan 15 §1.8.1).
        const FRAME_THREADS      = 1 << 1;
        /// Can process independent slices of one frame concurrently.
        const SLICE_THREADS      = 1 << 2;
        /// Tolerates mid-stream parameter changes.
        const PARAM_CHANGE       = 1 << 3;
        /// Audio encoder accepting a varying sample count per call.
        const VARIABLE_FRAME_SIZE = 1 << 4;
        /// Expensive to instantiate; a poor choice for format probing.
        const AVOID_PROBING      = 1 << 5;
        /// Backed by fixed-function hardware.
        const HARDWARE           = 1 << 6;
        /// Incomplete; requires the user to opt in explicitly.
        const EXPERIMENTAL       = 1 << 7;
        /// Covered by patents that D4 keeps out of the distributed build.
        ///
        /// CI asserts no component carrying this flag is reachable from a
        /// default-feature build.
        const PATENT_ENCUMBERED  = 1 << 8;
        /// One input may yield more than one output.
        ///
        /// Enforced: a component without this flag that produces a second
        /// output for one input is a protocol violation. Plan 15 §1.3 lists it;
        /// it was missing from the frozen set and the state machine cannot
        /// police N:M without it.
        const SUBFRAMES          = 1 << 9;
    }
}

/// Every flag paired with the name the CLI prints for it.
///
/// Order is bit order, which is the order `-h decoder=…` lists them in.
pub const CAP_NAMES: &[(Caps, &str)] = &[
    (Caps::DELAY, "delay"),
    (Caps::FRAME_THREADS, "frame_threads"),
    (Caps::SLICE_THREADS, "slice_threads"),
    (Caps::PARAM_CHANGE, "param_change"),
    (Caps::VARIABLE_FRAME_SIZE, "variable_frame_size"),
    (Caps::AVOID_PROBING, "avoid_probing"),
    (Caps::HARDWARE, "hardware"),
    (Caps::EXPERIMENTAL, "experimental"),
    (Caps::PATENT_ENCUMBERED, "patent_encumbered"),
    (Caps::SUBFRAMES, "subframes"),
];

impl Caps {
    /// Whether D4 keeps this component out of the distributed build.
    #[must_use]
    pub const fn is_patent_encumbered(self) -> bool {
        self.contains(Self::PATENT_ENCUMBERED)
    }

    /// Whether the caller must send `None` at end of stream to get every
    /// output. Callers that skip the drain on a `DELAY` component silently
    /// truncate the stream.
    #[must_use]
    pub const fn needs_drain(self) -> bool {
        self.contains(Self::DELAY)
    }

    /// Whether one input may produce more than one output.
    #[must_use]
    pub const fn may_expand(self) -> bool {
        self.contains(Self::SUBFRAMES)
    }

    /// Whether any form of intra-component threading is available.
    #[must_use]
    pub const fn is_threaded(self) -> bool {
        self.intersects(Self::FRAME_THREADS.union(Self::SLICE_THREADS))
    }

    /// The flags the user must explicitly opt into before this component may be
    /// selected: experimental status and patent encumbrance.
    #[must_use]
    pub const fn opt_in_required(self) -> Self {
        self.intersection(Self::EXPERIMENTAL.union(Self::PATENT_ENCUMBERED))
    }

    /// Resolve one flag by its CLI name.
    ///
    /// Named `from_cli_name` rather than `from_name` because `bitflags`
    /// generates a `from_name` of its own that matches the *constant* name
    /// (`"DELAY"`); this one matches what the CLI prints (`"delay"`).
    #[must_use]
    pub fn from_cli_name(name: &str) -> Option<Self> {
        CAP_NAMES
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|&(c, _)| c)
    }

    /// The names of the flags that are set, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        CAP_NAMES
            .iter()
            .filter(move |&&(c, _)| self.contains(c))
            .map(|&(_, n)| n)
    }
}

impl fmt::Display for Caps {
    /// `delay+subframes`, or `none`. This is what `-h decoder=…` prints, so it
    /// is stable output rather than a debug convenience.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for name in self.names() {
            if any {
                f.write_str("+")?;
            }
            f.write_str(name)?;
            any = true;
        }
        if any { Ok(()) } else { f.write_str("none") }
    }
}

bitflags::bitflags! {
    /// What a codec *format* implies, before any implementation is chosen.
    ///
    /// [`Caps`] describes an implementation; this describes the format itself,
    /// which is why it hangs off [`CodecId`](crate::CodecId) rather than off a
    /// descriptor. A container uses it to decide whether timestamps can be
    /// reordered before it has opened anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
    pub struct CodecProperties: u32 {
        /// Every frame decodes independently: no inter prediction, so any frame
        /// is a seek point.
        const INTRA_ONLY = 1 << 0;
        /// Discards information.
        const LOSSY      = 1 << 1;
        /// Can reconstruct its input exactly, at least in some mode.
        const LOSSLESS   = 1 << 2;
        /// Output order may differ from decode order, so `dts` and `pts`
        /// diverge and a reorder buffer is required.
        const REORDER    = 1 << 3;
        /// Can code interlaced fields.
        const FIELDS     = 1 << 4;
    }
}

impl CodecProperties {
    /// Whether decode order can differ from presentation order.
    #[must_use]
    pub const fn reorders(self) -> bool {
        self.contains(Self::REORDER)
    }

    /// Whether every frame is a seek point.
    #[must_use]
    pub const fn is_intra_only(self) -> bool {
        self.contains(Self::INTRA_ONLY)
    }
}
