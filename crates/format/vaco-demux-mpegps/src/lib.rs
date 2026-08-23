//! The MPEG-PS demuxer: ISO/IEC 11172-1 (MPEG-1 systems) and
//! ISO/IEC 13818-1 §2.5 (MPEG-2 program stream).
//!
//! # What makes this container different from MPEG-TS
//!
//! Both are pack/PES-based MPEG systems layers, but a program stream is
//! meant to be read from a local, mostly-reliable medium (a disc, a file)
//! rather than broadcast, and that shows up everywhere:
//!
//! * **One program only.** There is no PAT/PMT; a system header lists every
//!   stream up front (see [`pack::SystemHeader`]).
//! * **A clock reference in every pack**, not a periodic PCR — SCR instead
//!   of PCR, at the same 90 kHz/33-bit width MPEG-TS uses.
//! * **`private_stream_1` carries everything ISO systems layers do not
//!   name**: AC-3, DTS, LPCM and DVD subpicture tracks all multiplex through
//!   `stream_id` `0xBD`, distinguished only by a one-byte sub-stream id at
//!   the front of the payload. See [`substream`].
//! * **Two incompatible PES envelopes.** `ffmpeg -f mpeg`/`-f vcd` write the
//!   older MPEG-1 PES syntax; `-f vob`/`-f svcd`/`-f dvd` write the MPEG-2
//!   one. See [`pes`].
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`probe`] | content detection |
//! | [`pack`] | pack headers and the system header, both syntaxes |
//! | [`pes`] | PES packet headers, both syntaxes |
//! | [`substream`] | `private_stream_1` sub-stream ids |
//! | [`keyframe`] | best-effort MPEG-1/2 `picture_coding_type` sniff |
//! | [`demux`] | framing, PES assembly, the SCR clock, seeking |
//!
//! # A note on `vaco-format-mpeg-common`
//!
//! Plan 18 §8.3 names a `vaco-format-mpeg-common` crate as the intended
//! single home for start-code scanning, PES header parse/serialise and the
//! 33-bit timestamp codec, shared by MPEG-TS and MPEG-PS. It does not exist.
//! This crate's [`pes`] module is written independently against the cited
//! specifications rather than by reaching into `vaco-demux-mpegts`'s
//! `pes` module or by creating the shared crate outside this brief's scope —
//! see the docs file for the full reasoning and what unifying them later
//! would take.
//!
//! ```no_run
//! use vaco_demux_mpegps::MpegPsDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("movie.vob").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = MpegPsDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
//! for s in demux.streams() {
//!     println!("{:?} {:?}", s.id, s.params.codec_id);
//! }
//! let pkt = demux.read_packet()?;
//! println!("{:?} {} bytes", pkt.pts, pkt.len);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod demux;
pub mod keyframe;
pub mod pack;
pub mod pes;
pub mod probe;
pub mod substream;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, FormatOptions, ParserProvider};
use vaco_io::MediaSource;

pub use demux::{FLAGS, MpegPsDemuxer};
pub use probe::probe;

/// The registry descriptor. One demuxer covers every program-stream
/// profile (`mpeg`, `vcd`, `vob`, `svcd`, `dvd`) — reading is symmetric
/// across them; only muxing needs the separate profiles in
/// `vaco-mux-mpegps`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpeg",
    long_name: "MPEG-PS (MPEG-2 Program Stream)",
    extensions: &["mpg", "mpeg", "m2p", "vob", "vcd"],
    mime_types: &["video/mpeg"],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(MpegPsDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_answers_to_the_names_the_cli_uses() {
        assert!(DEMUXER.matches_name("mpeg"));
        assert!(DEMUXER.matches_extension("/tmp/x.vob"));
        assert!(DEMUXER.matches_extension("/tmp/x.VOB"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_declared_flags_are_the_ones_the_models_depend_on() {
        use vaco_format_core::FormatFlags;
        assert!(FLAGS.contains(FormatFlags::SHOW_IDS));
        assert!(FLAGS.contains(FormatFlags::GENERIC_INDEX));
        assert!(!FLAGS.contains(FormatFlags::TS_DISCONT));
        assert!(FLAGS.allows_byte_seek());
    }
}
