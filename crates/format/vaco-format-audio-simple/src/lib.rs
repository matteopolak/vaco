//! Demux and mux for nine simple audio containers.
//!
//! | Module | Format | Demux | Mux |
//! |---|---|---|---|
//! | [`wav`] | WAV / RF64 | thin over `vaco-format-riff` | `WAVEFORMATEX` PCM |
//! | [`w64`] | Sony Wave64 | thin over `vaco-format-riff` | PCM |
//! | [`aiff`] | AIFF / AIFF-C | chunk walk, extended80 sample rate | PCM, `AIFC` when the codec needs a compression type |
//! | [`caf`] | Apple CAF | chunk walk, 64-bit sizes | `lpcm`, `alaw`, `ulaw` |
//! | [`au`] | Sun/NeXT `.au` | fixed 24-byte header | signed/float/A-law/µ-law PCM |
//! | [`voc`] | Creative Voice | block-chain walk | one type-9 block, 16-bit PCM |
//! | [`sox`] | `SoX` native | fixed header | 32-bit signed PCM |
//! | [`ircam`] | BICSF/IRCAM | fixed 1024-byte header | 16-bit signed PCM |
//! | [`rso`] | Lego Mindstorms RSO | 8-byte header, no public spec | 8-bit unsigned mono PCM |
//!
//! Once a header is parsed every one of them reduces to the same thing: a
//! single stream of raw interleaved audio, framed into packets and
//! timestamped from a running sample count. That half lives in [`pcm`].
//!
//! None of the nine reads its metadata chunks — WAV's `LIST`/`INFO`, AIFF's
//! `MARK`, CAF's `info` — and none accepts a [`vaco_limits::Limits`]
//! override; each opens under `Limits::permissive()`.

#![forbid(unsafe_code)]

pub mod aiff;
pub mod au;
pub mod caf;
pub mod extended80;
pub mod flac;
pub mod ircam;
pub mod pcm;
pub mod rso;
pub mod sox;
pub mod voc;
pub mod w64;
pub mod wav;
