//! Matroska and `WebM` muxing, plus the `webm_chunk` segmented-output muxer.
//!
//! ```no_run
//! use vaco_codec_core::{CodecId, CodecParameters};
//! use vaco_core::{Rational, Timestamp};
//! use vaco_format_core::{FormatOptions, Muxer};
//! use vaco_io::{IoOptions, MediaSink};
//! use vaco_limits::{Budget, Limits};
//! use vaco_mux_matroska::MatroskaMuxer;
//! use vaco_packet::{Packet, PacketFlags};
//!
//! # struct Sink(Vec<u8>);
//! # impl MediaSink for Sink {
//! #     fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> { self.0.extend_from_slice(buf); Ok(()) }
//! #     fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> { Ok(pos) }
//! #     fn position(&self) -> u64 { self.0.len() as u64 }
//! #     fn is_seekable(&self) -> bool { false }
//! #     fn flush(&mut self) -> vaco_core::Result<()> { Ok(()) }
//! # }
//! let mut mux = MatroskaMuxer::new_matroska(Box::new(Sink(Vec::new())), &FormatOptions::default())?;
//! let idx = mux.add_stream(&CodecParameters::video().with_codec(CodecId::H264))?;
//! mux.write_header()?;
//! let mut budget = Budget::new(Limits::strict());
//! let mut pkt = Packet::from_slice(&mut budget, b"payload")?;
//! pkt.stream_index = idx;
//! pkt.pts = Timestamp::ZERO;
//! pkt.dts = Timestamp::ZERO;
//! pkt.flags = PacketFlags::KEY;
//! mux.write_packet(&pkt)?;
//! mux.write_trailer()?;
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`codec`] | [`vaco_codec_core::CodecId`] to Matroska `CodecID` string, and the `webm` codec allow-list |
//! | [`block`] | `SimpleBlock`/`BlockGroup` encoding and the three lacings |
//! | [`mux`] | [`MatroskaMuxer`] — the shared implementation behind `matroska` and `webm` |
//! | [`webm_chunk`] | [`webm_chunk::WebmChunkMuxer`] — numbered-chunk `WebM` output |
//!
//! # What is shared with the demuxer, and how
//!
//! The EBML layer — VINTs, the element header, the writer — lives in
//! [`vaco_format_ebml`] and is used directly here. The Matroska *schema*
//! (element ID constants) is a different thing: RFC 9559's element tree, kept
//! in `vaco-demux-matroska::ebml::schema` because that crate had it first, and
//! reused here as a dependency the same way `vaco-mux-ogg` reuses
//! `vaco-demux-ogg`'s page/CRC definitions (D19) rather than a second table of
//! the same 200-odd IDs.
//!
//! # The size-field problem (see `docs/format/vaco-mux-matroska.md`)
//!
//! `Segment`'s length is not known until every packet has been written.
//! Measured against `ffmpeg 8.1` (`-f matroska out.mkv` vs `-f matroska -`
//! piped to a non-seekable sink, both under `-fflags +bitexact` positioned as
//! an *output* option — see the crate's probing notes): a seekable sink gets
//! the true size patched into the reserved eight-octet field once the trailer
//! runs; a non-seekable sink keeps the RFC 8794 section 6.2 all-ones
//! "unknown size" marker forever. [`mux::MatroskaMuxer::write_trailer`] is
//! exactly that branch. `Cluster`, by contrast, is fully buffered in memory
//! before it is written — measured the same way, a `Cluster`'s size field is
//! always the shortest VINT that holds it, seekable or not — so no seek is
//! needed there at all, on either kind of sink.
//!
//! # What this crate does not do
//!
//! It does not read `webm_dash_manifest` (issue #570, a different crate's
//! scope), and it does not reach for a parser crate to build `CodecPrivate`
//! from raw bitstream extradata — D14.1 forbids a `vaco-mux-*` crate
//! depending on a `vaco-parse-*` one. `CodecPrivate` is
//! [`vaco_codec_core::CodecParameters::extradata`] written verbatim, which is
//! exactly the shape the AVC/HEVC configuration records and the Xiph-laced
//! Vorbis/Opus header packets already have when they arrived from a demuxer
//! that itself stores `CodecPrivate` verbatim into `extradata` (this
//! workspace's own `vaco-demux-matroska` does; see its `codec::private_is_extradata`).
//!
//! # Configuration
//!
//! [`vaco_format_core::FormatOptions`], read once at construction:
//! `fflags=+bitexact` suppresses `DateUTC` (never calling `vaco_time`, since a
//! muxer built for `wasm32` cannot reach the wall clock at all on some
//! targets); `start_time_realtime`, when set and not bitexact, becomes
//! `DateUTC`.
//!
//! # Dependencies
//!
//! `vaco-format-ebml` for the EBML layer; `vaco-demux-matroska` for the
//! Matroska element schema (D19); `vaco-format-core`, `vaco-io`, `vaco-core`,
//! `vaco-codec-core`, `vaco-packet`, `vaco-chlayout`, `vaco-limits` for the
//! rest of the container framework.

#![forbid(unsafe_code)]

pub mod block;
pub mod codec;
pub mod mux;
pub mod webm_chunk;

pub use mux::{MUXER_MATROSKA, MUXER_WEBM, MatroskaMuxer};
pub use webm_chunk::{MUXER_WEBM_CHUNK, WebmChunkMuxer};
