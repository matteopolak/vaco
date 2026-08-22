//! Matroska and `WebM` demuxing.
//!
//! ```no_run
//! use vaco_demux_matroska::MatroskaDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::{MemorySource, MediaSource};
//!
//! let bytes = std::fs::read("clip.mkv")?;
//! let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
//! let mut demux = MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
//! for stream in demux.streams() {
//!     println!("{} {:?} {}", stream.index, stream.params.codec_id, stream.time_base);
//! }
//! while let Ok(pkt) = demux.read_packet() {
//!     println!("{} {:?}", pkt.stream_index, pkt.pts);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`ebml`] | the whole EBML layer: VINTs, the element grammar, the schema table, unknown-size termination |
//! | [`block`] | `Block`/`SimpleBlock` headers and all four lacings |
//! | [`codec`] | `CodecID` string to [`vaco_codec_core::CodecId`] |
//! | [`probe`] | content detection |
//! | [`synth`] | a minimal EBML writer, for fixtures the reference muxer cannot produce |
//!
//! [`ebml`] deliberately depends on nothing else in this crate. Nothing else in
//! the workspace parses EBML today, so the layer lives here; when a Matroska
//! muxer or another EBML format wants it, moving the module to
//! `vaco-format-ebml` is a file move and a manifest edit.
//!
//! # The three things that make Matroska different from MP4
//!
//! 1. **The time base is not the usual one.** `TimestampScale` is nanoseconds
//!    per tick and defaults to 1 000 000, so the stream time base is `1/1000` —
//!    shared by every track in the segment, not per-track as in MP4.
//! 2. **Sizes may be unknown.** A `Segment` or a `Cluster` may declare no size
//!    at all, which is what a live `WebM` stream does. Terminating one needs the
//!    element schema, not the byte count: RFC 8794 section 6.2 ends it at the
//!    first element that is not a legal child. [`ebml::Stack`] is that rule.
//! 3. **One block may be many packets.** All four lacings pack several frames
//!    into one `Block`, so `read_packet` is a queue over a parser rather than a
//!    one-element-one-packet loop.
//!
//! # Specification
//!
//! RFC 9559 (Matroska), RFC 8794 (EBML), `draft-ietf-cellar-codec` for the codec
//! ID registry, and the `WebM` Project's container guidelines for the `webm`
//! `DocType` profile. Where behaviour is not in any of those it was measured
//! against `ffprobe 8.1`, and every such row says so at its use site.
//!
//! # Configuration
//!
//! [`vaco_format_core::FormatOptions`] as passed to
//! [`MatroskaDemuxer::open`]; `max_streams` bounds the track count and the
//! index options bound the `Cues`-derived index. Allocation is bounded by a
//! [`vaco_limits::Budget`] built from [`vaco_limits::Limits::permissive`].
//!
//! # Dependencies
//!
//! `vaco-format-core` for the traits, `vaco-io` for the byte source,
//! `vaco-packet`/`vaco-codec-core`/`vaco-chlayout`/`vaco-color` for the data
//! model, `vaco-limits` for the budget, `vaco-core` for exact rational and
//! timestamp arithmetic, and `miniz_oxide` for zlib content encodings.

#![forbid(unsafe_code)]

pub mod block;
pub mod codec;
mod demux;
pub mod ebml;
pub mod probe;
pub mod synth;

pub use demux::{DEMUXER, FLAGS, MatroskaDemuxer};
