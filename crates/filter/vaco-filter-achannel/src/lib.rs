//! T3 audio channel, layout and mixing filters: `axcorrelate`, `crossfeed`,
//! `earwax`, `extrastereo`, `haas`, `stereotools`, `stereowiden`.
//!
//! FT-4.13b (GitHub #482). Built against `vaco-filter-core` (the `Filter`
//! trait, the `Simple` adapter) and `vaco-filter-graph` (`FilterRegistry`),
//! exactly as `vaco-filter-audio`, `vaco-filter-audio-eq` and
//! `vaco-filter-audio-dynamics` are — `axcorrelate` additionally uses
//! `vaco-filter-framesync`'s `Synced` adapter, since it genuinely has two
//! inputs to align (the same reason `vaco-filter-audio-dynamics::
//! sidechaincompress` uses it).
//!
//! # Scope versus the brief that requested this crate, and versus the
//! reference binary
//!
//! The brief named a channel/layout/mixing family drawn loosely from
//! `amerge`, `amix`, `asplit`-adjacent work, `channelmap`, `channelsplit`,
//! `join`, `pan`, `surround`, `stereotools`, `stereowiden`, `haas`,
//! `crossfeed`, `earwax`, `axcorrelate`, `headphone`, `sofalizer` — and said
//! plainly not to trust that list in either direction. Checking
//! `crates/filter/*/vaco-component.toml` directly (D19: register nothing
//! already registered) shows `amerge`, `amix`, `channelmap`, `channelsplit`,
//! `join` and `pan` already registered by `vaco-filter-audio`, and `asplit`
//! already registered by `vaco-filter-plumbing` — seven of the brief's names
//! are already owned elsewhere, so this crate does not touch them.
//!
//! Counting `ffmpeg -filters` directly (D17) turned up one more member the
//! brief's list missed entirely: `extrastereo` ("Increase difference between
//! stereo audio channels") is exactly this family and is implemented here.
//! `amultiply`, `ainterleave` and `acrossfade` also take multiple audio
//! inputs but were *not* added — they multiply/interleave/crossfade in time,
//! not channel layout or mixing, so they are a different family.
//!
//! Three names are **not** implemented:
//!
//! * **`sofalizer`** does not exist in the reference binary this project was
//!   measured against (`ffmpeg -h filter=sofalizer` -> `Unknown filter`; the
//!   local build lacks `--enable-libmysofa`). There is nothing to be
//!   sample-exact against, so it cannot be measured at all, per D17.
//! * **`headphone`** needs a full HRTF convolution engine driven by
//!   caller-supplied impulse-response streams and a non-trivial channel-
//!   mapping grammar (`map`) — anticipated by this work package's brief as a
//!   likely skip, and confirmed as one after reading the option table.
//! * **`surround`** is an STFT/overlap-add upmix with per-channel spread and
//!   twenty `win_func` choices — a second FFT-domain filter bank on the
//!   scale of `vaco-filter-audio-eq::superequalizer`, disproportionate to
//!   this work package's pace target. Flagged here rather than silently
//!   dropped.
//!
//! Seven implemented plus seven already registered elsewhere plus three
//! skipped is fourteen — matching GitHub #482's own "~14" estimate, once the
//! overlap with `vaco-filter-audio`/`vaco-filter-plumbing` is accounted for.
//!
//! # What is measured versus structural
//!
//! Every module doc states its own evidence. In summary: `extrastereo`,
//! `stereowiden`, `stereotools`' eleven modes and level/balance/mute/phase
//! options, and `earwax`'s full 32-tap FIR (at 44100 Hz) are sample-exact
//! against `ffmpeg` 8.1, derived by probing the binary (D17), not by reading
//! its source (D7). `haas` is sample-exact for the reference's own default
//! options; other `middle_source`/`middle_phase` combinations are a
//! structural extension of that measured shape. `crossfeed`'s `strength=0`
//! gain-only case is measured exactly; its `strength > 0` crossfeed shape is
//! structural. `axcorrelate`'s three-way sign/magnitude behaviour
//! (identical/inverted/uncorrelated) is measured; whether the reference
//! demeans its window first is not distinguishable from the outside and is
//! documented as an open question. See `docs/filter/vaco-filter-achannel.md`.
#![forbid(unsafe_code)]

pub mod axcorrelate;
mod common;
pub mod crossfeed;
pub mod earwax;
pub mod extrastereo;
pub mod haas;
pub mod registry;
mod sample;
pub mod stereotools;
pub mod stereowiden;

pub use registry::AchannelRegistry;
