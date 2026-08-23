//! The FLV muxer: file header, `onMetaData`, and per-tag codec framing.
//!
//! Depends on `vaco-demux-flv` for one thing only — [`vaco_demux_flv::AmfValue`],
//! this format's AMF0 codec, reused rather than re-implemented (D19). This
//! crate's own tests also demux what they mux, with the same sibling crate.
//!
//! # What is supported
//!
//! Video: H.264 (legacy `AVCVIDEOPACKET` framing), HEVC/AV1/VP9 (Enhanced
//! RTMP, `CodedFramesX` — see `vaco-demux-flv::tag`'s module docs for why
//! that variant and not `CodedFrames`). Audio: AAC, MP3, PCM (legacy),
//! Opus/FLAC (Enhanced RTMP). One video and one audio stream at most — FLV's
//! `StreamID` field is always zero and multitrack is a further Enhanced RTMP
//! extension this crate does not implement.
//!
//! # What is deferred
//!
//! `onMetaData`'s `width`/`height`/`framerate` fields are not threaded
//! through from `CodecParameters` in this version — only `duration` (patched
//! at [`vaco_mux_flv::mux::FlvMuxer::write_trailer`] when the sink can seek)
//! and the codec-id fields are written. See `docs/format/vaco-mux-flv.md`.

#![forbid(unsafe_code)]

pub mod mux;

pub use mux::{FlvMuxer, MUXER};
