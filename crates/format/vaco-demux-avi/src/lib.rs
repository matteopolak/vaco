//! The AVI demuxer: Microsoft RIFF/AVI, plus the `OpenDML` AVI 2.0 extension.
//!
//! # What makes this container different
//!
//! AVI is a RIFF file (D19: `vaco-format-riff` owns the chunk grammar and the
//! `BITMAPINFOHEADER`/`WAVEFORMATEX` structures this crate's `strf` reads —
//! this crate is the `hdrl`/`strl`/`movi` walk on top of it, the way
//! `vaco-demux-mp4` is the box walk on top of `vaco-format-isom`).
//!
//! * **No per-packet timestamp.** A chunk's timestamp is a running count
//!   since the start of its own stream, derived from `strh.dwSampleSize` —
//!   see [`demux`]'s module docs for the exact arithmetic.
//! * **A notoriously ambiguous index.** `idx1`'s `dwOffset` is relative to
//!   the `movi` list in some files and to the start of the file in others,
//!   and both conventions are real. [`index::detect_offset_base`] resolves it
//!   by probing, not by assuming — see that module's docs for what was
//!   measured against `ffmpeg 8.1`'s own writer.
//! * **A second, hierarchical index** (`OpenDML` `indx`/`ix##`) for files over
//!   2 GiB, parsed and structurally supported but not exercised against a
//!   real multi-gigabyte file — see `docs/format/vaco-demux-avi.md`.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`hdrl`] | `avih`, `strh`, `strf`, `strn` — the header walk |
//! | [`index`] | `idx1`, `OpenDML` `indx`/`ix##`, and the offset-ambiguity probe |
//! | [`demux`] | the `movi` walk, the clock, and seeking |
//!
//! ```no_run
//! use vaco_demux_avi::AviDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("clip.avi").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = AviDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
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
pub mod hdrl;
pub mod index;

pub use demux::{AviDemuxer, DEMUXER, FLAGS, probe};
