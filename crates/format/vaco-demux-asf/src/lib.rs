//! The ASF (Advanced Systems Format) demuxer: `.asf`/`.wmv`/`.wma`.
//!
//! # Source
//!
//! Microsoft, *"Advanced Systems Format (ASF) Specification"*, Revision
//! 01.20.06. Clean-room from that document (D7/D15) plus black-box probing
//! of the installed `ffmpeg 8.1` binary's own output (D6/D17) — every place
//! that draws on a probe rather than the spec text says so in its own doc
//! comment.
//!
//! # What makes this container different
//!
//! ASF is a **fixed-size-packet** format: the File Properties Object states
//! one packet length up front, and every Data Packet in the file is exactly
//! that many bytes. A media object bigger than a packet is split into
//! several *fragments*; several small objects are packed behind one
//! multiple-payload header. Getting that framing right — [`packet`]'s job —
//! is most of what makes this demuxer non-trivial; the header objects
//! ([`header`]) are the easy half.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`header`] | File Properties, Stream Properties, Content Description, DRM detection |
//! | [`packet`] | Data Packet / payload-parsing-information decoding (all four payload shapes) |
//! | [`index`] | Simple Index Object and top-level Index Object |
//! | [`demux`] | [`demux::AsfDemuxer`]: ties the above together, fragment reassembly, seeking |
//!
//! # DRM
//!
//! Detected, never decrypted — see [`header`]'s module docs for exactly what
//! this crate does when a Content Encryption Object is present.
//!
//! ```no_run
//! use vaco_demux_asf::AsfDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("clip.asf").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = AsfDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
//! for s in demux.streams() {
//!     println!("{:?} {:?}", s.media_type(), s.params.codec_id);
//! }
//! let pkt = demux.read_packet()?;
//! println!("{:?} {} bytes", pkt.pts, pkt.len);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod demux;
pub mod header;
pub mod index;
pub mod packet;

pub use demux::{AsfDemuxer, DEMUXER, DEMUXER_O, FLAGS, probe, probe_opaque};
pub use header::Encryption;
