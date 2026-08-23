//! `private_stream_1` sub-stream demultiplexing.
//!
//! MPEG program streams have exactly one elementary-stream slot per
//! `stream_id` (`0xC0`–`0xDF` for audio, `0xE0`–`0xEF` for video), which is
//! not enough for a DVD carrying AC-3, DTS, LPCM and subpicture tracks
//! alongside MPEG audio. The DVD-Video and SVCD specifications solve this by
//! routing all of them through the single private-stream-1 `stream_id`
//! (`0xBD`) and prefixing the PES payload with a one-byte sub-stream id that
//! is not part of either ISO MPEG systems standard — this is the "genuinely
//! fiddly part" plan 18 §3.4.1 calls out, and it is where `vobsub` comes
//! from.
//!
//! # Provenance
//!
//! The sub-stream id ranges below are the DVD-Video and SVCD authoring
//! convention, not an ISO/IEC 13818-1 table: they are dictated by how every
//! DVD authoring tool and every player allocates the byte, which is a fact
//! about the format rather than an expressive choice, and are reproduced
//! from public technical references to the DVD-Video specification (not
//! from any `FFmpeg` source, per this project's clean-room policy). Measured
//! against `ffmpeg -f vob` output carrying an AC-3 track (2026-08-23): the
//! payload's first byte for that track is `0x80`, at the low end of the
//! documented AC-3 range.
use vaco_core::MediaType;

/// What kind of payload a `private_stream_1` sub-stream id names, and its
/// [`MediaType`].
///
/// `codec_id` is deliberately not resolved here: `vaco_codec_core::CodecId`
/// has no AC-3, DTS or DVD-flavoured LPCM/subpicture variant yet (surveyed
/// 2026-08-23 — see this crate's docs file). Reporting [`MediaType`] without
/// a codec id is the same choice `vaco-demux-matroska` already made for
/// codecs its own `CodecID` table cannot name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstreamKind {
    /// `0x80..=0x87`: AC-3 (Dolby Digital).
    Ac3,
    /// `0x88..=0x8F`: DTS.
    Dts,
    /// `0x98..=0x9F`: SDDS.
    Sdds,
    /// `0xA0..=0xA7`: LPCM, DVD framing (a 3-byte header the generic `Pcm`
    /// codec does not describe: sample rate, bit depth and mute flag are
    /// container-declared here rather than self-describing).
    Lpcm,
    /// `0x20..=0x3F`: DVD subpicture (run-length bitmap subtitles).
    Subpicture,
}

impl SubstreamKind {
    #[must_use]
    pub const fn media_type(self) -> MediaType {
        match self {
            Self::Ac3 | Self::Dts | Self::Sdds | Self::Lpcm => MediaType::Audio,
            Self::Subpicture => MediaType::Subtitle,
        }
    }

    /// Short, stable name for metadata/diagnostics — not `codec_name`, since
    /// no [`vaco_codec_core::CodecId`] exists to hold one yet.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ac3 => "ac3",
            Self::Dts => "dts",
            Self::Sdds => "sdds",
            Self::Lpcm => "lpcm_dvd",
            Self::Subpicture => "dvd_subpicture",
        }
    }
}

/// Classify a `private_stream_1` sub-stream id (the payload's first byte).
#[must_use]
pub const fn classify(sub_id: u8) -> Option<SubstreamKind> {
    match sub_id {
        0x20..=0x3F => Some(SubstreamKind::Subpicture),
        0x80..=0x87 => Some(SubstreamKind::Ac3),
        0x88..=0x8F => Some(SubstreamKind::Dts),
        0x98..=0x9F => Some(SubstreamKind::Sdds),
        0xA0..=0xA7 => Some(SubstreamKind::Lpcm),
        _ => None,
    }
}

/// The DVD LPCM 3-byte sub-header that follows the sub-stream id byte
/// (`emphasis`/`mute`/`reserved`/`frame_number`, then `quant`/`freq`, then
/// `channels`). Decoded fields only cover what a demuxer needs to declare
/// [`vaco_codec_core::AudioParameters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcmHeader {
    /// 16, 20 or 24.
    pub bits_per_sample: u8,
    /// 48000 or 96000 (44100/88200 are reserved in the DVD profile but
    /// decoded the same way; a player is expected to reject them).
    pub sample_rate: u32,
    pub channels: u8,
}

impl LpcmHeader {
    /// Parse the 3-byte LPCM sub-header that follows the sub-stream id.
    #[must_use]
    pub fn parse(b: &[u8]) -> Option<Self> {
        let b1 = *b.get(1)?;
        let quant = (b1 >> 6) & 0x03;
        let freq = (b1 >> 4) & 0x03;
        let channels = (b1 & 0x07) + 1;
        let bits_per_sample = match quant {
            0 => 16,
            1 => 20,
            _ => 24,
        };
        let sample_rate = if freq & 0x01 == 0 { 48_000 } else { 44_100 };
        let sample_rate = if freq & 0x02 != 0 {
            sample_rate * 2
        } else {
            sample_rate
        };
        Some(Self {
            bits_per_sample,
            sample_rate,
            channels,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn ac3_range_classifies() {
        assert_eq!(classify(0x80), Some(SubstreamKind::Ac3));
        assert_eq!(classify(0x87), Some(SubstreamKind::Ac3));
        assert_eq!(classify(0x88), Some(SubstreamKind::Dts));
    }

    #[test]
    fn subpicture_range_classifies() {
        assert_eq!(classify(0x20), Some(SubstreamKind::Subpicture));
        assert_eq!(classify(0x3F), Some(SubstreamKind::Subpicture));
        assert_eq!(classify(0x40), None);
    }

    #[test]
    fn lpcm_range_classifies_and_media_type_is_audio() {
        let k = classify(0xA0).unwrap();
        assert_eq!(k, SubstreamKind::Lpcm);
        assert_eq!(k.media_type(), MediaType::Audio);
    }

    #[test]
    fn subpicture_media_type_is_subtitle() {
        assert_eq!(classify(0x25).unwrap().media_type(), MediaType::Subtitle);
    }

    #[test]
    fn an_unrecognised_sub_id_is_none() {
        assert_eq!(classify(0x00), None);
        assert_eq!(classify(0x98), Some(SubstreamKind::Sdds));
        assert_eq!(classify(0xA8), None);
    }

    #[test]
    fn lpcm_header_default_shape_is_16_bit_48k_stereo() {
        let h = LpcmHeader::parse(&[0x00, 0b0000_0001, 0x80]).unwrap();
        assert_eq!(h.bits_per_sample, 16);
        assert_eq!(h.sample_rate, 48_000);
        assert_eq!(h.channels, 2);
    }
}
