//! The `mp3` muxer.
//!
//! Nearly a pass-through: every packet is already a complete, self-delimited
//! MPEG audio frame (this is what a demuxer reading the same format hands
//! out), so writing one is a byte copy. The only thing this crate adds is the
//! empty `ID3v2` header the reference always opens a stream with.

#![forbid(unsafe_code)]

mod mux;

use vaco_format_core::MuxerDesc;
use vaco_io::MediaSink;

pub use mux::MpegAudioMuxer;

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "mp3",
    long_name: "MP3 (MPEG audio layer 3)",
    extensions: &["mp3"],
    default_video: None,
    default_audio: Some(vaco_codec_core::CodecId::Mp3),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> vaco_core::Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(MpegAudioMuxer::new(sink)?))
}
