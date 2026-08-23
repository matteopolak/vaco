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
//! `vaco-filter-ameasure::kweight` and `vaco-filter-audio-dynamics::mcompand`
//! each duplicated the cookbook formulas outright. D19 says a shared concept
//! moves to a shared home rather than staying duplicated *or* staying
//! falsely believed to have one owner; this module is that move, and all
//! four crates now depend on it instead (`vaco-filter-aeq`,
//! `vaco-filter-ameasure` and `vaco-filter-audio-dynamics` are renamed to
//! `-aeq`/`-aanalysis`/`-adynamics` in a separate, later commit per
//! `planning/16-filters.md` §4.3 — this move happened under their original
//! names). The row's remaining two kernels — the EBU R128 loudness core and
//! partitioned FIR convolution — still have no caller in this crate and are
//! not added speculatively; whoever needs one next should add it here
//! rather than duplicating it, per the same rule.
//!
//! # Design note: no FFT
//!
//! The plan's row also lists a "phase-vocoder core" alongside WSOLA. `atempo`
//! is implemented here with plain time-domain WSOLA (windowed cross-
//! correlation search for the best splice point, then overlap-add) rather
//! than a phase vocoder, so this crate has no `vaco-tx` dependency yet. WSOLA
//! is the standard technique for this exact problem (time-domain
//! pitch-preserving tempo change) and needs no FFT; a phase-vocoder path can
//! be added by a future caller that specifically needs its trade-offs
//! (smoother transients at extreme ratios, at the cost of a dependency on
//! `vaco-tx`) without disturbing this one.
#![forbid(unsafe_code)]

pub mod biquad;
pub mod wave;
pub mod wsola;
