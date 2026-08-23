//! `stream_type` and registration-identifier resolution.
//!
//! ISO/IEC 13818-1 Table 2-34 assigns a codec to about forty of the 256
//! `stream_type` values. The rest are either reserved, or — far more
//! importantly — say only *how* the elementary stream is carried and leave
//! *what it is* to a descriptor. `0x06`, "PES packets containing private
//! data", is by a wide margin the most common value in real DVB and ATSC
//! multiplexes, and on its own it means nothing at all.
//!
//! So resolution is two-stage: the table gives a first answer, and the
//! descriptor loop overrides it. [`resolve`] is that, and it is the only entry
//! point a demuxer needs.
//!
//! # Why this crate has its own codec enum
//!
//! [`vaco_codec_core::CodecId`] has grown since this was written (it now
//! names AC-3, E-AC-3, DTS and VC-1 among others), but MPEG-TS still routinely
//! carries things with no `CodecId` at all: DVB subtitles, teletext, SCTE-35,
//! timed ID3. Collapsing them all onto "unknown" would lose the one fact the
//! PMT actually stated, so [`TsCodec`] carries the full repertoire and
//! [`TsCodec::codec_id`] maps across where a `CodecId` exists. When the enum
//! grows, only that one function changes.
//!
//! # The mux direction
//!
//! [`resolve`] and [`from_stream_type`] answer "what codec does this
//! `stream_type` mean" for a demuxer. [`for_codec`] answers the reverse
//! question a muxer asks — "what `stream_type` (and, in a private range,
//! what `registration_descriptor`) does this codec need" — and lives here
//! rather than being re-derived in `vaco-mux-mpegts`, per the one-definition-
//! per-concept rule (D19): a demuxer and a muxer disagreeing about what
//! `0x81` means is exactly the kind of drift a shared table is for.

use vaco_codec_core::CodecId;
use vaco_core::MediaType;

use crate::descriptor::{
    DescriptorIter, TAG_ATSC_AC3, TAG_ATSC_EAC3, TAG_DVB_AAC, TAG_DVB_AC3, TAG_DVB_DTS,
    TAG_DVB_EAC3, TAG_SUBTITLING, TAG_TELETEXT, TAG_VBI_TELETEXT,
};

/// What a transport stream can carry, named as `vaco-probe` prints it.
///
/// The names are interface facts (D9) and are reproduced; the enum shape is
/// ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TsCodec {
    Unknown,
    // --- video
    Mpeg1Video,
    Mpeg2Video,
    Mpeg4Video,
    H264,
    Hevc,
    Vvc,
    Av1,
    Vc1,
    Dirac,
    Cavs,
    Avs2,
    Avs3,
    Jpeg2000,
    // --- audio
    Mp1,
    Mp2,
    Mp3,
    Aac,
    AacLatm,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Opus,
    /// SMPTE 302M PCM, carried under registration `BSSD`.
    Pcm302m,
    /// Blu-ray LPCM.
    PcmBluray,
    // --- subtitle
    DvbSubtitle,
    DvbTeletext,
    /// Blu-ray presentation graphics.
    PgsSubtitle,
    // --- data
    /// SCTE-35 splice information, exposed but not interpreted.
    Scte35,
    /// Timed ID3 metadata, as HLS uses.
    TimedId3,
    /// SMPTE 336M KLV metadata.
    Klv,
    /// A stream the PMT declares and we can only report as private data.
    PrivateData,
}

impl TsCodec {
    /// The short name `vaco-probe` prints as `codec_name`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Mpeg1Video => "mpeg1video",
            Self::Mpeg2Video => "mpeg2video",
            Self::Mpeg4Video => "mpeg4",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Vvc => "vvc",
            Self::Av1 => "av1",
            Self::Vc1 => "vc1",
            Self::Dirac => "dirac",
            Self::Cavs => "cavs",
            Self::Avs2 => "avs2",
            Self::Avs3 => "avs3",
            Self::Jpeg2000 => "jpeg2000",
            Self::Mp1 => "mp1",
            Self::Mp2 => "mp2",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::AacLatm => "aac_latm",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Dts => "dts",
            Self::TrueHd => "truehd",
            Self::Opus => "opus",
            Self::Pcm302m => "pcm_s16be",
            Self::PcmBluray => "pcm_bluray",
            Self::DvbSubtitle => "dvb_subtitle",
            Self::DvbTeletext => "dvb_teletext",
            Self::PgsSubtitle => "hdmv_pgs_subtitle",
            Self::Scte35 => "scte_35",
            Self::TimedId3 => "timed_id3",
            Self::Klv => "klv",
            Self::PrivateData => "bin_data",
        }
    }

    /// Which stream list this codec belongs in.
    #[must_use]
    pub const fn media_type(self) -> MediaType {
        match self {
            Self::Mpeg1Video
            | Self::Mpeg2Video
            | Self::Mpeg4Video
            | Self::H264
            | Self::Hevc
            | Self::Vvc
            | Self::Av1
            | Self::Vc1
            | Self::Dirac
            | Self::Cavs
            | Self::Avs2
            | Self::Avs3
            | Self::Jpeg2000 => MediaType::Video,
            Self::Mp1
            | Self::Mp2
            | Self::Mp3
            | Self::Aac
            | Self::AacLatm
            | Self::Ac3
            | Self::Eac3
            | Self::Dts
            | Self::TrueHd
            | Self::Opus
            | Self::Pcm302m
            | Self::PcmBluray => MediaType::Audio,
            Self::DvbSubtitle | Self::DvbTeletext | Self::PgsSubtitle => MediaType::Subtitle,
            Self::Unknown | Self::Scte35 | Self::TimedId3 | Self::Klv | Self::PrivateData => {
                MediaType::Data
            }
        }
    }

    /// The workspace [`CodecId`], where one exists yet.
    ///
    /// `None` is not "unknown codec": it is "this codec is real, the PMT named
    /// it, and `vaco-codec-core` has no variant for it". A demuxer must keep
    /// reporting the stream — with its media type, its PID and its language —
    /// rather than dropping it.
    #[must_use]
    pub const fn codec_id(self) -> Option<CodecId> {
        match self {
            Self::H264 => Some(CodecId::H264),
            Self::Hevc => Some(CodecId::Hevc),
            Self::Av1 => Some(CodecId::Av1),
            Self::Aac => Some(CodecId::Aac),
            Self::AacLatm => Some(CodecId::AacLatm),
            Self::Opus => Some(CodecId::Opus),
            Self::Mp3 => Some(CodecId::Mp3),
            Self::Pcm302m | Self::PcmBluray => Some(CodecId::Pcm),
            _ => None,
        }
    }

    /// Whether a PES packet on this stream begins with a start code that a
    /// keyframe test can look at.
    ///
    /// Used to decide whether the adaptation field's `random_access_indicator`
    /// is the only key-frame evidence available.
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self.media_type(), MediaType::Video)
    }
}

/// A resolved elementary stream declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub codec: TsCodec,
    /// What `vaco-probe` prints as `codec_tag`.
    ///
    /// **Measured, not assumed**: for a stream carrying a registration
    /// descriptor the reference reports the four-character identifier
    /// (`ffprobe` shows `0x332d4341` for AC-3, which is `"AC-3"`); otherwise
    /// it reports the `stream_type` in the low byte (`0x001b` for H.264).
    pub codec_tag: [u8; 4],
    /// The raw `stream_type`, kept because two different codecs can share one
    /// and a diagnostic wants to say which value it saw.
    pub stream_type: u8,
}

/// The Table 2-34 answer, before descriptors are consulted.
#[must_use]
pub const fn from_stream_type(stream_type: u8) -> TsCodec {
    match stream_type {
        0x01 => TsCodec::Mpeg1Video,
        // Both MPEG-1 and MPEG-2 video are muxed as 0x02 in practice; the
        // elementary stream distinguishes them, so a parser refines this.
        0x02 | 0x80 => TsCodec::Mpeg2Video,
        // Layer is a property of the frames, not of the PMT. `mp2` is the
        // starting point because it is overwhelmingly what layer-II-capable
        // broadcast multiplexes carry; a parser corrects it to mp1 or mp3.
        0x03 | 0x04 => TsCodec::Mp2,
        0x0F | 0x1C => TsCodec::Aac,
        0x10 => TsCodec::Mpeg4Video,
        0x11 => TsCodec::AacLatm,
        0x15 => TsCodec::TimedId3,
        0x1B => TsCodec::H264,
        0x21 => TsCodec::Jpeg2000,
        0x24 | 0x25 => TsCodec::Hevc,
        0x33 => TsCodec::Vvc,
        0x42 => TsCodec::Cavs,
        0x81 => TsCodec::Ac3,
        0x83 => TsCodec::TrueHd,
        0x84 | 0x87 | 0xA1 => TsCodec::Eac3,
        0x82 | 0x85 | 0x86 | 0x8A | 0xA2 => TsCodec::Dts,
        0x90 => TsCodec::PgsSubtitle,
        0xD1 => TsCodec::Dirac,
        0xD2 => TsCodec::Avs2,
        0xD3 => TsCodec::Avs3,
        0xEA => TsCodec::Vc1,
        // 0x05 private sections and 0x06 private PES data say nothing at all;
        // a descriptor has to.
        _ => TsCodec::Unknown,
    }
}

/// The codec a four-character registration identifier declares.
#[must_use]
pub fn from_registration(id: [u8; 4]) -> Option<TsCodec> {
    Some(match &id {
        b"AC-3" | b"ac-3" => TsCodec::Ac3,
        b"EAC3" | b"eac3" | b"EC-3" => TsCodec::Eac3,
        b"AV01" => TsCodec::Av1,
        b"HEVC" => TsCodec::Hevc,
        b"VC-1" => TsCodec::Vc1,
        b"Opus" => TsCodec::Opus,
        b"drac" => TsCodec::Dirac,
        b"DTS1" | b"DTS2" | b"DTS3" | b"dtsh" => TsCodec::Dts,
        b"BSSD" => TsCodec::Pcm302m,
        b"KLVA" => TsCodec::Klv,
        b"ID3 " => TsCodec::TimedId3,
        b"CUEI" => TsCodec::Scte35,
        b"AVSV" | b"CAVS" => TsCodec::Cavs,
        b"AVS2" => TsCodec::Avs2,
        b"AVS3" => TsCodec::Avs3,
        b"VVC1" | b"vvc1" => TsCodec::Vvc,
        b"mp4a" => TsCodec::Aac,
        // `HDMV` and `GA94` say which *private range* applies, not which
        // codec; `from_stream_type` already reads the private range the way
        // both of them define it.
        _ => return None,
    })
}

/// Resolve one PMT entry to a codec.
///
/// Order, and it matters: the `stream_type` table first, then the registration
/// descriptor, then the DVB codec descriptors, then the subtitle descriptors.
/// Each later stage only fires where it has something to say, so a stream whose
/// `stream_type` is unambiguous is never second-guessed by a stray descriptor —
/// except for the two cases where a descriptor is *definitionally* stronger:
/// `teletext` and `subtitling` on a private-data stream.
#[must_use]
pub fn resolve(stream_type: u8, descriptors: &[u8]) -> Resolved {
    let mut codec = from_stream_type(stream_type);
    let iter = DescriptorIter::new(descriptors);
    let registration = iter.registration();

    if let Some(id) = registration
        && let Some(from_reg) = from_registration(id)
        && (codec == TsCodec::Unknown || is_private_range(stream_type))
    {
        codec = from_reg;
    }

    if codec == TsCodec::Unknown || stream_type == 0x06 || stream_type == 0x05 {
        // EN 300 468 §6.2: a private-data stream is identified by which DVB
        // descriptor accompanies it.
        for d in DescriptorIter::new(descriptors) {
            codec = match d.tag {
                TAG_DVB_AC3 | TAG_ATSC_AC3 => TsCodec::Ac3,
                TAG_DVB_EAC3 | TAG_ATSC_EAC3 => TsCodec::Eac3,
                TAG_DVB_DTS => TsCodec::Dts,
                TAG_DVB_AAC => TsCodec::Aac,
                TAG_TELETEXT | TAG_VBI_TELETEXT => TsCodec::DvbTeletext,
                TAG_SUBTITLING => TsCodec::DvbSubtitle,
                _ => continue,
            };
            break;
        }
    }

    if codec == TsCodec::Unknown && (stream_type == 0x05 || stream_type == 0x06) {
        codec = TsCodec::PrivateData;
    }

    let codec_tag = match registration {
        Some(id) => id,
        None => [stream_type, 0, 0, 0],
    };

    Resolved {
        codec,
        codec_tag,
        stream_type,
    }
}

/// Whether `stream_type` sits in a range whose meaning depends on which
/// organisation's private assignment applies.
const fn is_private_range(stream_type: u8) -> bool {
    stream_type >= 0x80
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_table_covers_the_common_broadcast_types() {
        assert_eq!(from_stream_type(0x02), TsCodec::Mpeg2Video);
        assert_eq!(from_stream_type(0x03), TsCodec::Mp2);
        assert_eq!(from_stream_type(0x0F), TsCodec::Aac);
        assert_eq!(from_stream_type(0x1B), TsCodec::H264);
        assert_eq!(from_stream_type(0x24), TsCodec::Hevc);
        assert_eq!(from_stream_type(0x81), TsCodec::Ac3);
        assert_eq!(from_stream_type(0x06), TsCodec::Unknown);
    }

    #[test]
    fn a_private_stream_becomes_ac3_through_its_registration() {
        // stream_type 0x06 plus registration "AC-3".
        let desc = [0x05u8, 4, b'A', b'C', b'-', b'3'];
        let r = resolve(0x06, &desc);
        assert_eq!(r.codec, TsCodec::Ac3);
        // Measured against ffprobe 8.1: the reported codec_tag is the
        // registration identifier, not the stream type.
        assert_eq!(r.codec_tag, *b"AC-3");
    }

    #[test]
    fn h264_reports_its_stream_type_as_the_tag() {
        let r = resolve(0x1B, &[]);
        assert_eq!(r.codec, TsCodec::H264);
        assert_eq!(r.codec_tag, [0x1B, 0, 0, 0]);
    }

    #[test]
    fn a_registration_never_overrides_an_unambiguous_stream_type() {
        // A stray `Opus` registration on an H.264 stream must not win.
        let desc = [0x05u8, 4, b'O', b'p', b'u', b's'];
        assert_eq!(resolve(0x1B, &desc).codec, TsCodec::H264);
    }

    #[test]
    fn the_private_range_defers_to_the_registration() {
        // 0x87 is E-AC-3 in the ATSC range, but a `HDMV` multiplex using the
        // same value with a DTS registration must resolve to DTS.
        let desc = [0x05u8, 4, b'D', b'T', b'S', b'1'];
        assert_eq!(resolve(0x87, &desc).codec, TsCodec::Dts);
        assert_eq!(resolve(0x87, &[]).codec, TsCodec::Eac3);
    }

    #[test]
    fn dvb_descriptors_identify_private_streams() {
        assert_eq!(resolve(0x06, &[0x6A, 0]).codec, TsCodec::Ac3);
        assert_eq!(resolve(0x06, &[0x7A, 0]).codec, TsCodec::Eac3);
        assert_eq!(resolve(0x06, &[0x7B, 0]).codec, TsCodec::Dts);
        assert_eq!(
            resolve(0x06, &[0x56, 5, b'e', b'n', b'g', 0x11, 0x00]).codec,
            TsCodec::DvbTeletext
        );
        assert_eq!(
            resolve(0x06, &[0x59, 8, b'e', b'n', b'g', 0x20, 0, 1, 0, 2]).codec,
            TsCodec::DvbSubtitle
        );
    }

    #[test]
    fn an_unidentifiable_private_stream_is_still_a_stream() {
        let r = resolve(0x06, &[]);
        assert_eq!(r.codec, TsCodec::PrivateData);
        assert_eq!(r.codec.media_type(), MediaType::Data);
        assert_eq!(r.codec.codec_id(), None);
    }

    #[test]
    fn a_reserved_stream_type_resolves_to_unknown_not_to_a_guess() {
        assert_eq!(resolve(0x00, &[]).codec, TsCodec::Unknown);
        assert_eq!(resolve(0x77, &[]).codec, TsCodec::Unknown);
    }

    #[test]
    fn media_types_are_assigned_to_every_variant() {
        // Every codec must land in a stream list, or a demuxer cannot report
        // it at all.
        for c in [
            TsCodec::Unknown,
            TsCodec::Mpeg2Video,
            TsCodec::Ac3,
            TsCodec::DvbSubtitle,
            TsCodec::Scte35,
            TsCodec::Klv,
        ] {
            let _ = c.media_type();
            assert!(!c.name().is_empty());
        }
    }

    #[test]
    fn codec_ids_agree_with_media_types_where_they_exist() {
        for c in [
            TsCodec::H264,
            TsCodec::Hevc,
            TsCodec::Av1,
            TsCodec::Aac,
            TsCodec::AacLatm,
            TsCodec::Opus,
            TsCodec::Mp3,
        ] {
            let id = c.codec_id().unwrap();
            assert_eq!(id.media_type(), c.media_type(), "{}", c.name());
            assert_eq!(id.name(), c.name(), "{}", c.name());
        }
    }

    #[test]
    fn every_stream_type_resolves_without_panicking() {
        for t in 0..=255u8 {
            let r = resolve(t, &[0x05, 4, b'X', b'Y', b'Z', b'W']);
            let _ = r.codec.media_type();
            let _ = r.codec.codec_id();
        }
    }
}
