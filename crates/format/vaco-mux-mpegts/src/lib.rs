//! The MPEG-TS muxer (ISO/IEC 13818-1): PAT/PMT/SDT, PES packetisation, PCR
//! insertion, per-PID continuity counters, and Blu-ray M2TS wrapping.
//!
//! FM-25, issue #576.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`tsw`] | the 188-byte transport packet, adaptation field, PCR, continuity counters, M2TS wrapping |
//! | [`pes`] | PES header encoding and the 33-bit PTS/DTS field |
//! | [`options`] | `-mpegts_*` flags and defaults, measured against `ffmpeg -h muxer=mpegts` |
//! | [`mux`] | [`mux::MpegTsMuxer`], the [`vaco_format_core::Muxer`] implementation that ties the above together |
//!
//! PAT/PMT/SDT section *writers* live in `vaco-format-mpegts-tables`
//! (`write_pat`/`write_pmt`/`write_sdt`), alongside that crate's existing
//! PSI readers — one definition of what a PAT looks like, shared by both
//! directions (D19), rather than a second model of the same three tables
//! built inside this crate.
//!
//! ```
//! use vaco_codec_core::{CodecId, CodecParameters};
//! use vaco_core::{MediaType, Timestamp};
//! use vaco_format_core::Muxer;
//! use vaco_io::DynBuf;
//! use vaco_limits::{Budget, Limits};
//! use vaco_packet::Packet;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let sink = Box::new(DynBuf::new());
//! let mut mux = (vaco_mux_mpegts::MUXER.open)(sink)?;
//! let params = CodecParameters {
//!     media_type: Some(MediaType::Video),
//!     codec_id: Some(CodecId::Mpeg2video),
//!     ..CodecParameters::new(MediaType::Video)
//! };
//! let video = mux.add_stream(&params)?;
//! mux.init()?;
//! mux.write_header()?;
//! let mut budget = Budget::new(Limits::permissive());
//! let mut pkt = Packet::from_slice(&mut budget, &[0u8; 32])?;
//! pkt.stream_index = video;
//! pkt.pts = Timestamp::new(0);
//! pkt.dts = Timestamp::new(0);
//! mux.write_packet(&pkt)?;
//! mux.write_trailer()?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod mux;
pub mod options;
pub mod pes;
pub mod tsw;

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::MediaSink;

use mux::MpegTsMuxer;

/// The registry descriptor for `-f mpegts`.
///
/// `m2ts`/`mpegtsraw` are **not** separate registrations here: measured
/// (`ffmpeg -muxers | grep -i ts`), the reference has exactly one MPEG-TS
/// muxer; M2TS is a *mode* of it (`-mpegts_m2ts_mode`, also auto-detected
/// from a `.m2ts` output extension), not a second format name. A caller
/// wanting M2TS output or any other non-default option constructs
/// [`MpegTsMuxer::with_options`] directly — this descriptor's `open` always
/// builds the plain-TS, default-options form, the same convention
/// `vaco-mux-mp4`'s `MovMuxer::with_options` and this workspace's other
/// options-bearing muxers use.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "mpegts",
    long_name: "MPEG-TS (MPEG-2 Transport Stream)",
    extensions: &["ts", "m2t", "m2ts", "mts"],
    // Measured: `ffmpeg -h muxer=mpegts` prints `Default video codec:
    // mpeg2video.` and `Default audio codec: mp2.`.
    default_video: Some(CodecId::Mpeg2video),
    default_audio: Some(CodecId::Mp2),
    open: open_muxer,
};

// `MuxerDesc::open`'s signature is frozen at `fn(Box<dyn MediaSink>) ->
// Result<Box<dyn Muxer>>`; `MpegTsMuxer::new` cannot fail, so this always
// returns `Ok`, which clippy would otherwise flag — the same situation
// `vaco-mux-mpegps`'s five `open_*` functions are in.
#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open, not a choice here"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(MpegTsMuxer::new(sink)) as Box<dyn Muxer>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_answers_to_the_name_the_cli_uses() {
        assert!(MUXER.matches_name("mpegts"));
        assert!(MUXER.extensions.contains(&"ts"));
        assert!(MUXER.extensions.contains(&"m2ts"));
        assert!(!MUXER.extensions.contains(&"mp4"));
    }

    #[test]
    fn the_default_codecs_match_the_measured_reference() {
        assert_eq!(MUXER.default_video, Some(CodecId::Mpeg2video));
        assert_eq!(MUXER.default_audio, Some(CodecId::Mp2));
    }

    #[test]
    fn the_descriptor_opens() {
        use vaco_io::DynBuf;
        let sink = Box::new(DynBuf::new());
        assert!((MUXER.open)(sink).is_ok());
    }
}
