//! Field-order, telecine, and deinterlace filters.
//!
//! This crate registers twenty reference filters:
//! `yadif`, `bwdif`, `w3fdif`, `estdif`, `separatefields`, `weave`,
//! `doubleweave`, `fieldorder`, `fieldmatch`, `fieldhint`, `detelecine`,
//! `telecine`, `idet`, `vfrdet`, `interlace`, `tinterlace`, `kerndeint`,
//! `pullup`, `repeatfields`, and `phase`. Their names and
//! arities were checked with ffmpeg 8.1. All are one-input/one-output except
//! `fieldmatch`, whose `ppsrc` option changes the input count from one to two.
//!
//! Frame-count changes remain inside a single pad through buffering:
//! [`vaco_filter_core::adapt::Simple`] can return [`FrameOut::Many`] or
//! [`FrameOut::None`]. Only `fieldmatch`'s two-input mode needs
//! [`vaco_filter_core::adapt::Paired`].
//!
//! One module per filter, `pub const DESC: FilterDesc` and a crate-private
//! constructor, is aggregated by [`registry::DeinterlaceRegistry`]. [`video`]
//! works on row bytes because these filters mostly select and rearrange rows,
//! avoiding a numeric round trip and remaining exact across sample depths.
//!
//! `idet` and `fieldmatch` need single-frame combing metrics rather than the
//! whole-plane comparison performed by SAD helpers.
//! [`vaco_filter_vdsp::comb_score`] and [`vaco_filter_vdsp::field_sad`] are
//! shared instead of duplicated; see `docs/filter/vaco-filter-vdsp.md`.
//!
//! See `docs/filter/vaco-filter-deinterlace.md` for the full per-filter
//! accounting of byte-exact paths, structural implementations, and detectors.
//! `idet` writes `lavfi.idet.*` metadata with a narrower vocabulary than the
//! reference. `vfrdet` exposes statistics internally because the reference
//! publishes only a final log line, not per-frame metadata.
#![forbid(unsafe_code)]

pub mod bwdif;
pub mod detelecine;
pub mod estdif;
pub mod fieldhint;
pub mod fieldmatch;
pub mod fieldorder;
pub mod idet;
pub mod interlace;
pub mod kerndeint;
mod mad;
mod opt_consts;
pub mod phase;
pub mod pullup;
pub mod registry;
pub mod repeatfields;
pub mod separatefields;
pub mod telecine;
pub mod tinterlace;
pub mod vfrdet;
mod video;
pub mod w3fdif;
pub mod weave;
pub mod yadif;

/// Benchmark-only entry points for measuring the shared motion-adaptive
/// kernel without making its filter-adapter state part of the supported API.
#[doc(hidden)]
pub mod bench_support {
    use vaco_core::Result;
    use vaco_frame::{Frame, FramePool};

    /// Run one frame through the production `yadif`/`bwdif`/`w3fdif`/`estdif`
    /// kernel with explicit temporal neighbours.
    pub fn deinterlace_frame(
        pool: &FramePool,
        prev: Option<&Frame>,
        cur: &Frame,
        next: Option<&Frame>,
        parity_tff: bool,
    ) -> Result<Frame> {
        crate::mad::deinterlace_frame(pool, prev, cur, next, parity_tff)
    }
}

pub use registry::DeinterlaceRegistry;
