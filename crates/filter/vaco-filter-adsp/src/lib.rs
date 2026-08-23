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
//! This crate did not exist before FT-4.13d (GitHub #484), so it is created
//! here, minimally: only [`wave`] (wave-table generation and a sample-rate-
//! driven LFO walker) and [`wsola`] (the time-domain WSOLA core `atempo`
//! needs) are implemented, because those are the only two kernels plan
//! §4.1's row lists that this work package's filters actually call. The
//! row's other three kernels — biquad coefficient design, the EBU R128
//! loudness core, and partitioned FIR convolution — are **not** added here:
//! `vaco-filter-audio-eq::biquad` already owns biquad design (D19: do not
//! write a second one), and neither R128 nor partitioned FIR has a caller in
//! this crate. Whoever needs one of those next should add it here rather
//! than duplicating it in their own crate, per the same rule.
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

pub mod wave;
pub mod wsola;
