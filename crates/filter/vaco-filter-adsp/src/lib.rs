//! Shared audio DSP kernels that cross filter-crate boundaries.
//!
//! Plan `16-filters.md` §4.1 places this crate here because `generate_wave_table`
//! (LFO shapes) and a WSOLA time-stretch core are each needed by more than one
//! `vaco-filter-a*` crate — `vaco-filter-aeffects` needs both, for its
//! LFO-driven modulation filters (`tremolo`, `vibrato`, `apulsator`, `chorus`,
//! `flanger`, `aphaser`) and for `atempo` respectively — and D19 says a shared
//! concept gets one definition, not one copy per crate that happens to need
//! it first.
//!
//! This crate did not exist before FT-4.13d (GitHub #484), so it was created
//! minimally at first: only [`wave`] (wave-table generation and a
//! sample-rate-driven LFO walker) and [`wsola`] (the time-domain WSOLA core
//! `atempo` needs) — the two kernels plan §4.1's row lists that FT-4.13d's
//! filters actually called.
//!
//! [`biquad`] (RBJ Audio EQ Cookbook coefficient design) joined next. It was
//! *not* added at FT-4.13d, on the theory that `vaco-filter-aeq::engine`
//! already owned biquad design and a second copy here would violate D19.
//! That theory was wrong in a way that cost real correctness:
//! `vaco-filter-aeq`'s biquad types were `pub(crate)`, not reusable, so
//! every crate that needed one had already written its own —
//! `vaco-filter-aeffects` shipped one-pole approximations in `aexciter`,
//! `deesser` and `virtualbass` specifically because of it, and
//! `vaco-filter-aanalysis::kweight` and `vaco-filter-adynamics::mcompand`
//! each duplicated the cookbook formulas outright. D19 says a shared concept
//! moves to a shared home rather than staying duplicated *or* staying
//! falsely believed to have one owner; this module is that move, and all
//! four crates now depend on it instead (`vaco-filter-aeq`,
//! `vaco-filter-aanalysis` and `vaco-filter-adynamics` are renamed to
//! `-aeq`/`-aanalysis`/`-adynamics` in a separate, later commit per
//! `planning/16-filters.md` §4.3 — this move happened under their original
//! names).
//!
//! # 2026-08-28 addition: [`fir`] (GitHub #461, FT-3.4), and why the EBU
//! R128 core is deliberately *not* also added here
//!
//! Of the row's two remaining kernels, only [`fir`] (partitioned overlap-add
//! FIR convolution) genuinely has no implementation anywhere in the tree —
//! added here per this doc's own long-standing invitation, for the FIR
//! reverb/HRTF-style filters (`headphone`, a possible `afir`) that need it
//! and do not exist yet.
//!
//! The EBU R128 loudness core is a *different* situation from the biquad
//! story above, not the same one repeating: `vaco-filter-aanalysis::loudness`
//! already implements the full BS.1770-4 gated scanner (K-weighting, 100 ms
//! sub-blocks, the 400 ms/3 s gating windows) for `ebur128` and
//! `replaygain`, and it is the *only* implementation — unlike biquad, no
//! second crate independently reinvented a narrower one, because
//! `vaco-filter-adynamics::loudnorm` explicitly declined to (its own doc
//! says plainly "this is not an EBU R128 / ITU-R BS.1770 implementation",
//! and names the simpler RMS-based approximation it uses instead, and why).
//! So there is one owner and zero unmet callers today: adding a second copy
//! here would be pure duplication with no consolidation to perform, the
//! same mistake this crate's own `edge_common`/box-blur/morphology/LUT
//! near-misses in `vaco-filter-vdsp` made for exactly this reason (see that
//! crate's doc). Extracting `vaco-filter-aanalysis::loudness` into a shared
//! home is real, correctly-scoped future work — for whoever next needs a
//! *second* BS.1770 caller, or for a deliberate refactor that also updates
//! `vaco-filter-aanalysis` to depend on the extracted copy, neither of which
//! this pass does.
//!
//! # Design note: no FFT until now
//!
//! The plan's row also lists a "phase-vocoder core" alongside WSOLA. `atempo`
//! is implemented here with plain time-domain WSOLA (windowed cross-
//! correlation search for the best splice point, then overlap-add) rather
//! than a phase vocoder, so this crate had no `vaco-tx` dependency until
//! [`fir`] needed one for its own, unrelated reason (FFT-domain block
//! convolution). WSOLA is the standard technique for time-domain
//! pitch-preserving tempo change and needs no FFT; a phase-vocoder path can
//! still be added by a future caller that specifically needs its trade-offs
//! without disturbing this one.
#![forbid(unsafe_code)]

pub mod biquad;
pub mod fir;
pub mod wave;
pub mod wsola;
