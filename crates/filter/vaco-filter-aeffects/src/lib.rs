//! T3 audio effects and modulation filters: echo/delay (`aecho`, `adelay`,
//! `compensationdelay`), LFO-modulated filters (`chorus`, `flanger`,
//! `aphaser`, `tremolo`, `vibrato`, `apulsator`), dynamics-adjacent
//! processors (`crystalizer`, `aexciter`, `deesser`, `dialoguenhance`,
//! `virtualbass`, `dcshift`), tempo (`atempo`), and the stereo/channel
//! family this crate started with (`crossfeed`, `stereotools`,
//! `stereowiden`, `extrastereo`, `earwax`, `haas`, `axcorrelate`).
//!
//! FT-4.13d (GitHub #484). This crate is `planning/16-filters.md` §4.3's
//! `vaco-filter-aeffects` row; it was originally built (FT-4.13b, GitHub
//! #482) under the name `vaco-filter-achannel`, which named only the
//! channel/mixing subset it had at the time. Renamed to match the plan's
//! row per `planning/FILTER-CRATE-DIVERGENCE.md` as part of landing this
//! work package, in a commit kept separate from the new filters so the move
//! is reviewable on its own. Built against `vaco-filter-core` (the `Filter`
//! trait, the `Simple` adapter) and `vaco-filter-graph` (`FilterRegistry`),
//! exactly as `vaco-filter-audio`, `vaco-filter-aeq` and
//! `vaco-filter-audio-dynamics` are — `axcorrelate` additionally uses
//! `vaco-filter-framesync`'s `Synced` adapter, since it genuinely has two
//! inputs to align. The LFO-driven filters share `vaco-filter-adsp`'s
//! wave-table module for their oscillators, and `atempo` uses that crate's
//! WSOLA core.
//!
//! # Every plan-row name against the reference, and what is missing
//!
//! Every name in plan §4.3's `aeffects` row exists in `ffmpeg -filters` and
//! `ffmpeg -h filter=<name>` (checked directly, D17) with matching media
//! type (`A->A`, or `N->A` for `headphone`) — the row and the reference
//! agree in both directions; no filter needed adding to or removing from
//! the row. Of the twenty-five filters it names, twenty-two are implemented
//! here (the seven pre-existing plus fifteen new this pass). Three are
//! not:
//!
//! * **`surround`** and **`headphone`** were flagged by this crate's
//!   original author as disproportionately large for this project's pace
//!   (an STFT/overlap-add upmix and a full HRTF convolution engine,
//!   respectively — each roughly the scale of
//!   `vaco-filter-aeq::superequalizer`) and remain unimplemented for
//!   the same reason; picking either one up is future work, not a gap
//!   introduced here.
//! * **`hdcd`** decodes a proprietary, bit-level companding/peak-extend
//!   scheme that reads control codes out of the audio's own low-order bits
//!   (with a code-detect timer, per-channel gain matching, and a
//!   configurable "valid bits" location) — reverse-engineering that from
//!   black-box probing alone, to the standard this project holds itself to,
//!   is a project on the scale of a dedicated work package, not a line item
//!   in this one. Left unimplemented rather than shipped as a guess.
//!
//! # What is measured versus structural
//!
//! Every module states its own evidence; this is the summary. **Sample-exact**
//! against `ffmpeg` 8.1, derived by probing the binary (D17) rather than
//! reading its source (D7): `dcshift` (plain-shift path), `adelay`,
//! `compensationdelay`, `aecho`, `tremolo`, `apulsator` (`mode=sine,
//! timing=hz, width=1`), `crystalizer`, `extrastereo`, `stereowiden`,
//! `stereotools`, `earwax`, and `deesser` at its default `i=0`. **Exact by
//! construction** (an algebraic property of this module's own formula, not
//! a live comparison, usually because the reference either has no
//! corresponding option value to probe or rejects one that would let it be
//! measured): `aecho`'s zero-decay identity (the reference itself rejects
//! `decays=0`), `flanger`'s all-defaults identity, `aphaser`'s
//! zero-decay pure-gain case, `chorus`'s zero-decays dry-only case,
//! `vibrato`'s zero-depth identity, `aexciter`'s zero-amount pure-gain
//! case. **Structural, not measured** (a standard DSP technique
//! implemented directly rather than reverse-engineered, because the effect
//! has no discrete-impulse signature to probe or needs a shared kernel this
//! project does not have yet): `chorus`, `flanger`, `aphaser`, `vibrato`'s
//! exact modulation shape; `apulsator`'s `width != 1` and `bpm`/`ms`
//! timing; `aexciter`, `deesser`, `virtualbass` one-pole band splits —
//! *tried* a real biquad from `vaco-filter-adsp` in all three once it
//! became reachable, measured it against the reference, and it made the
//! match worse in two cases and no measurable difference in the third
//! (see their own module docs for the numbers), so the one-pole design is
//! kept in all three; `dialoguenhance` (measured to
//! *not* be an identity even at its own defaults — a real voice-activity
//! gate, not a mix knob — and implemented as a plain mid/side rebalance
//! instead); `atempo` (WSOLA's window/search shape, not its
//! length-scaling invariants, which are measured against
//! `vaco-filter-adsp::wsola`'s own tests). `haas` is sample-exact at
//! defaults, structural elsewhere. `crossfeed`'s `strength=0` case is
//! measured exactly, `strength > 0` is structural. `axcorrelate`'s sign
//! behaviour is measured; whether the reference demeans its window first is
//! an open question. See `docs/filter/vaco-filter-aeffects.md`.
#![forbid(unsafe_code)]

pub mod adelay;
pub mod aecho;
pub mod aexciter;
pub mod aphaser;
pub mod apulsator;
pub mod atempo;
pub mod axcorrelate;
pub mod chorus;
mod common;
pub mod compensationdelay;
pub mod crossfeed;
pub mod crystalizer;
pub mod dcshift;
pub mod deesser;
pub mod dialoguenhance;
pub mod earwax;
pub mod extrastereo;
pub mod flanger;
pub mod haas;
pub mod registry;
mod sample;
pub mod stereotools;
pub mod stereowiden;
pub mod tremolo;
pub mod vibrato;
pub mod virtualbass;

pub use registry::AeffectsRegistry;
