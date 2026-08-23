//! `null`: discards every packet.
//!
//! # Why this is more load-bearing than its size suggests
//!
//! `-f null -` is the workhorse of nearly every test that exercises this
//! project's demux/decode/mux spine without wanting a real file on disk.
//! `vaco-cli` already carries a local, hand-rolled `NullMuxer`
//! (`crates/app/vaco-cli/src/nullmux.rs`) because at the time it was written
//! `crates/format/` had zero `vaco-mux-*` crates and `vaco -f null -` would
//! otherwise have had nothing to run at all. This module is the real,
//! registered version — see the crate docs for whether the CLI's local copy
//! is now redundant.
//!
//! # Measured against the reference
//!
//! `ffmpeg -h muxer=null` (ffmpeg 8.1, `LC_ALL=C`):
//!
//! ```text
//! Muxer null [raw null video]:
//!     Default video codec: wrapped_avframe.
//!     Default audio codec: pcm_s16le.
//! ```
//!
//! `pcm_s16le` has a [`CodecId`] in this workspace and is reproduced exactly.
//! `wrapped_avframe` is the reference's pseudo-codec for "an undecoded
//! `AVFrame` handed straight to a muxer with no real encoder" — it has no
//! bitstream, no extradata, and no [`CodecId`] variant here, so
//! [`MUXER_NULL::default_video`] is `None` rather than a guess. This is a
//! deliberate difference from `vaco-cli`'s local copy, which declares `None`
//! for *both* fields on the reasoning that "this build has no encoders" — a
//! true fact about the whole workspace at the time it was written, but this
//! crate's job is to describe what the container itself wants, which for
//! audio is exactly representable.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// A [`Muxer`] that discards every packet.
///
/// No state beyond a stream counter: unlike `vaco-cli`'s local copy, this
/// type keeps no tally, because a caller that wants counted-and-discarded
/// output can wrap this muxer (or the sink it is given) itself. Keeping this
/// type minimal is what makes it the obvious thing to register — a tallying
/// version is a different, CLI-shaped concern layered on top, not a property
/// of the container format.
#[derive(Debug, Default)]
pub struct NullSinkMuxer {
    stream_count: u32,
}

impl NullSinkMuxer {
    #[must_use]
    pub const fn new() -> Self {
        Self { stream_count: 0 }
    }
}

impl Muxer for NullSinkMuxer {
    fn flags(&self) -> FormatFlags {
        FLAGS
    }

    fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
        let index = self.stream_count;
        self.stream_count += 1;
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, _packet: &Packet) -> Result<()> {
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        Ok(())
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        // No opinion, matching the reasoning in `vaco-cli`'s local copy: a
        // sink with nowhere to put a timescale should not invent one.
        None
    }
}

/// The container flags `-f null` is declared with.
///
/// Mirrors `vaco-cli`'s local `nullmux::FLAGS` exactly (same reasoning: the
/// most permissive set [`vaco_format_core::mux`] currently accepts —
/// `NOTIMESTAMPS` clears both timestamp fields and the M18 interleave path
/// accepts that, so a sink that discards bytes is not forced through the
/// strictest DTS discipline in the framework). This crate could not
/// independently re-derive the reference's own internal `AVFMT_*` bits — there
/// is no CLI surface that prints them — so the set below is an architectural
/// judgement call carried over from the existing implementation rather than a
/// fresh probe.
pub const FLAGS: FormatFlags = FormatFlags::NOFILE
    .union(FormatFlags::VARIABLE_FPS)
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::TS_NEGATIVE)
    .union(FormatFlags::NOTIMESTAMPS);

/// The descriptor `-f null` resolves to.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is MuxerDesc::open's, which every container shares"
)]
fn open_null(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(NullSinkMuxer::new()))
}

/// `null`: discards everything. See the module docs for why `default_video`
/// is `None` where the reference's own default (`wrapped_avframe`) has no
/// [`CodecId`] here, while `default_audio` matches the reference exactly.
pub static MUXER_NULL: MuxerDesc = MuxerDesc {
    name: "null",
    long_name: "raw null video",
    extensions: &[],
    default_video: None,
    default_audio: Some(CodecId::PcmS16le),
    open: open_null,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::MediaType;
    use vaco_limits::{Budget, Limits};

    fn params(media: MediaType) -> CodecParameters {
        CodecParameters::new(media)
    }

    fn packet(len: usize) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        Packet::from_slice(&mut budget, &vec![0u8; len]).unwrap_or_else(|_| Packet::empty())
    }

    #[test]
    fn add_stream_indices_increment_from_zero() {
        let mut m = NullSinkMuxer::new();
        assert_eq!(m.add_stream(&params(MediaType::Video)).unwrap(), 0);
        assert_eq!(m.add_stream(&params(MediaType::Audio)).unwrap(), 1);
    }

    #[test]
    fn every_call_succeeds_and_writes_nothing_observable() {
        let mut m = NullSinkMuxer::new();
        m.add_stream(&params(MediaType::Video)).unwrap();
        m.write_header().unwrap();
        let mut p = packet(128);
        p.stream_index = 0;
        m.write_packet(&p).unwrap();
        // An out-of-range stream index must not panic (indexing_slicing is
        // denied workspace-wide precisely so this is checked, not assumed).
        p.stream_index = 99;
        m.write_packet(&p).unwrap();
        m.write_trailer().unwrap();
    }

    #[test]
    fn flags_relax_strict_dts_and_accept_missing_timestamps() {
        assert!(!FLAGS.requires_strict_dts());
        assert!(FLAGS.contains(FormatFlags::NOTIMESTAMPS));
        assert!(FLAGS.contains(FormatFlags::NOFILE));
    }

    #[test]
    fn descriptor_matches_reference_defaults_where_representable() {
        assert!(MUXER_NULL.matches_name("null"));
        assert_eq!(MUXER_NULL.default_codec(MediaType::Video), None);
        assert_eq!(
            MUXER_NULL.default_codec(MediaType::Audio),
            Some(CodecId::PcmS16le)
        );
    }

    #[test]
    fn opens_from_the_registry_descriptor() {
        use vaco_format_core::vacoraw::MemorySink;
        let sink = Box::new(MemorySink::new());
        assert!((MUXER_NULL.open)(sink).is_ok());
    }
}
