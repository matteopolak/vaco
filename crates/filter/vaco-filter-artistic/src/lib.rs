//! T3 artistic/stylisation video filters — `planning/16-filters.md` §4.2's
//! `vaco-filter-artistic` row. Five implemented: `amplify`, `delogo`,
//! `epx`, `noise`, `vignette`.
//!
//! # This crate did not exist; a dispatch brief named a crate the plan does not have
//!
//! The dispatch brief that led to this crate asked
//! for a `vaco-filter-effect` crate covering roughly two dozen "stylisation"
//! filters (`sobel`, `prewitt`, `roberts`, `kirsch`, `edgedetect`, `morpho`,
//! `erosion`, `dilation`, `deflate`, `inflate`, `shuffleframes`,
//! `shufflepixels`, `shuffleplanes`, `swaprect`, `swapuv`, `tmix`, `lagfun`,
//! `random`, `photosensitivity`, `noise`, `vignette`, `pixelize`, among
//! others). Per `planning/AGENT-CONSTRAINTS.md`'s "the plan already
//! partitions the filters; do not invent a crate", the plan's own §4.2 table
//! was checked before writing anything, and almost the entire named list
//! already has a home and, for most of it, is already **built and
//! committed**:
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
//! `hqx`, `super2xsai`, `cover_rect`, `find_rect`. This crate implements
//! five of those eleven; the rest are listed under "Left for a follow-up"
//! below, each with the specific reason it stopped rather than a blanket
//! "out of time". This is a genuine, reported change of scope from the
//! dispatch brief, not a silent substitution.
//!
//! # What is verified versus structural
//!
//! | Filter | Confidence |
//! |---|---|
//! | [`amplify`] | Framecrc-level: the temporal window, the tolerance/threshold gate and the low/high delta clamp are all measured exactly against the reference (`ffmpeg 8.1`, `-bitexact`), including the one-frame-more-than-expected readiness delay. See that module's doc for the full derivation. |
//! | [`epx`] | Framecrc-level at `n=2` (Scale2x), from the public `scale2x.it` specification, not the reference's source. `n=3` (Scale3x) is implemented from the same specification but not independently re-probed. |
//! | [`vignette`] | Framecrc-level for the `cos^4` forward/backward formula at `dither=0` (8-bit luma/gray). **`dither=1` is the filter's own default** and is not reproduced — a bounded sweep (a 32x32 uniform frame's dither pattern showed no short-period tiling, ruling out a simple ordered-dither matrix within the time available) confirms it is a genuine per-pixel pseudo-random jitter, not merely unattempted. Chroma-plane handling and `aspect != 1`'s extreme corners are two smaller, separately documented gaps. |
//! | [`noise`] | Framecrc-level only at `strength=0` (the default), which is a genuine, verified no-op. Any `strength > 0` uses this crate's own PRNG and is not framecrc-verified, matching `vaco-filter-temporal::random`'s precedent — reproducing the reference's generator needs its source (D7). |
//! | [`delogo`] | Structural, **not** framecrc-verified. The measured weighted-bilinear border-interpolation formula matches three of a test box's four columns exactly but diverges on the fourth at every row tried; several alternate conventions were tried and none fixed the fourth column without breaking the other three, so this is reported as an unresolved discrepancy rather than a border effect quietly worked around. |
//!
//! See `docs/filter/vaco-filter-artistic.md` for the full framecrc
//! comparison table and every measurement's exact command line.
//!
//! # Left for a follow-up, each for a distinct reason
//!
//! * **`hqx`** — not attempted, and not purely for lack of time: hq2x/hq3x/
//!   hq4x classify each pixel's neighbourhood into one of 256 patterns and
//!   look up a hand-tuned interpolation rule per pattern. That table is
//!   *authorial* — Stepin designed it by visual experimentation, not derived
//!   from a formula or dictated by a format's own constraints (contrast
//!   `epx`'s four-comparison rule, which fully determines the output with no
//!   table at all) — so reproducing it faithfully would mean transcribing
//!   the specific 256-case mapping from somewhere, and the only source this
//!   pass found for the exact table was the reference's own implementation.
//!   D7 stops there; this is a "cannot be done cleanly from public
//!   documentation" case, not a "did not get to it" one.
//! * **`xbr`**, **`super2xsai`** — unlike `hqx`, these *are* independently
//!   published by their authors (Hyllian's own xBR description; the
//!   original 2xSaI algorithm predates and is independent of `FFmpeg`), so D7
//!   is not the blocker — this pass's remaining time was, after `epx`
//!   established the pixel-art-scaler pattern was worth pursuing at all.
//!   Genuinely deferred for time, not declined on principle.
//! * **`removelogo`** — reads a second input, a bitmap mask file in a format
//!   specific to this filter, which is both a new file format to specify
//!   (beyond this crate's scope so far) and a case of parsing a
//!   user-supplied file the fuzzing requirement in this project's own rules
//!   would then apply to — a larger unit of work than `delogo`, which needs
//!   no side file.
//! * **`cover_rect`**, **`find_rect`** — template matching against a second
//!   bitmap input, at multiple mipmap scales. `find_rect` additionally
//!   *reports a position* rather than transforming pixels (`ffmpeg -h
//!   filter=find_rect` shows no video-modifying options at all beyond the
//!   template/threshold/search-window ones), so verifying it would need a
//!   different assertion shape than framecrc entirely — a `showinfo`-style
//!   metadata check, not a pixel diff. Not attempted; flagged rather than
//!   forced into the wrong verification shape.
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
mod rng;
pub mod registry;
pub mod vignette;

pub use registry::ArtisticRegistry;
