//! T3 artistic/stylisation video filters. Implemented: `amplify`, `delogo`,
//! `epx`, `noise`, `removelogo`, and `vignette`.
//!
//! Each filter's registration stays in this crate; sibling crates own their
//! own filter families. Unimplemented filters remain explicit below so a
//! missing capability is not mistaken for a near-match.
//!
//! # What is verified versus structural
//!
//! | Filter | Confidence |
//! |---|---|
//! | [`amplify`] | Framecrc-level: the temporal window, the tolerance/threshold gate and the low/high delta clamp are all measured exactly against the reference (`ffmpeg 8.1`, `-bitexact`), including the one-frame-more-than-expected readiness delay. See that module's doc for the full derivation. |
//! | [`epx`] | Framecrc-level at both `n=2` (Scale2x) and `n=3` (Scale3x), from the public `scale2x.it` specification, not the reference's source — both scale factors re-probed against a corner pixel exercising every conditional branch. |
//! | [`vignette`] | Framecrc-level for the `cos^4` forward/backward formula at `dither=0` (8-bit luma/gray). **`dither=1` is the filter's own default** and is not reproduced — a bounded sweep (a 32x32 uniform frame's dither pattern showed no short-period tiling, ruling out a simple ordered-dither matrix within the time available) confirms it is a genuine per-pixel pseudo-random jitter, not merely unattempted. Chroma-plane handling and `aspect != 1`'s extreme corners are two smaller, separately documented gaps. |
//! | [`noise`] | Framecrc-level only at `strength=0` (the default), which is a genuine, verified no-op. Any `strength > 0` uses this crate's own PRNG and is not framecrc-verified because the reference generator is not independently specified. |
//! | [`delogo`] | Structural, **not** framecrc-verified. The measured weighted-bilinear border-interpolation formula matches three of a test box's four columns exactly but diverges on the fourth at every row tried; several alternate conventions were tried and none fixed the fourth column without breaking the other three, so this is reported as an unresolved discrepancy rather than a border effect quietly worked around. |
//! | [`removelogo`] | Structural, **not** framecrc-verified — it reuses `delogo`'s interpolation core (over the mask's bounding box) and therefore inherits that core's discrepancy. What *is* measured: the mask is a plain PGM (P5), and mask intensity thresholds rather than blends (bisected the on/off boundary to somewhere in `10..32`, not narrowed further). |
//!
//! See `docs/filter/vaco-filter-artistic.md` for the full framecrc
//! comparison table and every measurement's exact command line.
//!
//! # Left for a follow-up, each for a distinct reason
//!
//! * **`hqx`** — its 256-entry hand-tuned interpolation table is authorial,
//!   not derived from a formula; the only exact table found was in the
//!   reference implementation.
//! * **`xbr`** — an independent description exists, but it samples a 5x5
//!   neighbourhood and applies roughly 8–12 weighted-distance comparisons
//!   per corner, substantially larger than `epx`'s 3x3 single-check rule.
//! * **`super2xsai`** — published helper arithmetic was available, but not
//!   the complete diagonal pattern-selection logic for its four output
//!   sub-pixels. Guessing that structure would risk a confident near-match.
//! * **`cover_rect`**, **`find_rect`** — these perform multi-scale template
//!   matching against a second bitmap. `find_rect` reports position metadata,
//!   not pixels, so it needs a `showinfo`-style assertion; neither search was
//!   implemented without a verified reference for that behavior.
//!

#![forbid(unsafe_code)]
#![allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h are the natural names for pixel coordinates and \
              dimensions throughout this crate's image-processing math, \
              exactly as vaco-filter-convolve allows the same lint for \
              the same reason"
)]

pub mod amplify;
mod common;
pub mod delogo;
pub mod epx;
pub mod noise;
pub mod registry;
pub mod removelogo;
mod rng;
pub mod vignette;

pub use registry::ArtisticRegistry;
