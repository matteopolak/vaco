//! The generic verbatim muxer: 39 of this crate's 40 registrations write
//! nothing but the packet payloads, back to back, with no header and no
//! trailer at all.
//!
//! # Measured against ffmpeg 8.1
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -c:v libx264 -f h264 t.h264
//! $ xxd t.h264 | head -1        # starts with 00 00 00 01 67 ... — the encoder's
//!                                 own Annex-B bytes, nothing prepended
//! ```
//!
//! `write_header` and `write_trailer` do nothing observable (no bytes, no
//! seek-back) for every registration in this module — the reference's
//! `rawenc.c` is exactly `write_packet: avio_write(pb, pkt->data, pkt->size)`
//! and nothing else. `yuv4mpegpipe` (see [`crate::y4m`]) is the one
//! registration in this crate that is not this simple.
//!
//! Every registration accepts exactly **one** stream — a headerless dump has
//! nowhere to multiplex a second one — and [`RawMuxer::add_stream`] rejects a
//! second call with [`vaco_core::Error::Unsupported`].

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::mux::BitstreamAction;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_format_nalu::{LengthSize, convert::length_prefixed_to_annexb};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Whether `codec` is one of the NAL-unit-structured codecs this crate's raw
/// registrations require Annex B framing for.
///
/// `h264`/`hevc`/`vvc` are the only registrations here whose reference muxer
/// (`rawenc.c`'s `h264_write_packet`/`hevc_write_packet`) rewrites a
/// length-prefixed sample rather than writing it verbatim — matches
/// `vaco-mux-mpegts::is_h264_or_hevc`, which names the same three codecs for
/// the same reason.
const fn needs_annexb_framing(codec: CodecId) -> bool {
    matches!(codec, CodecId::H264 | CodecId::Hevc | CodecId::Vvc)
}

/// Whether `payload` already opens with an Annex B start code (`00 00 01` or
/// `00 00 00 01`) — see `vaco-mux-mpegts`'s identical helper for why this
/// makes [`RawMuxer::maybe_convert`] safe to call unconditionally.
fn starts_with_annexb_start_code(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1])
}

/// One verbatim registration.
#[derive(Debug, Clone, Copy)]
pub struct RawSpec {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub default_video: Option<CodecId>,
    pub default_audio: Option<CodecId>,
}

/// The verbatim muxer, parameterised at construction by [`RawSpec`].
#[derive(Debug)]
pub struct RawMuxer {
    out: IoWriter,
    has_stream: bool,
    /// `Some(n)` once [`RawMuxer::add_stream`] has seen an H.264/HEVC/VVC
    /// stream whose `CodecParameters` declared an `n`-byte NAL length
    /// prefix (`avcC`/`hvcC` style). A raw `h264`/`hevc`/`vvc` file has no
    /// out-of-band configuration record, so — same reasoning as
    /// `vaco-mux-mpegts::MuxStream::length_size` — such a stream must be
    /// rewritten to Annex B before it can go in the file at all.
    length_size: Option<LengthSize>,
    /// The codec that set `length_size`, so [`RawMuxer::check_bitstream`]
    /// knows which splice filter (if any) to ask for.
    codec_id: Option<CodecId>,
    /// Set the first time [`RawMuxer::check_bitstream`] is asked, mirroring
    /// `vaco-mux-mpegts::MuxStream::bsf_decided` — a raw muxer carries one
    /// stream, so one flag suffices.
    bsf_decided: bool,
    /// Bounds [`length_prefixed_to_annexb`]'s output allocation.
    convert_budget: Budget,
    /// The one video codec this registration's `RawSpec` names, if any —
    /// see [`RawMuxer::add_stream`]. `None` means the registration is
    /// codec-agnostic (`data`, `rawvideo`'s own codec check lives in its
    /// pixel-format instead), so any video codec is accepted.
    expected_video: Option<CodecId>,
    /// The one audio codec this registration's `RawSpec` names, if any —
    /// same reasoning as `expected_video`.
    expected_audio: Option<CodecId>,
}

impl RawMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(
        sink: Box<dyn MediaSink>,
        expected_video: Option<CodecId>,
        expected_audio: Option<CodecId>,
    ) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            has_stream: false,
            length_size: None,
            codec_id: None,
            bsf_decided: false,
            convert_budget: Budget::new(Limits::strict()),
            expected_video,
            expected_audio,
        })
    }

    /// Rewrite `payload` to Annex B if this muxer's one stream declared a
    /// length-prefixed framing at [`Muxer::add_stream`] time — the fallback
    /// a caller with no `BsfProvider` (or a VVC stream, which this crate has
    /// no splice filter for) still needs. Guarded by
    /// [`starts_with_annexb_start_code`] so it is a no-op once a real BSF
    /// (requested through [`RawMuxer::check_bitstream`]) has already run.
    fn maybe_convert(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let Some(length_size) = self.length_size else {
            return Ok(payload.to_vec());
        };
        if starts_with_annexb_start_code(payload) {
            return Ok(payload.to_vec());
        }
        let mut out = Vec::new();
        length_prefixed_to_annexb(payload, length_size, &mut out, &mut self.convert_budget)?;
        Ok(out)
    }
}

impl Muxer for RawMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.has_stream {
            return Err(Error::Unsupported(
                "a raw elementary-stream muxer carries exactly one stream",
            ));
        }
        // Measured on `vc1`: `ffmpeg -c copy -f vc1` on an H.264 source fails
        // with "vc1 muxer supports only codec vc1 for type video" rather than
        // writing the mismatched bytes — every registration whose `RawSpec`
        // names a specific codec refuses every other one of the same media
        // type the same way, not only `vc1`. A registration with no
        // declared codec for a media type (`data`'s `None`/`None`) accepts
        // anything of that type, matching the reference's own generic dump.
        let expected = match params.media_type {
            Some(MediaType::Video) => self.expected_video,
            Some(MediaType::Audio) => self.expected_audio,
            _ => None,
        };
        // `rawvideo` is the one registration this check must not apply to,
        // despite naming a `default_video`: measured directly, `ffmpeg -c
        // copy -f rawvideo` on an H.264 source is accepted (no "muxer
        // supports only codec" refusal, unlike every other registration
        // here that names one) — `default_video` states what a bare `-f
        // rawvideo` encodes *to* by default, not a hard restriction on
        // what `-c copy` may carry. `rawenc.c`'s generic dump has no
        // codec check at all for it, same as `data`.
        if let (Some(expected), Some(actual)) = (expected, params.codec_id)
            && expected != actual
            && expected != CodecId::Rawvideo
        {
            return Err(Error::Unsupported(
                "this raw muxer supports only its one declared codec",
            ));
        }
        self.has_stream = true;
        if let Some(codec_id) = params.codec_id
            && needs_annexb_framing(codec_id)
        {
            self.codec_id = Some(codec_id);
            self.length_size = params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .filter(|&n| n > 0)
                .and_then(LengthSize::new);
        }
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let converted = self.maybe_convert(packet.payload())?;
        self.out.write(&converted)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.out.flush()
    }

    /// Ask M6 for `h264_mp4toannexb`/`hevc_mp4toannexb` when the one stream
    /// declared length-prefixed framing — same condition
    /// [`RawMuxer::maybe_convert`] uses, and the same shape as
    /// `vaco-mux-mpegts::MpegTsMuxer::check_bitstream`. VVC is excluded, as
    /// there: this crate has no `vvc_mp4toannexb` to ask for, so a VVC
    /// stream keeps `maybe_convert`'s framing-only behaviour as its only
    /// conversion.
    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        _pkt: &Packet,
    ) -> Result<BitstreamAction> {
        if self.bsf_decided {
            return Ok(BitstreamAction::Keep);
        }
        self.bsf_decided = true;
        let asks_for_splice = matches!(params.codec_id, Some(CodecId::H264 | CodecId::Hevc))
            && params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .is_some_and(|n| n > 0);
        if !asks_for_splice {
            return Ok(BitstreamAction::Keep);
        }
        Ok(BitstreamAction::Insert {
            name: match params.codec_id {
                Some(CodecId::Hevc) => "hevc_mp4toannexb",
                _ => "h264_mp4toannexb",
            },
        })
    }
}

macro_rules! raw_reg {
    ($ident:ident, $name:literal, $long_name:literal, $exts:expr, $dv:expr, $da:expr) => {
        pub const $ident: MuxerDesc = MuxerDesc {
            name: $name,
            long_name: $long_name,
            extensions: $exts,
            default_video: $dv,
            default_audio: $da,
            open: |sink: Box<dyn MediaSink>| {
                Ok(Box::new(RawMuxer::new(sink, $dv, $da)?) as Box<dyn Muxer>)
            },
        };
    };
}

// ---------------------------------------------------------------------- PCM
//
// Every muxer below names the specific codec the reference names, measured
// one at a time:
//
//   $ ffmpeg -h muxer=s24le   ->  Default audio codec: pcm_s24le.
//   $ ffmpeg -h muxer=alaw    ->  Default audio codec: pcm_alaw.
//
// They all reported the generic `CodecId::Pcm` until 2026-08-23 — a codec the
// reference does not have, and the single invented name in our whole `-codecs`
// listing. Six of the twenty needed a `CodecId` variant that did not exist
// (`pcm_u16le` and its five siblings); those were added rather than left
// generic, because "the enum cannot say it" had stopped being true for the
// other fourteen and would only have hidden the remaining six.

raw_reg!(
    MUXER_ALAW,
    "alaw",
    "PCM A-law",
    &["al"],
    None,
    Some(CodecId::PcmAlaw)
);
raw_reg!(
    MUXER_MULAW,
    "mulaw",
    "PCM mu-law",
    &["ul"],
    None,
    Some(CodecId::PcmMulaw)
);
raw_reg!(
    MUXER_F32BE,
    "f32be",
    "PCM 32-bit floating-point big-endian",
    &[],
    None,
    Some(CodecId::PcmF32be)
);
raw_reg!(
    MUXER_F32LE,
    "f32le",
    "PCM 32-bit floating-point little-endian",
    &[],
    None,
    Some(CodecId::PcmF32le)
);
raw_reg!(
    MUXER_F64BE,
    "f64be",
    "PCM 64-bit floating-point big-endian",
    &[],
    None,
    Some(CodecId::PcmF64be)
);
raw_reg!(
    MUXER_F64LE,
    "f64le",
    "PCM 64-bit floating-point little-endian",
    &[],
    None,
    Some(CodecId::PcmF64le)
);
raw_reg!(
    MUXER_S16BE,
    "s16be",
    "PCM signed 16-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmS16be)
);
raw_reg!(
    MUXER_S16LE,
    "s16le",
    "PCM signed 16-bit little-endian",
    &["sw"],
    None,
    Some(CodecId::PcmS16le)
);
raw_reg!(
    MUXER_S24BE,
    "s24be",
    "PCM signed 24-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmS24be)
);
raw_reg!(
    MUXER_S24LE,
    "s24le",
    "PCM signed 24-bit little-endian",
    &[],
    None,
    Some(CodecId::PcmS24le)
);
raw_reg!(
    MUXER_S32BE,
    "s32be",
    "PCM signed 32-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmS32be)
);
raw_reg!(
    MUXER_S32LE,
    "s32le",
    "PCM signed 32-bit little-endian",
    &[],
    None,
    Some(CodecId::PcmS32le)
);
raw_reg!(
    MUXER_S8,
    "s8",
    "PCM signed 8-bit",
    &["sb"],
    None,
    Some(CodecId::PcmS8)
);
raw_reg!(
    MUXER_U16BE,
    "u16be",
    "PCM unsigned 16-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmU16be)
);
raw_reg!(
    MUXER_U16LE,
    "u16le",
    "PCM unsigned 16-bit little-endian",
    &["uw"],
    None,
    Some(CodecId::PcmU16le)
);
raw_reg!(
    MUXER_U24BE,
    "u24be",
    "PCM unsigned 24-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmU24be)
);
raw_reg!(
    MUXER_U24LE,
    "u24le",
    "PCM unsigned 24-bit little-endian",
    &[],
    None,
    Some(CodecId::PcmU24le)
);
raw_reg!(
    MUXER_U32BE,
    "u32be",
    "PCM unsigned 32-bit big-endian",
    &[],
    None,
    Some(CodecId::PcmU32be)
);
raw_reg!(
    MUXER_U32LE,
    "u32le",
    "PCM unsigned 32-bit little-endian",
    &[],
    None,
    Some(CodecId::PcmU32le)
);
raw_reg!(
    MUXER_U8,
    "u8",
    "PCM unsigned 8-bit",
    &["ub"],
    None,
    Some(CodecId::PcmU8)
);
raw_reg!(
    MUXER_VIDC,
    "vidc",
    "PCM Archimedes VIDC",
    &[],
    None,
    Some(CodecId::PcmVidc)
);

// ----------------------------------------------------------------- raw video

raw_reg!(
    MUXER_RAWVIDEO,
    "rawvideo",
    "raw video",
    &["yuv", "rgb"],
    Some(CodecId::Rawvideo),
    None
);

// ----------------------------------------------------------------- bitstream
//
// Long names, extensions and default-codec names below are the muxer's own,
// captured separately from the demuxer's — they are not always the same
// string. Measured differences worth flagging explicitly, because a future
// editor pattern-matching from `vaco-demux-raw` would otherwise "fix" them
// back to a wrong shared value:
//
// | Name | Demux long_name | Mux long_name |
// |---|---|---|
// | `avs3` | `raw AVS3-P2/IEEE1857.10` | `AVS3-P2/IEEE1857.10` (no `raw` prefix) |
// | `cavsvideo` | `raw Chinese AVS (Audio Video Standard)` | `raw Chinese AVS (Audio Video Standard) video` |
// | `evc` | `EVC Annex B` | `raw EVC video` |
// | `vc1` | `raw VC-1` | `raw VC-1 video` |
//
// Extensions also diverge per-format (`h264` demux accepts `h26l`/`avc` too;
// mux only ever writes `.h264`/`.264`; `cavsvideo` demuxes `.avs` but muxes
// `.cavs`; `dirac`/`dnxhd`/`h263` have mux extensions and no demux ones).
// `CodecId` has no variant for most of these codecs (see the crate docs), so
// `default_video` is `None` except where a real variant exists.

raw_reg!(
    MUXER_AVS2,
    "avs2",
    "raw AVS2-P2/IEEE1857.4 video",
    &["avs", "avs2"],
    Some(CodecId::Avs2),
    None
);
raw_reg!(
    MUXER_AVS3,
    "avs3",
    "AVS3-P2/IEEE1857.10",
    &["avs3"],
    Some(CodecId::Avs3),
    None
);
raw_reg!(
    MUXER_BIT,
    "bit",
    "G.729 BIT file format",
    &["bit"],
    None,
    // Measured: `ffmpeg -h muxer=bit` -> `Default audio codec: g729.`
    Some(CodecId::G729)
);
raw_reg!(
    MUXER_CAVSVIDEO,
    "cavsvideo",
    "raw Chinese AVS (Audio Video Standard) video",
    &["cavs"],
    Some(CodecId::Cavs),
    None
);
raw_reg!(MUXER_DATA, "data", "raw data", &[], None, None);
raw_reg!(
    MUXER_DIRAC,
    "dirac",
    "raw Dirac",
    &["drc", "vc2"],
    Some(CodecId::Dirac),
    None
);
raw_reg!(
    MUXER_DNXHD,
    "dnxhd",
    "raw DNxHD (SMPTE VC-3)",
    &["dnxhd", "dnxhr"],
    Some(CodecId::Dnxhd),
    None
);
raw_reg!(MUXER_EVC, "evc", "raw EVC video", &["evc"], Some(CodecId::Evc), None);
raw_reg!(MUXER_H261, "h261", "raw H.261", &["h261"], Some(CodecId::H261), None);
raw_reg!(MUXER_H263, "h263", "raw H.263", &["h263"], Some(CodecId::H263), None);
raw_reg!(
    MUXER_H264,
    "h264",
    "raw H.264 video",
    &["h264", "264"],
    Some(CodecId::H264),
    None
);
raw_reg!(
    MUXER_HEVC,
    "hevc",
    "raw HEVC video",
    &["hevc", "h265", "265"],
    Some(CodecId::Hevc),
    None
);
raw_reg!(MUXER_M4V, "m4v", "raw MPEG-4 video", &["m4v"], Some(CodecId::Mpeg4), None);
raw_reg!(
    MUXER_MJPEG,
    "mjpeg",
    "raw MJPEG video",
    &["mjpg", "mjpeg"],
    Some(CodecId::Jpeg),
    None
);
raw_reg!(
    MUXER_OBU,
    "obu",
    "AV1 low overhead OBU",
    &["obu"],
    Some(CodecId::Av1),
    None
);
raw_reg!(MUXER_VC1, "vc1", "raw VC-1 video", &["vc1"], Some(CodecId::Vc1), None);
raw_reg!(
    MUXER_VVC,
    "vvc",
    "raw H.266/VVC video",
    &["vvc", "h266", "266"],
    // Measured: `ffmpeg -h muxer=vvc` -> `vvc`, not the `h264` this carried.
    Some(CodecId::Vvc),
    None
);

/// The 39 verbatim registrations (everything but `yuv4mpegpipe`), in
/// `ffmpeg -muxers` family order.
pub const RAW_MUXERS: &[&MuxerDesc] = &[
    &MUXER_ALAW,
    &MUXER_MULAW,
    &MUXER_F32BE,
    &MUXER_F32LE,
    &MUXER_F64BE,
    &MUXER_F64LE,
    &MUXER_S16BE,
    &MUXER_S16LE,
    &MUXER_S24BE,
    &MUXER_S24LE,
    &MUXER_S32BE,
    &MUXER_S32LE,
    &MUXER_S8,
    &MUXER_U16BE,
    &MUXER_U16LE,
    &MUXER_U24BE,
    &MUXER_U24LE,
    &MUXER_U32BE,
    &MUXER_U32LE,
    &MUXER_U8,
    &MUXER_VIDC,
    &MUXER_RAWVIDEO,
    &MUXER_AVS2,
    &MUXER_AVS3,
    &MUXER_BIT,
    &MUXER_CAVSVIDEO,
    &MUXER_DATA,
    &MUXER_DIRAC,
    &MUXER_DNXHD,
    &MUXER_EVC,
    &MUXER_H261,
    &MUXER_H263,
    &MUXER_H264,
    &MUXER_HEVC,
    &MUXER_M4V,
    &MUXER_MJPEG,
    &MUXER_OBU,
    &MUXER_VC1,
    &MUXER_VVC,
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::vacoraw::MemorySink;

    #[test]
    fn there_are_thirty_nine_verbatim_registrations() {
        assert_eq!(RAW_MUXERS.len(), 39);
    }

    #[test]
    fn a_second_stream_is_rejected() {
        let sink = Box::new(MemorySink::new());
        let mut m = RawMuxer::new(sink, None, None).unwrap();
        assert!(m.add_stream(&CodecParameters::video()).is_ok());
        assert!(m.add_stream(&CodecParameters::video()).is_err());
    }

    #[test]
    fn a_mismatched_codec_is_rejected() {
        // Measured: `ffmpeg -c copy -f vc1` on an H.264 source fails with
        // "vc1 muxer supports only codec vc1 for type video" — a registration
        // that names a specific codec refuses every other one.
        let sink = Box::new(MemorySink::new());
        let mut m = RawMuxer::new(sink, Some(CodecId::Vc1), None).unwrap();
        let mut wrong = CodecParameters::video();
        wrong.codec_id = Some(CodecId::H264);
        assert!(m.add_stream(&wrong).is_err());
        assert!(!m.has_stream);
        let mut right = CodecParameters::video();
        right.codec_id = Some(CodecId::Vc1);
        assert!(m.add_stream(&right).is_ok());
    }

    #[test]
    fn a_codec_agnostic_registration_accepts_anything() {
        // `data`'s `RawSpec` declares neither a video nor an audio codec, so
        // it takes whatever `-c copy` hands it — matching the reference's
        // own generic dump.
        let sink = Box::new(MemorySink::new());
        let mut m = RawMuxer::new(sink, None, None).unwrap();
        let mut params = CodecParameters::video();
        params.codec_id = Some(CodecId::H264);
        assert!(m.add_stream(&params).is_ok());
    }

    #[test]
    fn rawvideo_accepts_any_video_codec_despite_naming_one() {
        // Measured: `ffmpeg -c copy -f rawvideo` on an H.264 source is
        // accepted, unlike `vc1`/`h264`/every other single-codec
        // registration — see the doc comment on the check this exempts.
        let sink = Box::new(MemorySink::new());
        let mut m = RawMuxer::new(sink, Some(CodecId::Rawvideo), None).unwrap();
        let mut params = CodecParameters::video();
        params.codec_id = Some(CodecId::H264);
        assert!(m.add_stream(&params).is_ok());
    }

    #[test]
    fn packets_are_written_back_to_back_with_no_header_or_trailer() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = RawMuxer::new(Box::new(sink), None, None).unwrap();
        m.add_stream(&CodecParameters::video()).unwrap();
        m.write_header().unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let p0 = Packet::from_slice(&mut budget, b"AAAA").unwrap();
        let p1 = Packet::from_slice(&mut budget, b"BB").unwrap();
        m.write_packet(&p0).unwrap();
        m.write_packet(&p1).unwrap();
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"AAAABB");
    }
}
