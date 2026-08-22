//! The immutable policy half of the crate.

use std::time::Instant;

/// The caps a component instance must respect, as an immutable policy value.
///
/// `Limits` holds *no* counters. Everything mutable — cumulative allocation,
/// fuel — lives in [`Budget`](crate::Budget), which is created from a `Limits`
/// and is the thing a parser threads through its call graph. Splitting them this
/// way makes `Limits` `Send + Sync + Clone` and cheap to share across a whole
/// pipeline while keeping every counter single-owner, so consumption order is
/// deterministic and a fuzz finding replays exactly.
///
/// Construct with [`Limits::permissive`] or [`Limits::strict`] and adjust with
/// the `with_*` methods; the struct is `#[non_exhaustive]` so adding a knob is
/// not a breaking change and no caller can accidentally leave one at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Cumulative live bytes charged to one component instance.
    pub max_alloc_total: u64,
    /// Largest single allocation.
    pub max_alloc_single: u64,
    /// Per-axis video dimension.
    pub max_dimension: u32,
    /// Bytes in one decoded frame, across all planes.
    pub max_frame_bytes: u64,
    /// Audio channel count.
    pub max_channels: u16,
    /// Audio sample rate, Hz.
    pub max_sample_rate: u32,
    /// Streams in one container.
    pub max_streams: u32,
    /// Side-data entries on one packet or frame.
    pub max_side_data: u32,
    /// Bytes read while probing for a format.
    pub max_probe_bytes: u64,
    /// Bytes of metadata retained from one container.
    pub max_metadata_bytes: u64,
    /// Units available to input-derived loops. See [`Budget::consume_fuel`].
    ///
    /// [`Budget::consume_fuel`]: crate::Budget::consume_fuel
    pub fuel: u64,
    /// Wall-clock cutoff, checked at packet and frame boundaries.
    ///
    /// Deliberately last-resort: an `Instant` comparison is not reproducible, so
    /// it can never be the mechanism a regression test depends on.
    pub deadline: Option<Instant>,
}

const MIB: u64 = 1 << 20;

impl Limits {
    /// Generous caps sized for real-world media. The CLI default.
    ///
    /// The numbers come from the largest plausible legitimate input, not from
    /// what an attacker might send: 8K RGBA at 16 bits is ~530 MB, so a 1 GiB
    /// total is roomy without being unbounded.
    #[must_use]
    pub const fn permissive() -> Self {
        Self {
            max_alloc_total: 1024 * MIB,
            max_alloc_single: 512 * MIB,
            max_dimension: 65_536,
            max_frame_bytes: 512 * MIB,
            max_channels: 128,
            max_sample_rate: 2_822_400,
            max_streams: 4096,
            max_side_data: 256,
            max_probe_bytes: 32 * MIB,
            max_metadata_bytes: 16 * MIB,
            fuel: 1 << 32,
            deadline: None,
        }
    }

    /// Conservative caps for untrusted input, fuzzing and library embedders.
    ///
    /// 64 MiB total. A library dropped into someone else's process should be
    /// conservative unless it is told otherwise, so this is the default an
    /// embedder gets and `permissive` is the deliberate opt-out.
    #[must_use]
    pub const fn strict() -> Self {
        Self {
            max_alloc_total: 64 * MIB,
            max_alloc_single: 16 * MIB,
            max_dimension: 8192,
            max_frame_bytes: 16 * MIB,
            max_channels: 64,
            max_sample_rate: 384_000,
            max_streams: 256,
            max_side_data: 64,
            max_probe_bytes: MIB,
            max_metadata_bytes: MIB,
            fuel: 1 << 26,
            deadline: None,
        }
    }

    /// The tightest useful configuration: for `limit_*` fuzz targets, which
    /// assert that every component fails cleanly rather than panicking when the
    /// budget is absurd.
    #[must_use]
    pub const fn tiny() -> Self {
        Self {
            max_alloc_total: 1 << 16,
            max_alloc_single: 1 << 14,
            max_dimension: 256,
            max_frame_bytes: 1 << 16,
            max_channels: 8,
            max_sample_rate: 48_000,
            max_streams: 8,
            max_side_data: 4,
            max_probe_bytes: 4096,
            max_metadata_bytes: 4096,
            fuel: 1 << 16,
            deadline: None,
        }
    }

    /// Override the cumulative allocation cap.
    #[must_use]
    pub const fn with_alloc_total(mut self, bytes: u64) -> Self {
        self.max_alloc_total = bytes;
        self
    }

    /// Override the single-allocation cap.
    #[must_use]
    pub const fn with_alloc_single(mut self, bytes: u64) -> Self {
        self.max_alloc_single = bytes;
        self
    }

    /// Override the fuel allowance.
    #[must_use]
    pub const fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// Set a wall-clock deadline.
    #[must_use]
    pub const fn with_deadline(mut self, at: Instant) -> Self {
        self.deadline = Some(at);
        self
    }
}

impl Default for Limits {
    /// [`Limits::strict`] — the conservative choice, because a `Default` is what
    /// gets used when nobody thought about it.
    fn default() -> Self {
        Self::strict()
    }
}
