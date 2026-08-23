//! The FLV demuxer: Adobe's Flash Video container, plus the Enhanced RTMP
//! (E-RTMP) codec-signalling extension for HEVC/AV1/VP9/Opus/FLAC.
//!
//! # What makes this container different
//!
//! FLV was designed for progressive download and live streaming, so it ships
//! **no header stream list**: an 11-byte tag header repeats for the life of
//! the file, and the first audio/video tag *is* the stream declaration. See
//! [`demux`]'s module docs for how stream discovery works without one.
//!
//! * Timestamps are trivial by container standards — every tag states its own
//!   millisecond timestamp directly, just laid out in an unusual byte order.
//! * The codec identity comes from a 4-bit field in legacy tags, or a `FourCC`
//!   in Enhanced RTMP ones; [`tag`] has both tables, measured against
//!   `ffmpeg 8.1`.
//! * `onMetaData` (an AMF0 script tag) supplies `width`/`height`/`duration`
//!   ahead of the media tags that would otherwise have to state them, so it
//!   is cached and applied when a stream is created — [`amf`] is this crate's
//!   own AMF0 codec, reused verbatim by `vaco-mux-flv` (D19).
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`amf`] | AMF0 decode/encode: `AmfValue` |
//! | [`tag`] | the 11-byte tag header, the back-pointer, both codec-id tables |
//! | [`demux`] | the tag walk, `onMetaData`, packet emission, seeking |
//!
//! ```no_run
//! use vaco_demux_flv::FlvDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("clip.flv").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = FlvDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
//! let pkt = demux.read_packet()?;
//! println!("stream {} {:?} {} bytes", pkt.stream_index, pkt.pts, pkt.len);
//! for s in demux.streams() {
//!     println!("{:?} {:?}", s.media_type(), s.params.codec_id);
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod amf;
pub mod demux;
pub mod tag;

pub use amf::AmfValue;
pub use demux::{DEMUXER, FLAGS, FlvDemuxer, probe};
