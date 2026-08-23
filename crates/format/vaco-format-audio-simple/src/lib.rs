//! `wav`, `w64`, `aiff`, `caf`, `au`, `voc`, `sox`, `ircam`, `rso` — demux and
//! mux, all nine in one crate.
//!
//! Per plan `18-formats.md` §3.4.6: these nine "share so much structure that
//! one crate carrying [nine] demuxers and [nine] muxers is the right
//! factoring." In practice the shared structure is narrower than that
//! framing suggests — WAV and W64 are genuinely thin over
//! `vaco-format-riff`, but AIFF/CAF/AU/VOC/SOX/IRCAM/RSO each have their own
//! header shape and are not built on a shared parser the way WAV is. What
//! *is* shared, in [`pcm`], is the second half every one of the nine reduces
//! to once its header is parsed: a single stream of raw interleaved audio,
//! framed into packets and timestamped from a running sample count. See
//! [`pcm`]'s module docs.
//!
//! # What is in here
//!
//! | Module | Format | Demux | Mux |
//! |---|---|---|---|
//! | [`wav`] | WAV / RF64 | thin over `vaco-format-riff` | plain `WAVEFORMATEX` PCM |
//! | [`w64`] | Sony Wave64 | thin over `vaco-format-riff` (128-bit GUIDs instead of `FourCCs`) | plain PCM |
//! | [`aiff`] | AIFF / AIFF-C | full chunk walk, extended80 sample rate | big-endian integer PCM |
//! | [`caf`] | Apple CAF | full chunk walk, native 64-bit signed sizes | PCM, `lpcm` only |
//! | [`au`] | Sun/NeXT `.au` | fixed 24-byte header | signed/float/A-law/µ-law PCM |
//! | [`voc`] | Creative Voice | block-chain walk (not one contiguous span — see [`voc`]) | one type-9 block, 16-bit PCM |
//! | [`sox`] | `SoX` native | fixed header, always 32-bit samples | 32-bit signed PCM |
//! | [`ircam`] | BICSF/IRCAM | fixed 1024-byte header | 16-bit signed PCM |
//! | [`rso`] | Lego Mindstorms RSO | 8-byte header, no public spec (black-box probed) | 8-bit unsigned mono PCM |
//!
//! | Shared | Contents |
//! |---|---|
//! | [`pcm`] | [`pcm::RawPcmDemuxer`], [`pcm::sample_fmt_for`], [`pcm::params`] — the data-pointer half every format above reduces to |
//! | [`extended80`] | AIFF's 80-bit IEEE-754 extended-precision sample rate |
//!
//! # Reuse of `vaco-format-riff`
//!
//! WAV and W64 are the two formats this crate did **not** have to write a
//! header parser for: `vaco-format-riff::wave::WaveFormatEx` (the `fmt `
//! payload), `vaco_format_riff::wave_tags` (`wFormatTag` → codec identity)
//! and `vaco_format_riff::rf64::Ds64` (the `RF64` 64-bit-size extension) are
//! all used as-is. [`wav`] and [`w64`] contribute only the chunk walk that
//! finds `fmt `/`data` and the [`vaco_format_core::Demuxer`]/
//! [`vaco_format_core::Muxer`] glue.
//!
//! # Sample format gaps this crate did not introduce
//!
//! `vaco-sampfmt::SampleFmt` has no 24-bit variant. This is not a problem
//! for any of the nine formats: **measured** against `ffmpeg`/`ffprobe` 8.1,
//! the reference's own *working* sample format for 24-bit container PCM is
//! 32-bit (`sample_fmt=s32`, `bits_per_raw_sample=24`), not a packed 24-bit
//! type — see [`pcm::sample_fmt_for`]'s docs for the measurement. So there
//! was no gap to route around once the right question ("what does the
//! reference call this?", not "what would a natural Rust type look like?")
//! was asked.
//!
//! # What every format here defers
//!
//! Metadata surfaces named in plan `18-formats.md` §3.4.6 — WAV's
//! `LIST/INFO`, `cue `/`adtl`, `bext`, `iXML`; AIFF's `MARK`/`INST`; CAF's
//! `info`/`chan` — are not read into `Stream::metadata`/chapters. Every
//! format demuxes and (except IRCAM/RSO, no metadata surface exists to skip)
//! ignores its own metadata chunks rather than misreading them; see each
//! module's docs for exactly what is skipped. Per the brief: nine formats
//! present with the audio path working beats one format's metadata surface
//! complete.
//!
//! Also deferred: none of the nine `Xxx::open` associated functions accepts
//! a [`vaco_limits::Limits`] override the way `vaco-demux-mpegts::MpegTsDemuxer
//! ::open_with_limits` does — each opens under a fixed internal
//! `Limits::permissive()` budget. Every parse is still bounded (nothing
//! allocates without going through that budget, and `fuzz/fuzz_targets/
//! audio_simple_demux.rs` exercises exactly this), but an embedder wanting a
//! tighter cap than "permissive" has no lever to pull yet.

#![forbid(unsafe_code)]

pub mod aiff;
pub mod au;
pub mod caf;
pub mod extended80;
pub mod ircam;
pub mod pcm;
pub mod rso;
pub mod sox;
pub mod voc;
pub mod w64;
pub mod wav;
