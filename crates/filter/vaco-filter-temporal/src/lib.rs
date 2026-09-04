//! Temporal and interleave video filters.
//!
//! The reference exposes `fps`,
//! framestep, tpad, tmix, tblend, tmedian, tlut2, tmidequalizer, decimate,
//! mpdecimate, deflicker, lagfun, freezedetect, freezeframes, dejudder,
//! fsync, and random`; the names and arities were checked with ffmpeg 8.1.
//! `fps` is owned by `vaco_filter_video_format`, so this crate registers the
//! other sixteen without duplicating that name.
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private constructor, is aggregated by [`registry::TemporalRegistry`].
//! [`video`] is the shared plane-decode/encode helper most of the pixel-math
//! filters use. It remains local because neither this crate nor
//! `vaco-filter-denoise` can depend on the other and no lower crate owns it.
//! `decimate`, `mpdecimate`, and `freezedetect` share the `scene_sad`, block,
//! and normalized kernels from `vaco-filter-vdsp`.
//!
//! `freezeframes` is the only filter here with two video
//! inputs; it goes through `vaco_filter_framesync::{FrameSyncFilter,
//! Synced}`. `tlut2` is a single-input temporal filter: a two-frame raw-video
//! probe confirmed that it compares each frame with its immediate predecessor,
//! so it holds one frame rather than using framesync.
//!
//! See `docs/filter/vaco-filter-temporal.md` for the full per-filter
//! accounting: reference comparisons, independent properties, and explicit
//! structural simplifications for `random`, `dejudder`, and `tmidequalizer`.
#![forbid(unsafe_code)]

pub mod decimate;
pub mod deflicker;
pub mod dejudder;
pub mod framestep;
pub mod freezedetect;
pub mod freezeframes;
pub mod fsync;
pub mod lagfun;
pub mod mpdecimate;
pub mod random;
pub mod registry;
pub mod rng;
pub mod tblend;
pub mod tlut2;
pub mod tmedian;
pub mod tmidequalizer;
pub mod tmix;
pub mod tpad;
mod video;

pub use registry::TemporalRegistry;
