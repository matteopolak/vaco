//! The ASF (Advanced Systems Format) muxer: `.asf`/`.wmv`/`.wma`, registered
//! as `asf` and `asf_stream`.
//!
//! # Source
//!
//! Microsoft, *"Advanced Systems Format (ASF) Specification"*, Revision
//! 01.20.06 — clean-room from the document plus black-box probing of
//! `ffmpeg 8.1`'s own `asf`/`asf_stream` muxer output (D6/D7/D17); see
//! `docs/format/vaco-mux-asf.md` for the exact probes.
//!
//! # What is supported
//!
//! Video: H.264, HEVC, VP8, VP9, MJPEG, PNG (via the same generic
//! `BITMAPINFOHEADER`/`biCompression` mechanism `vaco-mux-avi` uses) and
//! VC-1 (`WMV3`, ASF's own native video codec). Audio: PCM, MP3, AAC, and
//! Windows Media Audio 1/2/9-Pro. Anything else is
//! [`vaco_core::Error::Unsupported`] from [`Muxer::add_stream`].
//!
//! # What is deferred
//!
//! * **Compressed payloads** ([\[ASF\] §5.2.3.2/.4](vaco_format_asf)) are
//!   never written, only read (by `vaco-demux-asf`). Every packet this
//!   muxer writes uses the ordinary (uncompressed) payload shape, which is
//!   always legal — compressing is a space optimisation the spec explicitly
//!   marks optional.
//! * **The top-level Index Object** ([\[ASF\] §6.2](vaco_format_asf)) is not
//!   written; only the Simple Index Object, one per video stream, is.
//! * **Digital rights management** is neither written nor required —
//!   irrelevant to a muxer that only ever writes plaintext content.
//!
//! ```no_run
//! use vaco_codec_core::{CodecId, CodecParameters};
//! use vaco_format_core::{FormatOptions, Muxer};
//! use vaco_io::{IoWriter, MediaSink};
//! # fn sink() -> Box<dyn MediaSink> { unimplemented!() }
//! # fn main() -> vaco_core::Result<()> {
//! let mut mux = vaco_mux_asf::AsfMuxer::new(sink(), &FormatOptions::default())?;
//! let idx = mux.add_stream(&CodecParameters::audio().with_codec(CodecId::PcmS16le))?;
//! mux.write_header()?;
//! // ...write_packet(...) for each sample, then...
//! mux.write_trailer()?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod codec;
pub mod mux;

pub use mux::{AsfMuxer, MUXER, MUXER_STREAM};
