//! Motion video filters: `mestimate`, `minterpolate`, `framerate`,
//! `deshake`.
//!
//! # What is implemented, and what is not
//!
//! Two of the row's four filters:
//!
//! - [`framerate`] — a real, working frame-rate converter that blends
//!   between the two bracketing input frames rather than merely duplicating
//!   (see `vaco-filter-video-format::fps` for the duplicate-only sibling).
//!   **Not** the reference's own algorithm: the reference does a
//!   block-level motion-compensated blend by default; this ships a plain
//!   per-pixel linear cross-fade with a whole-frame scene-cut gate (via
//!   [`vaco_filter_vdsp::normalised_sad`]) that falls back to nearest-frame
//!   selection across a cut. That is a real, named divergence, not a
//!   partial implementation of the reference's own algorithm — see that
//!   module's doc for the exact formula and its residual.
//! - [`deshake`] — single-pass, causal, translation-only stabilisation: a
//!   sparse grid of block motion searches
//!   ([`vaco_filter_vdsp::motion::search_block`]) feeds a per-frame
//!   translation estimate, an exponential moving average tracks the
//!   "intentional" path, and the frame is warped
//!   ([`vaco_filter_vdsp::affine`]) back toward it. The reference's own
//!   `deshake` (and the separate `vidstabdetect`/`vidstabtransform` pair)
//!   do two-pass feature tracking with a full affine (rotation + zoom)
//!   correction and a non-causal (lookahead) smoothing window; this is
//!   real and reduces jitter, but is a strictly simpler model, named as
//!   such rather than presented as equivalent.
//!
//! **Not attempted**, both for the same reason: `mestimate` reports a
//! dense per-macroblock motion vector field as the reference's own
//! diagnostic overlay/side-data format, and `minterpolate` needs that same
//! field plus an occlusion-aware bidirectional interpolator (multiple
//! named modes: `dup`, `blend`, `mci`) — both are substantially larger than
//! `framerate`'s single-hypothesis blend and were not reached in this
//! pass's time budget. `deshake`/`framerate` were prioritised because they
//! are independently useful and already had a real dependency
//! (`vaco-filter-vdsp::motion`) built for exactly this purpose.

#![forbid(unsafe_code)]

mod common;
pub mod deshake;
pub mod framerate;
pub mod registry;
pub mod stabdetect;
pub mod stabtransform;

pub use registry::MotionRegistry;
