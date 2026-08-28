//! T3 audio analysis and measurement filters (FT-4.13c, GitHub #483).
//!
//! Built against `vaco-filter-core` (the `Filter` trait, the `Simple`
//! adapter) and `vaco-filter-graph` (`FilterRegistry`), exactly as
//! `vaco-filter-audio`/`-eq`/`-dynamics` are; the two-input filters here
//! (`apsnr`, `asdr`, `asisdr`) go through `vaco-filter-framesync`'s
//! `Synced` adapter instead, the same way
//! `vaco-filter-adynamics::sidechaincompress` does.
//!
//! # Scope versus the brief that requested this crate
//!
//! GitHub #483 suggests membership by name (`aspectralstats`, `ashowinfo`,
//! `aphasemeter`, an `astats`-adjacent filter, `ebur128`, `replaygain`,
//! `apsnr`, `asdr`, `asisdr`, `axcorrelate`, an `aformat`-adjacent probing
//! filter, an `adrc`-adjacent analysis filter, and
//! `showfreqs`/`showspectrum`/`showvolume`/`showwaves`) but says plainly not
//! to trust that list. Measured against `ffmpeg -filters` / `ffmpeg -h
//! filter=<name>` (D17) and cross-checked against plan
//! `16-filters.md` §4.3, whose `vaco-filter-aanalysis` row is exactly this
//! work package's origin (`16 §8.4`, per the GitHub issue), the list was
//! wrong in four ways:
//!
//! 1. **`showfreqs`/`showspectrum`/`showvolume`/`showwaves` do not belong
//!    here.** They are `A->V` visualisers; plan 16 §4.3 gives them their
//!    own crate, `vaco-filter-avvis` (alongside `showspectrumpic`,
//!    `showcqt`, `showcwt`, `showspatial`, `showwavespic`, `avectorscope`,
//!    `a3dscope`, `abitscope`, `ahistogram`, `spectrumsynth`) — a
//!    different, separately-assigned T3 work package, not a sibling
//!    grouping inside this one.
//! 2. **There is no "`adrc`-adjacent analysis" filter.** `adrc` ("Audio
//!    Spectral Dynamic Range Controller") modifies the signal; plan 16
//!    §4.3 places it under `vaco-filter-adynamics`, not
//!    `vaco-filter-aanalysis`.
//! 3. **There is no "`aformat`-adjacent probing" filter.** `aformat`
//!    itself is a T1 format-conversion filter (`vaco-filter-aformat` in
//!    the plan, already registered by `vaco-filter-audio`); nothing in the
//!    reference's audio-measurement family is adjacent to it.
//! 4. **`axcorrelate` is real and does belong in this family, but is
//!    already taken.** `vaco-filter-achannel` (FT-4.13b, GitHub #482 — a
//!    *different* work package's brief, apparently also handed
//!    `axcorrelate`) landed first and already registers it. Plan 16 §4.3
//!    places `axcorrelate` under `vaco-filter-aanalysis` (this crate's
//!    mapping), not under any channel/mixing row, so the plan of record
//!    agrees with this crate rather than with `-achannel` — but the
//!    dup-check gate cares about what is *registered*, not about which
//!    plan row is more textually correct, so this crate does not
//!    re-register it. A genuine two-issue overlap, flagged for the
//!    orchestrator rather than resolved by editing a crate this agent does
//!    not own.
//!
//! What plan 16 §4.3's `vaco-filter-aanalysis` row actually names is
//! fourteen filters: `astats`, `aspectralstats`, `ebur128`, `drmeter`,
//! `silencedetect`, `replaygain`, `apsnr`, `asdr`, `asisdr`, `axcorrelate`,
//! `aderivative`, `aintegral`, `ashowinfo`, `aphasemeter`. Three of those —
//! `astats` and `silencedetect` (already registered by
//! `vaco-filter-adynamics`, whose own scope drifted to include them)
//! and `axcorrelate` (point 4 above) — are excluded here; registering any
//! of them would be exactly the D19 violation the dup-check gate exists to
//! catch. This crate implements and registers the remaining eleven.
//!
//! # Shape
//!
//! * [`common`] — option parsing shared by every filter here, plus
//!   [`common::PairStats`] (the accumulator `apsnr`/`asdr`/`asisdr` each
//!   reduce differently) and [`common::INPUT01_PADS`].
//! * [`sample`] — the same f64-domain frame decode/encode every sibling
//!   audio crate duplicates; see that module's doc for why.
//! * [`kweight`] — the ITU-R BS.1770-4 K-weighting filter design.
//! * [`loudness`] — the gated BS.1770-4 loudness scanner built on
//!   `kweight`, shared by `ebur128` and `replaygain`.
//! * One module per filter, each exposing `pub const DESC: FilterDesc` and
//!   `pub(crate) fn create`, aggregated by [`registry::AmeasureRegistry`].
//!
//! # What is verified versus structural
//!
//! Numerically verified against independent closed forms or published,
//! ffmpeg-independent formulas (see each module's own doc for its specific
//! oracle): `aderivative`/`aintegral` (a round-trip property), `apsnr`/
//! `asdr`/`asisdr` (hand-computable two-sample cases, and the scale-
//! invariance contrast between `asdr` and `asisdr` specifically),
//! `aphasemeter` (Pearson-correlation fixed points),
//! `aspectralstats` (synthetic spectra with known shapes), `drmeter` (the
//! published TT DR Meter algorithm's own fixed point and monotonicity),
//! `ebur128`/`replaygain` (a calibrated loudness reference tone, and the
//! BS.1770-4 gating algorithm's absolute-gate behaviour). Structural —
//! present and exercised, but not held to an independent oracle in the
//! same way — `ashowinfo` (the checksum field is a diagnostic, not a
//! match to the reference's unmeasured algorithm; see that module's doc).
//! `ebur128`'s and `aphasemeter`'s video output, and BS.1770's true-peak
//! oversampling filter, are documented gaps, not implemented at all. See
//! `docs/filter/vaco-filter-aanalysis.md`.
#![forbid(unsafe_code)]

pub mod aderivative;
pub mod aintegral;
pub mod aphasemeter;
pub mod apsnr;
pub mod asdr;
pub mod ashowinfo;
pub mod asisdr;
pub mod aspectralstats;
mod common;
pub mod drmeter;
pub mod ebur128;
mod kweight;
mod loudness;
pub mod registry;
pub mod replaygain;
mod sample;

pub use registry::AmeasureRegistry;
