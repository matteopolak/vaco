//! The AVI muxer: `hdrl`/`strl` header, `movi` chunks, `idx1`.
//!
//! Companion to `vaco-demux-avi`, and deliberately independent of it at the
//! source level (D19's "one definition per concept" is about shared
//! *concepts*, and a reader's parse state and a writer's serialise state are
//! not the same concept just because they describe the same bytes) — but
//! this crate's tests do depend on it, to verify what gets written actually
//! demuxes back to what was asked for.
//!
//! # What is supported
//!
//! Video: H.264, HEVC, VP8, VP9, MJPEG, PNG — the codecs
//! [`vaco_codec_core::CodecId`] has a variant for and this crate has a
//! `biCompression` `FourCC` for. Audio: PCM (constant-bitrate, `dwSampleSize`
//! set), MP3, AAC (both variable-bitrate, one chunk per frame). Anything else
//! is [`vaco_core::Error::Unsupported`] from [`Muxer::add_stream`] rather than
//! a guessed tag a reader would misidentify.
//!
//! # What is deferred
//!
//! `OpenDML` (`indx`/`ix##`) writing, for files that would exceed the ~1 GiB
//! practical `idx1`-only limit — see `docs/format/vaco-mux-avi.md`. `idx1`
//! alone is what `vaco-demux-avi` measured `ffmpeg 8.1` itself writing for
//! ordinary files, so this covers the common case.

#![forbid(unsafe_code)]

pub mod mux;

pub use mux::{AviMuxer, MUXER};
