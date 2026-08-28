//! GXF (General eXchange Format, SMPTE 360M/360-2009) demuxer.
//!
//! Layer 4. See `docs/format/vaco-format-gxf.md` for the full account;
//! this comment covers only the module map.
//!
//! Written from the published SMPTE 360-2009 standard (a stable/archived
//! document SMPTE itself distributes at no charge —
//! `https://pub.smpte.org/pub/st360/st0360-2009_stable2016.pdf`, confirmed
//! genuinely public and not a leak or a mirror of copyrighted
//! implementation source), clean-room (D7/D15), cross-checked against a
//! real file `ffmpeg -f gxf` writes on this machine (D6/D17) — this
//! machine's `ffmpeg 8.1` has both a `gxf` demuxer and a `gxf` muxer
//! (`ffmpeg -demuxers`/`-muxers`, confirmed not assumed), unlike this
//! project's IMF work, so both directions of this crate have a real
//! differential bar to measure against.

#![forbid(unsafe_code)]

pub mod demux;
pub mod map;
pub mod media;
pub mod mux;
pub mod packet;

pub use demux::{DEMUXER, GxfDemuxer};
pub use mux::{GxfMuxer, MUXER};
