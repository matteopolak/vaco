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
//! **Twelve landed in this crate**: `psnr`, `ssim`, `identity`, `msad`,
//! `signalstats`, `blackdetect`, `blackframe`, `bbox` — the six the brief
//! calls out to land first, plus `msad` (a near-free extension of the
//! `identity`/`psnr` machinery and of `vaco-filter-vdsp`) and `blackframe`
//! (a near-free extension of `blackdetect`'s pixel-threshold logic) — plus,
//! in a 2026-08-23 continuation pass, [`entropy`] and [`cropdetect`], the two
//! this wave's own brief called the best remaining value. See those two
//! modules' own docs for their measurements, and this crate's docs file for
//! two real walls found while investigating `bitplanenoise` and `siti`: both
//! looked like clean closed forms and measured out to *not* be — a Sobel-SI
//! formula that matches at one amplitude and not another, and a per-pixel
//! bit-noise metric whose numerator stayed constant while its denominator
//! tracked frame width in a way no simple per-pixel formula reproduced in
//! the time available. Neither was shipped as a guess. [`showinfo`] landed
//! once interface gap 13 closed — see below. [`scdet`] landed in the #113
//! pass: the earlier note below called it an unmeasured "scene-cut
//! heuristic combining a mean-absolute-frame-difference with its own
//! frame-to-frame delta" — two independent constructed probes (one on a
//! literal step function, one on `testsrc`'s continuous content) pinned
//! both `mafd`'s exact `/256` scale factor and `score`'s actual rule
//! (suppressed only on an *exact* repeat of the previous `mafd`, not on any
//! decrease), so see [`scdet`]'s own doc rather than this paragraph's
//! now-superseded description of it as unmeasured.
//!
//! **Eleven did not land**, for four different reasons:
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
//!   `readvitc`, `photosensitivity`, `scdet`, `codecview`,
//!   `cropdetect` — not explicitly named by the brief, left for pace
//!   reasons and one real interface gap:
//!   - `codecview` still does not fit this crate's whole model: it
//!     visualises motion vectors, and while `vaco_frame::FrameSideData`
//!     grew a `MotionVectors` variant closing interface gap 14's *shape*,
//!     no decoder in this workspace populates one (D5) — a decoder-side
//!     gap, not a measurement-formula gap, and not reachable from a filter
//!     crate regardless of the side-data variant existing. `showinfo`
//!     *did* land — see the gap 13 section below — once its own channel
//!     existed.
//!
//! ## Interface gap 13 closed: `showinfo`
//!
//! [`vaco_frame::FrameSideData::Log`] is the console-log-only channel
//! `showinfo` needed: measured (`ffprobe -show_frames` through it, `ffmpeg
//! 8.1`) to write no `AVFrame::metadata` at all, so interface gap 11's
//! dictionary genuinely could not have carried its output, whatever key it
//! was stuffed under. [`showinfo`] reproduces the reference's own two-line
//! format byte for byte against a real synthetic frame — see that module's
//! doc for every field's measurement, including the Adler-32 checksum seed
//! (`(a=0, b=0)`, the same one `planning/AGENT-CONSTRAINTS.md` already
//! recorded for `framecrc`/`framehash`, confirmed independently here) and
//! the population-vs-sample standard deviation distinction.
//!
//! Not reproduced: the two `config in`/`config out` lines the reference
//! prints once per graph link rather than per frame (out of scope for a
//! per-`Frame` side-data channel), and anything for a pixel format deeper
//! than 8 bits per component (not measured — every fixture available was
//! `yuv420p`).
//!   - `readeia608`/`readvitc` need bit-accurate waveform decoding
//!     (EIA-608 line-21 encoding, SMPTE VITC bi-phase marks) that is
//!     substantial to get right and, without a real captured line to decode
//!     against, hard to verify independently of the implementation itself —
//!     exactly the "oracle that shares your misreading" trap
//!     `planning/AGENT-CONSTRAINTS.md` warns about.
//!   - `blockdetect`, `photosensitivity` are both multi-frame or
//!     full-academic-paper algorithms (block-grid period search; a temporal
//!     luminance-flash detector needing a rolling multi-frame window) that
//!     this pass did not have time to measure precisely enough to trust,
//!     given how badly `bitplanenoise` and `siti` (below) punished an
//!     optimistic "looks closed-form" read. Left for a follow-up rather
//!     than shipped under-measured. `scdet`, the third filter this
//!     paragraph used to name, is no longer in this list — see the #113
//!     pass note above.
//!   - `bitplanenoise` and `siti` were investigated at length and are the
//!     two real findings of this pass, not scope-cut for time: `bitplanenoise`'s
//!     noise ratio for a maximally-noisy fixture holds its **numerator**
//!     constant (`4`) while scaling its denominator with frame width in a
//!     way no per-pixel bit-difference formula this pass tried reproduced
//!     (tested horizontal, vertical, and vertical-with-wraparound
//!     hypotheses; none fit `w=3` through `w=16` simultaneously). `siti`'s
//!     `SI` (Sobel-gradient standard deviation) matched the textbook
//!     ITU-T P.910 formula **exactly** on a maximum-contrast fixture
//!     (`0`/`255` split, `356.93` predicted and measured) but the *same*
//!     formula on the *same* spatial pattern at a smaller amplitude
//!     (`100`/`120`) predicted `27.99` against a measured `33.59` — a ~20%
//!     miss that a linear operator (which Sobel-then-variance is) cannot
//!     produce from a pure amplitude change, ruling out "the constant is
//!     slightly off" as an explanation. Both are recorded in this crate's
//!     docs with the exact probes, per this crate's own falsification
//!     discipline: an SSIM-style "measured, not byte-exact, and here is
//!     the number that proves it" is honest; guessing a formula that
//!     passes the one fixture it was fit to is not.
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
