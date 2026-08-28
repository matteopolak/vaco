//! T3 artistic/stylisation video filters — `planning/16-filters.md` §4.2's
//! `vaco-filter-artistic` row. Six implemented: `amplify`, `delogo`, `epx`,
//! `noise`, `removelogo`, `vignette`.
//!
//! # This crate did not exist; a dispatch brief named a crate the plan does not have
//!
//! The dispatch brief that led to this crate asked for a `vaco-filter-effect`
//! crate covering roughly two dozen "stylisation" filters (`sobel`,
//! `prewitt`, `roberts`, `kirsch`, `edgedetect`, `morpho`, `erosion`,
//! `dilation`, `deflate`, `inflate`, `shuffleframes`, `shufflepixels`,
//! `shuffleplanes`, `swaprect`, `swapuv`, `tmix`, `lagfun`, `random`,
//! `photosensitivity`, `noise`, `vignette`, `pixelize`, among others). Per
//! `planning/AGENT-CONSTRAINTS.md`'s "the plan already partitions the
//! filters; do not invent a crate", the plan's own §4.2 table was checked
//! before writing anything, and almost the entire named list already has a
//! home and, for most of it, is already **built and committed**:
//!
//! * `sobel`, `prewitt`, `roberts`, `kirsch`, `scharr`, `edgedetect`,
//!   `morpho`, `erosion`, `dilation`, `deflate`, `inflate`, `median`,
//!   `convolution` — `vaco-filter-convolve` (issue #468, `agent:blur2`,
//!   `ASSIGNMENTS.md` status `done`).
//! * `swaprect`, `swapuv`, `shuffleframes`, `shuffleplanes`, `pixelize` —
//!   `vaco-filter-geometry` (issue #470, `agent:geom2`, status `done`).
//!   `shufflepixels` is in that crate's row but was deliberately **not**
//!   implemented there: its `seed` option (range to `UINT32_MAX`) reads as a
//!   generator seed, that crate never identified the generator, and it
//!   declined to ship a shuffle that would compile and pass an
//!   identity-permutation test while diverging from the reference at every
//!   other seed. See that crate's `lib.rs` doc for the measurement. The same
//!   reasoning applies here and this crate does not attempt it either.
//! * `tmix`, `lagfun`, `random` — `vaco-filter-temporal` (issue #475,
//!   `agent:temporal`, status `done`). `random`'s doc records the same
//!   "reservoir shuffle is right, the reference's specific PRNG is not
//!   reproduced" divergence this crate's own `noise` lands on independently
//!   below.
//! * `photosensitivity` — belongs to the `vaco-filter-analysis` row (issue
//!   #477), which `ASSIGNMENTS.md` lists as `assigned` to `agent:analysis2`
//!   and under active, same-day commits throughout this crate's whole time
//!   in the tree. Not this crate's to touch under the single-writer rule.
//!
//! What was left, unclaimed, and actually in this crate's real row:
//! `noise`, `vignette`, `amplify`, `delogo`, `removelogo`, `epx`, `xbr`,
//! `hqx`, `super2xsai`, `cover_rect`, `find_rect`. This crate implements six
//! of those eleven; the rest are listed under "Left for a follow-up" below,
//! each with the specific reason it stopped rather than a blanket "out of
//! time". This is a genuine, reported change of scope from the dispatch
//! brief, not a silent substitution.
//!
//! # What is verified versus structural
//!
//! | Filter | Confidence |
//! |---|---|
//! | [`amplify`] | Framecrc-level: the temporal window, the tolerance/threshold gate and the low/high delta clamp are all measured exactly against the reference (`ffmpeg 8.1`, `-bitexact`), including the one-frame-more-than-expected readiness delay. See that module's doc for the full derivation. |
//! | [`epx`] | Framecrc-level at both `n=2` (Scale2x) and `n=3` (Scale3x), from the public `scale2x.it` specification, not the reference's source — both scale factors re-probed against a corner pixel exercising every conditional branch. |
//! | [`vignette`] | Framecrc-level for the `cos^4` forward/backward formula at `dither=0` (8-bit luma/gray). **`dither=1` is the filter's own default** and is not reproduced — a bounded sweep (a 32x32 uniform frame's dither pattern showed no short-period tiling, ruling out a simple ordered-dither matrix within the time available) confirms it is a genuine per-pixel pseudo-random jitter, not merely unattempted. Chroma-plane handling and `aspect != 1`'s extreme corners are two smaller, separately documented gaps. |
//! | [`noise`] | Framecrc-level only at `strength=0` (the default), which is a genuine, verified no-op. Any `strength > 0` uses this crate's own PRNG and is not framecrc-verified, matching `vaco-filter-temporal::random`'s precedent — reproducing the reference's generator needs its source (D7). |
//! | [`delogo`] | Structural, **not** framecrc-verified. The measured weighted-bilinear border-interpolation formula matches three of a test box's four columns exactly but diverges on the fourth at every row tried; several alternate conventions were tried and none fixed the fourth column without breaking the other three, so this is reported as an unresolved discrepancy rather than a border effect quietly worked around. |
//! | [`removelogo`] | Structural, **not** framecrc-verified — it reuses `delogo`'s interpolation core (over the mask's bounding box) and therefore inherits that core's discrepancy. What *is* measured: the mask is a plain PGM (P5), and mask intensity thresholds rather than blends (bisected the on/off boundary to somewhere in `10..32`, not narrowed further). |
//!
//! See `docs/filter/vaco-filter-artistic.md` for the full framecrc
//! comparison table and every measurement's exact command line.
//!
//! # Left for a follow-up, each for a distinct reason
//!
//! * **`hqx`** — a **D7** call, not a time one. hq2x/hq3x/hq4x classify
//!   each pixel's neighbourhood into one of 256 patterns and look up a
//!   hand-tuned interpolation rule per pattern. That table is *authorial* —
//!   Stepin designed it by visual experimentation, not derived from a
//!   formula or dictated by a format's own constraints (contrast `epx`'s
//!   four-comparison rule, which fully determines the output with no table
//!   at all) — and the only source this pass found for the exact table was
//!   the reference's own implementation.
//! * **`xbr`** — independently published (Hyllian's own description), so D7
//!   is not the blocker, but genuinely **larger than `epx`, not "a fraction
//!   of the cost"** as first assumed: a shader-level reference
//!   implementation samples a 5x5 neighbourhood (`epx`'s is 3x3) and
//!   applies roughly 8-12 distinct weighted-distance comparisons *per
//!   corner* via coefficient matrices, not `epx`'s single boolean check.
//!   Deferred for time, with that correction on record rather than a
//!   restated assumption.
//! * **`super2xsai`** — also independently published (predates and is
//!   separate from `FFmpeg`), so again not a D7 case. Three independent
//!   attempts to source its complete pixel-selection logic (which diagonal
//!   pattern selects which of the `INTERPOLATE`/`Q_INTERPOLATE`-computed
//!   values for each of the four output sub-pixels) found the two
//!   arithmetic helper functions but not a precise statement of the
//!   selection logic itself. Guessing at that structure risks exactly the
//!   "confidently wrong" failure mode this project's own conventions warn
//!   against, so it is left unattempted rather than reconstructed from
//!   partial information.
//! * **`cover_rect`**, **`find_rect`** — template matching against a second
//!   bitmap input, at multiple mipmap scales. `find_rect` reports a
//!   *position*, not a transformed frame (`ffmpeg -h filter=find_rect` has
//!   no pixel-modifying options at all), so the right verification shape is
//!   a `showinfo`-style metadata assertion, not a framecrc pixel diff —
//!   decided, and recorded here, before any implementation was attempted.
//!   `cover_rect` itself exposes no coordinate options at all (only a
//!   replacement bitmap and a cover/blur mode), which reads as depending on
//!   `find_rect`'s own detection — most plausibly via frame-metadata the two
//!   filters exchange when chained, the same mechanism `vaco-filter-mm`'s
//!   `showinfo` precedent already established for surfacing detector output
//!   as metadata rather than pixels. Neither filter's underlying
//!   multi-scale correlation search was implemented in the time available.
//!
//! # A shared PRNG, now needed a fourth time
//!
//! [`noise`] needed the same small dependency-free `SplitMix64` generator
//! `vaco-filter-temporal::random` and `vaco-filter-source`'s generators
//! already carry their own copies of, for the same reason (a `seed` option
//! this crate cannot reproduce the reference's actual generator for, per
//! D7). See `planning/TECH-DEBT.md` for the consolidation recommendation
//! this pass made rather than acted on unilaterally — moving it crosses
//! into `vaco-filter-vdsp`, a crate this pass does not own.

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
pub mod removelogo;
mod rng;
pub mod registry;
pub mod vignette;

pub use registry::ArtisticRegistry;
