//! T1/T2 temporal and interleave filters (FT-4.12b, GitHub #475).
//!
//! # Membership, checked against the reference rather than assumed
//!
//! `planning/16-filters.md` SS4.3's `vaco-filter-temporal` row lists `fps,
//! framestep, tpad, tmix, tblend, tmedian, tlut2, tmidequalizer, decimate,
//! mpdecimate, deflicker, lagfun, freezedetect, freezeframes, dejudder,
//! fsync, random`. Every one of those seventeen names was checked against
//! `ffmpeg -hide_banner -filters` and `ffmpeg -h filter=<name>` (ffmpeg 8.1,
//! 2026-08-23) and every one exists with exactly that name and arity — the
//! row is exact, nothing to add or drop. `fps` is **not** registered by this
//! crate: it already exists as `vaco_filter_video_format::fps`, registering
//! it again here would collide in `cargo xtask dup-check`, and the owning
//! crate is out of this brief's scope to edit. The other sixteen are here.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `fn create`, aggregated by [`registry::TemporalRegistry`] —
//! the same shape `vaco-filter-denoise` and `vaco-filter-audio-eq` use.
//! [`video`] is the shared plane-decode/encode helper most of the pixel-math
//! filters are built on (mirrors `vaco-filter-denoise::video`, duplicated
//! rather than shared because neither crate depends on the other and no
//! lower crate offers it yet — see this crate's docs for the note left for
//! whoever eventually hoists it).
//!
//! # `scene_sad`
//!
//! The row's extra-deps column calls for `vdsp (scene_sad)`. No such
//! implementation existed anywhere under `crates/filter/` when this crate was
//! written (checked with `grep -rln scene_sad`), and `vaco-filter-vdsp` —
//! the crate plan SS4.1 places it in — did not exist either, so it is created
//! here, minimally (just `scene_sad` and its block/normalised variants), per
//! this brief's explicit instruction to write a missing shared kernel where
//! the plan says it lives rather than inlining a second copy. `decimate`,
//! `mpdecimate` and `freezedetect` all build on it.
//!
//! # Multi-input filters use `vaco-filter-framesync`
//!
//! `freezeframes` (`VV->V`) is the one filter in this row with two video
//! inputs; it goes through `vaco_filter_framesync::{FrameSyncFilter,
//! Synced}` rather than a hand-rolled two-pad `Filter` impl, per this
//! brief's reuse instruction. Every other filter in the row is a single
//! video pad in, one out — `tlut2` included: despite the name's kinship with
//! the two-*stream* `lut2` (`vaco-filter-lut`'s cousin, `srcx`/`srcy` pads),
//! `tlut2` is documented and measured (`ffmpeg -h filter=tlut2`, two-frame
//! raw-video probe) as a *temporal* filter — one input pad, comparing the
//! current frame against its own immediately preceding frame — so it needs
//! no framesync at all, just one held frame of state.
//!
//! # What is verified versus structural
//!
//! See `docs/filter/vaco-filter-temporal.md` for the full per-filter
//! accounting: which ones are checked byte-for-byte against
//! `ffmpeg -f framecrc`, which are held to an independent property (identity
//! at a trivial parameter, a hand-computable closed form, a predictable drop
//! count), and which are documented structural simplifications (`random`'s
//! shuffle order, `dejudder`'s re-timing, `tmidequalizer`'s windowing) with
//! the gap named rather than silently approximated.
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
