//! T2/T3 video analysis and detection filters (FT-4.12d, GitHub #477).
//!
//! # Membership, checked against the reference rather than assumed
//!
//! `planning/16-filters.md` SS4.3's `vaco-filter-analysis` row lists `psnr,
//! ssim, ssim360, xpsnr, vif, vmafmotion, msad, identity, blackdetect,
//! blockdetect, bitplanenoise, entropy, siti, signalstats, readeia608,
//! readvitc, showinfo, photosensitivity, scdet, bbox, codecview,
//! blackframe, cropdetect, signature`. Every one of those twenty-three names
//! exists in `ffmpeg -hide_banner -filters` and `ffmpeg -h filter=<name>`
//! (ffmpeg 8.1, 2026-08-23) with that exact spelling and arity — the row
//! matches the reference in both directions, nothing to add or drop.
//!
//! **Eight landed in this crate**: `psnr`, `ssim`, `identity`, `msad`,
//! `signalstats`, `blackdetect`, `blackframe`, `bbox` — the six the brief
//! calls out to land first, plus `msad` (a near-free extension of the
//! `identity`/`psnr` machinery and of `vaco-filter-vdsp`) and `blackframe`
//! (a near-free extension of `blackdetect`'s pixel-threshold logic).
//!
//! **Fifteen did not land**, for three different reasons:
//!
//! * `vmafmotion`, `ssim360`, `vif`, `signature` — explicitly named in the
//!   brief as likely-to-leave. `vif` needs a wavelet-domain natural-scene
//!   statistics model from a separate published paper (Sheikh & Bovik 2006)
//!   that was not implemented in the time available; `vmafmotion` and
//!   `ssim360` build on `vif`/`ssim` machinery this crate did not extend to
//!   them; `signature` (MPEG-7 video signature) is a substantial standalone
//!   algorithm (frame partitioning into a fixed set of regions, per-region
//!   feature vectors, a whole matching/lookup layer) out of proportion to
//!   this wave's time budget.
//! * `xpsnr` — a judgement call to leave. It is not just weighted `psnr`:
//!   the reference's XPSNR is documented to apply per-block perceptual
//!   weighting derived from local activity, and getting that weighting
//!   right needs its own measurement pass this crate did not have time for;
//!   shipping an under-measured `xpsnr` in the crate other filters are
//!   verified against was judged worse than leaving it out and saying so.
//! * `blockdetect`, `bitplanenoise`, `entropy`, `siti`, `readeia608`,
//!   `readvitc`, `showinfo`, `photosensitivity`, `scdet`, `codecview`,
//!   `cropdetect` — not explicitly named by the brief, left for pace
//!   reasons and one real interface gap:
//!   - `showinfo` and `codecview` do not fit this crate's whole model.
//!     `showinfo` is measured (`ffprobe -show_frames` through it) to write
//!     **no** frame metadata at all — its output is a console log line, a
//!     channel this workspace's filter framework does not have — so
//!     interface gap 11 does not help it; it needs a different gap.
//!     `codecview` visualises motion vectors, which are not a
//!     `vaco_frame::FrameSideData` variant this workspace has yet — a
//!     decoder-side gap, not a measurement-formula gap.
//!   - `readeia608`/`readvitc` need bit-accurate waveform decoding
//!     (EIA-608 line-21 encoding, SMPTE VITC bi-phase marks) that is
//!     substantial to get right and, without a real captured line to decode
//!     against, hard to verify independently of the implementation itself —
//!     exactly the "oracle that shares your misreading" trap
//!     `planning/AGENT-CONSTRAINTS.md` warns about.
//!   - `blockdetect`, `bitplanenoise`, `entropy`, `siti`, `photosensitivity`,
//!     `scdet`, `cropdetect` are all individually tractable (each was
//!     partially measured while scoping this crate — see this crate's
//!     report) but did not fit this wave's time budget once the eight
//!     landed filters were verified to the standard this crate's brief
//!     demands. Left for a follow-up rather than shipped under-measured.
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
//! # `psnr`/`ssim`/`identity`/`msad` use `vaco-filter-core::Paired`, not `vaco-filter-framesync`
//!
//! The brief's own text suggested `vaco-filter-framesync` for these —
//! measured and found not to match: `ffmpeg -h filter=psnr` (and `ssim`,
//! `identity`, `msad`) carries **no** `eof_action`/`shortest`/`repeatlast`/
//! `ts_sync_mode` section at all, unlike `ffmpeg -h filter=alphamerge`,
//! which has the full framesync surface verbatim. These four filters
//! measure identically to `framepack` — [`vaco_filter_core::adapt::Paired`]'s
//! own worked example of "strict lockstep, no independent per-input
//! timeline" — not to `overlay`/`alphamerge`'s per-input-timeline shape. See
//! [`psnr`]'s module doc for the measurement in full.
//!
//! # What was added to `vaco-filter-vdsp`
//!
//! Two kernels, per this crate's explicit invitation to extend rather than
//! duplicate: `plane_sse` (`psnr`'s MSE numerator) and `identical_count`
//! (`identity`'s bit-exact-fraction numerator/denominator). `msad` needed
//! no addition — it is `normalised_sad`, already there for
//! `freezedetect`, used directly.
//!
//! # Two measured surprises, recorded because they contradict a reasonable guess
//!
//! * `psnr`'s `mse_avg`/`ssim`'s `All` average their per-component values
//!   **weighted by sample count** (so a subsampled chroma plane counts for
//!   less), but `identity`/`msad`'s `_avg` fields average **unweighted** —
//!   the two families do not share one averaging rule. See
//!   [`fmt::weighted_average`] and [`fmt::simple_average`]'s docs for the
//!   measurement that tells them apart.
//! * `ssim` is not byte-exact against the reference on *any* input,
//!   including the degenerate zero-variance case where the published
//!   formula has a windowing-independent closed form — the reference's own
//!   number for that case does not match the closed form either, which
//!   means its internal implementation is not the unmodified textbook
//!   floating-point Gaussian window. See [`ssim`]'s module doc for the
//!   exact numbers and the arithmetic that catches this.
#![forbid(unsafe_code)]

pub mod bbox;
pub mod blackdetect;
pub mod blackframe;
mod fmt;
pub mod identity;
pub mod msad;
pub mod psnr;
pub mod registry;
pub mod signalstats;
pub mod ssim;
mod video;
