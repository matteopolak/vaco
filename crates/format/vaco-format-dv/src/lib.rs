//! DV: SMPTE 314M / IEC 61834.
//!
//! # This is not really a container
//!
//! Every other crate in `crates/format/` demultiplexes a byte stream that
//! carries its own structure — boxes, EBML elements, PES packets, RIFF
//! chunks. DV has none of that. A DV elementary stream is just a sequence
//! of fixed-size frames (120000 bytes at NTSC, 144000 at PAL — see
//! [`profile`]), and "demuxing" it is reading one frame at a time and
//! handing it to a caller as a packet. The interesting complexity DV has
//! instead is *inside* each frame: audio, subcode (timecode) and auxiliary
//! metadata are all interleaved into fixed byte ranges alongside the
//! compressed video, rather than living in a separate stream the way every
//! other format here works. See [`demux`] for exactly how far this crate
//! goes into that today.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`profile`] | frame-size/system detection from the `dsf` bit |
//! | [`demux`] | fixed-size frame reads, one video packet per frame |
//! | [`mux`] | the inverse: write whole frames back out |
//!
//! ```no_run
//! use vaco_format_dv::demux::DvDemuxer;
//! use vaco_format_core::Demuxer;
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("clip.dv").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = DvDemuxer::open(src)?;
//! for s in demux.streams() {
//!     println!("{:?} {:?}", s.index, s.params.media_type);
//! }
//! let pkt = demux.read_packet()?;
//! println!("frame {} bytes", pkt.len);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod demux;
pub mod mux;
pub mod profile;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, MuxerDesc, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

pub use demux::{DvDemuxer, FLAGS};
pub use mux::DvMuxer;

/// Content sniff: the first four bytes must decode as a DV Header block.
#[must_use]
pub fn probe(data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    let head = [
        data.get(0).unwrap_or(0),
        data.get(1).unwrap_or(0),
        data.get(2).unwrap_or(0),
        data.get(3).unwrap_or(0),
    ];
    if profile::DvProfile::detect(&head).is_some() {
        vaco_format_core::ProbeScore::CONTENT
    } else {
        vaco_format_core::ProbeScore::from_extension(data, &["dv", "dif"])
    }
}

/// The registry descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "dv",
    long_name: "DV (Digital Video)",
    extensions: &["dv", "dif"],
    mime_types: &["video/x-dv"],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

/// The registry descriptor for muxing.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "dv",
    long_name: "DV (Digital Video)",
    extensions: &["dv"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // DV carries no in-band codec configuration beyond the fixed frame
    // itself — the codec identity comes from the format, not from a parsed
    // header — so a `ParserProvider` has nothing to do here.
    Ok(Box::new(DvDemuxer::open(src)?))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open; DvMuxer::new cannot fail"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(DvMuxer::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demuxer_descriptor_answers_to_the_names_the_cli_uses() {
        assert!(DEMUXER.matches_name("dv"));
        assert!(DEMUXER.matches_extension("/tmp/x.dv"));
        assert!(DEMUXER.matches_extension("/tmp/x.dif"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_muxer_descriptor_answers_to_its_name() {
        assert!(MUXER.matches_name("dv"));
    }
}
