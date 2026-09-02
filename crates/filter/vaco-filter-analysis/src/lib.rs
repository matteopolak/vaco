//! T2/T3 video analysis and detection filters: per-frame metrics (`psnr`,
//! `ssim`, `identity`, `msad`, `signalstats`), scene/content detectors
//! (`blackdetect`, `blackframe`, `bbox`, `cropdetect`, `scdet`), and
//! diagnostics (`entropy`, `showinfo`).
//!
//! `bitplanenoise`, `siti`, `vif`, `vmafmotion`, `ssim360`, `signature`,
//! `xpsnr`, `blockdetect`, `readeia608`, `readvitc`, `photosensitivity` and
//! `codecview` are not implemented — see `docs/filter/vaco-filter-analysis.md`
//! for why, including two filters (`bitplanenoise`, `siti`) that looked like
//! clean closed forms and measured out not to be.
//!
//! # Shape
//!
//! One module per filter, each exposing `pub const DESC: FilterDesc` and a
//! crate-private `fn create`, aggregated by [`registry::AnalysisRegistry`] —
//! the same shape `vaco-filter-temporal` and `vaco-filter-denoise` use.
//! [`video`] holds shared pads, plane access and option parsing; [`fmt`]
//! holds the three distinct `lavfi.<filter>.<key>` value-formatting rules
//! measured across this crate's filters (see its own doc for why there are
//! three, not one).
//!
//! # Design notes worth keeping in mind
//!
//! * `psnr`/`ssim`/`identity`/`msad` pair their two inputs with
//!   [`vaco_filter_core::adapt::Paired`], not `vaco-filter-framesync`:
//!   `ffmpeg -h filter=psnr` (and `ssim`, `identity`, `msad`) carries no
//!   `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` section, unlike a
//!   filter that actually uses framesync (e.g. `alphamerge`). See
//!   [`psnr`]'s module doc for the measurement.
//! * `psnr`'s `mse_avg`/`ssim`'s `All` average their per-component values
//!   **weighted by sample count** (so a subsampled chroma plane counts for
//!   less), but `identity`/`msad`'s `_avg` fields average **unweighted** —
//!   the two families do not share one averaging rule. See
//!   [`fmt::weighted_average`] and [`fmt::simple_average`].
//! * `ssim` is not byte-exact against the reference on *any* input,
//!   including the degenerate zero-variance case where the published
//!   formula has a windowing-independent closed form — the reference's own
//!   number for that case does not match the closed form either, so its
//!   internal implementation is not the unmodified textbook floating-point
//!   Gaussian window. See [`ssim`]'s module doc for the exact numbers.
#![forbid(unsafe_code)]

pub mod bbox;
pub mod blackdetect;
pub mod blackframe;
pub mod cropdetect;
pub mod entropy;
mod fmt;
pub mod identity;
pub mod msad;
pub mod psnr;
pub mod registry;
pub mod scdet;
pub mod showinfo;
pub mod signalstats;
pub mod ssim;
mod video;
