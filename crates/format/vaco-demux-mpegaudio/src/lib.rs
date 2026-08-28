//! The `mp3` demuxer: MPEG-1/2/2.5 Layer I/II/III elementary streams,
//! `ID3v2`/`ID3v1` tags, and the Xing/Info/VBRI VBR headers.

#![forbid(unsafe_code)]

mod demux;
mod probe;

use vaco_format_core::{DemuxerDesc, FormatFlags, ParserProvider};
use vaco_io::MediaSource;

pub use demux::MpegAudioDemuxer;

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mp3",
    long_name: "MP2/3 (MPEG audio layer 2/3)",
    extensions: &["mp3", "mp2", "m2a", "mpa"],
    mime_types: &["audio/mpeg", "audio/x-mpeg", "audio/mp3"],
    flags: FormatFlags::empty(),
    probe: probe::probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> vaco_core::Result<Box<dyn vaco_format_core::Demuxer>> {
    Ok(Box::new(MpegAudioDemuxer::open(
        src,
        &vaco_format_core::FormatOptions::default(),
    )?))
}
