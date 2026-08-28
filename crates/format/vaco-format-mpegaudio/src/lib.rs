//! MPEG-1/2/2.5 audio (Layer I/II/III) frame headers and the container-level
//! side information carried in the first frame: Xing/Info, VBRI and the LAME
//! extension tag.
//!
//! This crate parses **framing**, not audio. `vaco-demux-mpegaudio` and
//! `vaco-mux-mpegaudio` use it to find frame boundaries and report duration;
//! `vaco-codec-mpegaudio` uses the same [`MpegAudioHeader`] to configure decode.
//! Splitting it out once, rather than duplicating the bit-rate/sample-rate
//! tables in both crates, is the point.

#![forbid(unsafe_code)]

pub mod header;
pub mod lame;
pub mod vbri;
pub mod xing;

pub use header::{ChannelMode, Emphasis, Layer, MpegAudioHeader, Version, version_for_sample_rate};
pub use lame::LameTag;
pub use vbri::VbriHeader;
pub use xing::XingHeader;
