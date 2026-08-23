//! The MPEG-PS muxer family: `mpeg`, `vcd`, `vob`, `svcd`, `dvd`.
//!
//! Five registry entries sharing one implementation ([`mux::PsMuxer`]),
//! differing only in [`mux::MuxProfile`]: which PES envelope (MPEG-1 or
//! MPEG-2), which pack-header syntax, and whether packs are padded to a
//! fixed size.
//!
//! # `vaco_codec_core::CodecId` gap
//!
//! Surveyed 2026-08-23: there is no `CodecId` variant for MPEG-1/2 video,
//! MPEG audio (layer I/II), AC-3, DTS or DVD-flavoured LPCM — which is most
//! of what these five containers actually carry. [`Muxer::add_stream`]
//! still works (it only needs [`vaco_codec_core::CodecParameters::media_type`]
//! to route a stream to a video or audio `stream_id` range), but there is no
//! standard way for a caller to ask for a `private_stream_1` substream
//! (AC-3/DTS/LPCM/subpicture) until those codec ids exist. This crate
//! exposes its own placeholder convention — a `codec_tag` of `b"AC-3"`,
//! `b"DTS "`, `b"LPCM"` or `b"dvsp"` — tested directly in `mux.rs`, and
//! meant to be replaced by real `CodecId` matching once it lands (see the
//! docs file).
//!
//! ```no_run
//! use vaco_codec_core::CodecParameters;
//! use vaco_core::MediaType;
//! use vaco_format_core::Muxer;
//! use vaco_io::DynBuf;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let sink = Box::new(DynBuf::new());
//! let mut mux = (vaco_mux_mpegps::MUXER_VOB.open)(sink)?;
//! let idx = mux.add_stream(&CodecParameters::new(MediaType::Video))?;
//! mux.write_header()?;
//! // mux.write_packet(&packet)?; // one PES + pack per call
//! mux.write_trailer()?;
//! # let _ = idx;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod mux;
pub mod pack;
pub mod pes;

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;

use mux::{PROFILE_DVD, PROFILE_MPEG, PROFILE_SVCD, PROFILE_VCD, PROFILE_VOB, PsMuxer};

// `MuxerDesc::open`'s signature is frozen at `fn(Box<dyn MediaSink>) ->
// Result<Box<dyn Muxer>>`; `PsMuxer::new` cannot fail, so each of these
// always returns `Ok`, which clippy would otherwise flag.
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open, not a choice here"
)]
fn open_mpeg(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(PsMuxer::new(sink, &PROFILE_MPEG)))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open"
)]
fn open_vcd(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(PsMuxer::new(sink, &PROFILE_VCD)))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open"
)]
fn open_vob(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(PsMuxer::new(sink, &PROFILE_VOB)))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open"
)]
fn open_svcd(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(PsMuxer::new(sink, &PROFILE_SVCD)))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open"
)]
fn open_dvd(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(PsMuxer::new(sink, &PROFILE_DVD)))
}

/// `ffmpeg -f mpeg`: MPEG-1 Systems / MPEG program stream.
///
/// `default_video`/`default_audio` are `None`: no `CodecId` exists yet for
/// MPEG-1 video or MPEG audio layer II, the codecs a real `mpeg` file
/// carries (see the crate docs).
pub const MUXER_MPEG: MuxerDesc = MuxerDesc {
    name: "mpeg",
    long_name: "MPEG-1 Systems / MPEG program stream",
    extensions: &["mpg", "mpeg"],
    default_video: None::<CodecId>,
    default_audio: None::<CodecId>,
    open: open_mpeg,
};

/// `ffmpeg -f vcd`: MPEG-1 Systems / MPEG program stream (VCD profile).
pub const MUXER_VCD: MuxerDesc = MuxerDesc {
    name: "vcd",
    long_name: "MPEG-1 Systems / MPEG program stream (VCD)",
    extensions: &["dat"],
    default_video: None::<CodecId>,
    default_audio: None::<CodecId>,
    open: open_vcd,
};

/// `ffmpeg -f vob`: MPEG-2 PS (VOB).
pub const MUXER_VOB: MuxerDesc = MuxerDesc {
    name: "vob",
    long_name: "MPEG-2 PS (VOB)",
    extensions: &["vob"],
    default_video: None::<CodecId>,
    default_audio: None::<CodecId>,
    open: open_vob,
};

/// `ffmpeg -f svcd`: MPEG-2 PS (SVCD).
pub const MUXER_SVCD: MuxerDesc = MuxerDesc {
    name: "svcd",
    long_name: "MPEG-2 PS (SVCD)",
    extensions: &["mpg"],
    default_video: None::<CodecId>,
    default_audio: None::<CodecId>,
    open: open_svcd,
};

/// `ffmpeg -f dvd`: MPEG-2 PS (DVD VOB).
pub const MUXER_DVD: MuxerDesc = MuxerDesc {
    name: "dvd",
    long_name: "MPEG-2 PS (DVD VOB)",
    extensions: &["vob"],
    default_video: None::<CodecId>,
    default_audio: None::<CodecId>,
    open: open_dvd,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_descriptors_answer_to_their_own_name() {
        assert!(MUXER_MPEG.matches_name("mpeg"));
        assert!(MUXER_VCD.matches_name("vcd"));
        assert!(MUXER_VOB.matches_name("vob"));
        assert!(MUXER_SVCD.matches_name("svcd"));
        assert!(MUXER_DVD.matches_name("dvd"));
    }
}
