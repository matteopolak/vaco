//! Capability flags.

bitflags::bitflags! {
    /// What an implementation can do. Consulted before it is instantiated.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Caps: u32 {
        /// Buffers internally; must be drained with a `None` send at EOF.
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
    }
}
