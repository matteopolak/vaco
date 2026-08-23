//! Video composite filters: `overlay` and `rotate`, and the alpha-blend
//! primitives they share — plan 16 §4.2's compositing row, GitHub issue
//! #465 (FT-4.1c).
//!
//! # Why these two are one crate
//!
//! Neither filter is a byte mover like `vaco-filter-video-geometry`'s six:
//! both interpret sample *values*, not just move bytes. `overlay` blends two
//! independent timelines with `vaco-filter-framesync` and an alpha formula
//! measured against ffmpeg 8.1 (see [`blend`]); `rotate` resamples with
//! interpolation and fills the corners the source never reaches with a solid
//! colour built the same way `vaco-filter-video-geometry::fill` builds
//! `pad`'s border (independently reimplemented here — see [`fill`] — because
//! that helper is crate-private to its own crate).
//!
//! # Shape
//!
//! | Module | Contents |
//! |---|---|
//! | [`blend`] | the "over" alpha formula, measured for straight and premultiplied alpha, plus the byte-level plane walk both filters' pixel math goes through |
//! | [`format_opt`] | `overlay`'s `format=`/`alpha=` enums and their measured `PixFmt` mapping |
//! | [`fill`] | a solid-colour frame via `vaco-scale`, for `rotate`'s corners |
//! | [`overlay`] | the filter: `vaco-filter-framesync`-driven, per-frame `x`/`y` expressions |
//! | [`rotate`] | the filter: angle expression, `rotw`/`roth`, bilinear or nearest sampling |
//! | [`registry`] | [`registry::CompositeRegistry`], the `FilterRegistry` impl |
//!
//! # What is measured versus what is a documented gap
//!
//! `docs/filter/vaco-filter-video-composite.md` carries the full edge-case
//! table. The headline findings, each established against the shipped
//! `ffmpeg 8.1` binary (D17) rather than recalled or read from its source
//! (D7):
//!
//! * `overlay`'s `x`/`y` are evaluated **per frame** by default (`eval=frame`,
//!   the reference's own default) — not once at `configure`, unlike every
//!   filter in `vaco-filter-video-geometry`.
//! * `overlay`'s `alpha=` option (`auto`/`unknown`/`straight`/`premultiplied`)
//!   only changes the output when the *background* itself carries alpha;
//!   `auto` and `unknown` share option value `0` and both mean "straight".
//! * `rotate`'s default `ow`/`oh` are the **literal strings `"iw"`/`"ih"`**
//!   — same size as the input, clipped — not a bounding-box fit. The
//!   bounding box is available only via the `rotw(a)`/`roth(a)` expression
//!   functions, which this crate implements as `vaco-expr` externs.
//!
//! # Depth
//!
//! [`blend::composite`] and [`rotate`]'s resampler both work in 8-bit
//! samples read through `vaco_frame`'s byte-oriented `Plane` API. The
//! `format=` values that name a 10-bit target (`yuv420p10`, `yuv422p10`,
//! `yuv444p10`) are parsed and mapped to their measured `PixFmt`, matching
//! the reference's own option surface, but are rejected with
//! [`vaco_core::Error::Unsupported`] at `configure` rather than composited
//! with wrong byte-pair math — an honest gap, not a guess.
//!
//! # wasm (D18)
//!
//! Nothing here reads a clock: `t` comes from a frame's own timestamp via
//! `vaco_core::Timestamp::to_seconds`, never `Instant::now()`. `cargo build
//! --target wasm32-unknown-unknown` passes unchanged.
#![forbid(unsafe_code)]

pub mod blend;
pub mod fill;
pub mod format_opt;
mod geom;
pub mod overlay;
pub mod registry;
pub mod rotate;

#[cfg(test)]
mod tests_invariants;

pub use registry::CompositeRegistry;
