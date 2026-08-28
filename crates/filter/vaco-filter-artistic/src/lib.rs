//! T3 artistic/stylisation video filters — `planning/16-filters.md` §4.2's
//! `vaco-filter-artistic` row.
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
//!   and under active, same-day commits at the time this crate was written.
//!   Not this crate's to touch under the single-writer rule.
//!
//! What is left, unclaimed, and actually in this crate's row: `noise`,
//! `vignette`, plus `epx`/`xbr`/`hqx`/`super2xsai`/`amplify`/`delogo`/
//! `removelogo`/`cover_rect`/`find_rect`, none of which had an owner in
//! `planning/ASSIGNMENTS.md` when this crate was written. This pass
//! implements the two reachable in the time available — `noise` and
//! `vignette` — and leaves the rest for a follow-up (see "Left for a
//! follow-up" below). This is a genuine change of scope from the dispatch
//! brief, reported rather than silently substituted.
//!
//! # What is verified versus structural
//!
//! * **`vignette`** — the darkening formula (`cos^4` optical vignetting law,
//!   `mode=forward` and `mode=backward`, default `angle`/`x0`/`y0`/`aspect`)
//!   is measured framecrc-exact against the reference for 8-bit luma/gray
//!   planes with `dither=0`. Three documented gaps, all measured rather than
//!   guessed: `dither=1` (**the default**) adds a per-pixel jitter that is
//!   deterministic across runs despite no `seed` option, and this crate did
//!   not identify its generator in the time available — same shape as
//!   `noise`/`random`, see [`vignette`]'s own doc; chroma-plane handling
//!   (scaled around the neutral `128` point) is structurally reasoned, not
//!   pixel-measured, after a first probe left a 1-count residual this pass
//!   did not chase down; and `aspect != 1` reproduces the interior exactly
//!   but not the extra hard clipping the reference applies near the frame's
//!   extreme corners. See `docs/filter/vaco-filter-artistic.md` for the
//!   measurements.
//! * **`noise`** — the option surface (`all_seed`/`all_strength`/`all_flags`
//!   and the four `c0`..`c3` component variants, `a`/`p`/`t`/`u` flag
//!   letters) is real (`ffmpeg -h filter=noise`, 2026-08-28) and the shape
//!   (independent per-component additive noise, strength 0-100, temporal
//!   flag persists the noise buffer across frames instead of redrawing it)
//!   is implemented, but the actual pseudo-random sequence is **not** the
//!   reference's: reproducing it would mean reading the reference's source
//!   (D7), and, like `vaco-filter-temporal::random`, this crate has no
//!   `rand`-family dependency to add for one filter. Not framecrc-verified;
//!   see [`noise`]'s doc.
//!
//! # Left for a follow-up (out of this pass's time budget)
//!
//! `epx`/`xbr`/`hqx`/`super2xsai` (pixel-art upscaling — well-published
//! algorithms, but each is its own multi-rule engine and none were reached);
//! `amplify` (temporal min/max-window comparison); `delogo`/`removelogo`
//! (region interpolation, and `removelogo` additionally reads a bitmap side
//! file); `cover_rect`/`find_rect` (template matching against a reference
//! bitmap). None block `noise`/`vignette`.

#![forbid(unsafe_code)]

mod common;
mod rng;
pub mod noise;
pub mod registry;
pub mod vignette;

pub use registry::ArtisticRegistry;
