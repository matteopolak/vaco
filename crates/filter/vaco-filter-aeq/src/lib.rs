//! T2 audio EQ filters: the biquad family (`equalizer`, `bass`, `lowshelf`,
//! `treble`, `highshelf`, `tiltshelf`, `highpass`, `lowpass`, `bandpass`,
//! `bandreject`, `allpass`, `biquad`) plus `anequalizer`, `firequalizer`,
//! `superequalizer`.
//!
//! FT-4.8a (GitHub #471), one of two children FT-4.8 (#56) split into for
//! single-writer ownership — the other is `vaco-filter-audio-dynamics`
//! (#472). Built against `vaco-filter-core` (the `Filter` trait, the
//! `Simple` adapter) and `vaco-filter-graph` (`FilterRegistry`), exactly as
//! `vaco-filter-audio` is.
//!
//! # Scope versus the brief that requested this crate
//!
//! The brief that requested this crate named thirteen filters (missing
//! `tiltshelf` and `firequalizer`); GitHub #471 — checked directly with
//! `gh issue view`, per this project's standing practice of verifying an
//! epic's real children rather than trusting a brief's restatement of them —
//! names fifteen: "the biquad family (12 filters from one file) plus
//! `anequalizer`, `firequalizer`, `superequalizer`". Counting
//! `ffmpeg -filters` directly (D17: measure, don't recall) confirms twelve
//! registered names share `af_biquads.c`'s option class
//! (`bass`/`lowshelf` print the identical class string `bass/lowshelf`;
//! `treble`/`highshelf`/`tiltshelf` print `treble/high/tiltshelf`), so the
//! twelve are `equalizer`, `bass`, `lowshelf`, `treble`, `highshelf`,
//! `tiltshelf`, `highpass`, `lowpass`, `bandpass`, `bandreject`, `allpass`,
//! `biquad` — all fifteen are implemented here.
//!
//! # Shape
//!
//! * [`vaco_filter_adsp::biquad`] — the Audio EQ Cookbook biquad math:
//!   coefficient formulas and their frequency-response verification. This
//!   was FT-4.8a's "hard part", built here as a crate-private `engine`
//!   module; moved to `vaco-filter-adsp` (D19) once three other crates
//!   needed the same math and found this crate's copy `pub(crate)` and
//!   unreachable. See that module's doc for what makes its tests a real
//!   oracle rather than a second transcription of the same formula.
//! * [`common`] — option parsing shared by the biquad-family filters, and
//!   [`common::Biquad`], the `FrameFilter` every one of them but `tiltshelf`
//!   *is*.
//! * [`sample`] — the same f64-domain frame decode/encode
//!   `vaco-filter-audio::sample` uses, duplicated rather than shared (see
//!   that module's doc for why).
//! * One module per filter, each exposing `pub const DESC: FilterDesc` and
//!   `pub(crate) fn create`, aggregated by [`registry::EqRegistry`].
//!
//! # What is verified versus structural
//!
//! Numerically verified against the cookbook's own frequency-response
//! predictions (`-3 dB` points, design-frequency gains, DC/Nyquist
//! behaviour): `lowpass`, `highpass`, `bandpass` (both `csg` states),
//! `bandreject`, `allpass` (both orders), `equalizer`, `bass`/`lowshelf`,
//! `treble`/`highshelf`, `tiltshelf`. Structural — present, exercised on
//! their common path, but not held to a frequency-response oracle in the
//! same way — `biquad` (coefficients are user-supplied, so there is no
//! design formula to check against beyond finiteness), `anequalizer` (its
//! `params` grammar is read from the reference's documented syntax, not
//! measured against a running filter), `superequalizer` (an IIR
//! approximation of an FFT-domain reference filter bank), `firequalizer`
//! (`gain_entry` control points only; the general `gain` expression is not
//! implemented). See `docs/filter/vaco-filter-aeq.md`.
#![forbid(unsafe_code)]

pub mod allpass;
pub mod anequalizer;
pub mod bandpass;
pub mod bandreject;
pub mod bass;
pub mod biquad;
mod common;
pub mod equalizer;
pub mod firequalizer;
pub mod highpass;
pub mod highshelf;
pub mod lowpass;
pub mod lowshelf;
pub mod registry;
mod sample;
pub mod superequalizer;
pub mod tiltshelf;
pub mod treble;

pub use registry::EqRegistry;
