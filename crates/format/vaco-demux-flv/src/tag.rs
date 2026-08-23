//! The 11-byte FLV tag header, the back-pointer, and the codec-id tables for
//! both the legacy 4-bit codec fields and the Enhanced RTMP `FourCC` extension.
//!
//! Adobe *Video File Format Specification, Version 10.1*, plus the
//! community-maintained *Enhanced RTMP* specification (Veovera Software
//! Organization) for the `FourCC`-based codec signalling `ffmpeg 8.1` uses for
//! HEVC/AV1/VP9/Opus/FLAC.
//!
//! # What was measured, and why the Enhanced RTMP composition-time handling
//! is a known simplification
//!
//! `ffmpeg -c:v libsvtav1 -f flv` and `-c:v libx265 -f flv` were built and
//! walked byte-for-byte (`docs/format/vaco-demux-flv.md` has the trace). Both
//! confirmed the header shape below: a top bit marking the extended header,
//! then a `FrameType`/`PacketType` nibble pair, then a four-byte `FourCC`.
//!
//! What could **not** be confirmed byte-exactly: whether `PacketType::CodedFrames`
//! (`1`) carries a three-byte composition-time-offset field the way legacy
//! `AVCPacketType::Nalu` always does. The AV1 sample used it for inter frames
//! and the HEVC sample used `CodedFramesX` (`3`, specified as never carrying
//! one) for the equivalent tag, so neither sample disambiguates the case this
//! crate cannot yet tell apart from real bytes. [`ExPacketType::CodedFrames`]
//! is therefore treated identically to `CodedFramesX` — payload starts
//! immediately after the `FourCC`, composition time `0` — which is exactly
//! right for `CodedFramesX` and *may* include three leading bytes of
//! composition time in a `CodedFrames` payload from an encoder that uses it.
//! Legacy AVC (`CodecId::H264`, the common case, and the one most existing FLV
//! content uses) is unaffected — its composition time is unambiguous and
//! handled exactly.

use vaco_codec_core::CodecId;

/// Tag type byte values.
pub(crate) const TAG_AUDIO: u8 = 8;
pub(crate) const TAG_VIDEO: u8 = 9;
pub(crate) const TAG_SCRIPT: u8 = 18;

/// Bytes in a tag header, not counting the 4-byte back-pointer before it:
/// `TagType(1) + DataSize(3) + Timestamp(3) + TimestampExtended(1) + StreamID(3)`.
pub(crate) const TAG_HEADER_LEN: u64 = 11;
/// Bytes in the `PreviousTagSize` back-pointer preceding every tag.
pub(crate) const BACK_POINTER_LEN: u64 = 4;

/// `FrameType`'s `1` value — decoding may start here. Shared by the legacy and
/// Enhanced RTMP video headers, which both put it in the same nibble position.
pub(crate) const FRAME_TYPE_KEY: u8 = 1;

/// Legacy `VideoTagHeader` `CodecID` (the low nibble of the first byte, when
/// the Enhanced RTMP high bit is not set) -> [`CodecId`], where the shared
/// enum has a matching variant.
///
/// `2` (Sorenson H.263), `3`/`6` (Screen video, v1/v2), `4`/`5` (On2 VP6,
/// with/without alpha) have no [`CodecId`] variant today and resolve to
/// `None` — the same "no guessed near miss" rule `vaco-format-riff`'s tag
/// tables use.
#[must_use]
pub(crate) fn legacy_video_codec_id(codec: u8) -> Option<CodecId> {
    match codec {
        1 => Some(CodecId::Jpeg),
        7 => Some(CodecId::H264),
        _ => None,
    }
}

/// Legacy `AudioTagHeader` `SoundFormat` (the high nibble of the first byte)
/// -> [`CodecId`].
///
/// `1` (ADPCM), `4`/`5`/`6` (Nellymoser variants), `11` (Speex), `15`
/// (device-specific) have no variant and resolve to `None`. `7`/`8` (G.711
/// A-law/µ-law) resolve to [`CodecId::Pcm`], mirroring
/// `vaco_format_riff::wave_tags`' identical choice for `WAVE_FORMAT_ALAW`/
/// `WAVE_FORMAT_MULAW`.
#[must_use]
pub(crate) fn legacy_audio_codec_id(format: u8) -> Option<CodecId> {
    match format {
        0 | 3 | 7 | 8 => Some(CodecId::Pcm),
        2 | 14 => Some(CodecId::Mp3),
        10 => Some(CodecId::Aac),
        _ => None,
    }
}

/// Enhanced RTMP's `FourCC` codec identifiers -> [`CodecId`].
#[must_use]
pub(crate) fn fourcc_codec_id(fourcc: [u8; 4]) -> Option<CodecId> {
    match &fourcc {
        b"avc1" => Some(CodecId::H264),
        b"hvc1" => Some(CodecId::Hevc),
        b"av01" => Some(CodecId::Av1),
        b"vp09" => Some(CodecId::Vp9),
        b"Opus" => Some(CodecId::Opus),
        b"fLaC" => Some(CodecId::Flac),
        b".mp3" => Some(CodecId::Mp3),
        _ => None,
    }
}

/// Enhanced RTMP `VideoPacketType` (the low nibble of the extended header's
/// first byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExPacketType {
    /// Codec configuration data (an `AVCDecoderConfigurationRecord`-shaped
    /// blob, or the codec's equivalent) — becomes `extradata`.
    SequenceStart,
    /// A coded frame. See the module docs for the composition-time caveat.
    CodedFrames,
    /// No further frames on this codec/track.
    SequenceEnd,
    /// A coded frame with no composition-time field, ever.
    CodedFramesX,
    /// Codec-specific side metadata (HDR/colour info, channel mapping, …) —
    /// not decoded.
    Other,
}

impl ExPacketType {
    #[must_use]
    pub(crate) const fn from_nibble(n: u8) -> Self {
        match n {
            0 => Self::SequenceStart,
            1 => Self::CodedFrames,
            2 => Self::SequenceEnd,
            3 => Self::CodedFramesX,
            _ => Self::Other,
        }
    }
}

/// Read a big-endian signed 24-bit integer (the `CompositionTime` field).
#[must_use]
pub(crate) fn read_i24(b: [u8; 3]) -> i32 {
    let u = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
    // Sign-extend bit 23.
    if u & 0x0080_0000 != 0 {
        (u | 0xFF00_0000).cast_signed()
    } else {
        u.cast_signed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_tables_cover_the_measured_common_cases() {
        assert_eq!(legacy_video_codec_id(7), Some(CodecId::H264));
        assert_eq!(legacy_audio_codec_id(10), Some(CodecId::Aac));
        assert_eq!(legacy_audio_codec_id(2), Some(CodecId::Mp3));
    }

    #[test]
    fn unrepresented_legacy_codecs_are_none_not_a_guess() {
        assert_eq!(legacy_video_codec_id(4), None); // On2 VP6
        assert_eq!(legacy_audio_codec_id(1), None); // ADPCM
    }

    #[test]
    fn fourcc_table_covers_the_measured_enhanced_codecs() {
        assert_eq!(fourcc_codec_id(*b"av01"), Some(CodecId::Av1));
        assert_eq!(fourcc_codec_id(*b"hvc1"), Some(CodecId::Hevc));
        assert_eq!(fourcc_codec_id(*b"Opus"), Some(CodecId::Opus));
    }

    #[test]
    fn i24_sign_extends() {
        assert_eq!(read_i24([0x00, 0x00, 0x01]), 1);
        assert_eq!(read_i24([0xFF, 0xFF, 0xFF]), -1);
        assert_eq!(read_i24([0x80, 0x00, 0x00]), -8_388_608);
    }

    #[test]
    fn packet_type_maps_the_documented_nibbles() {
        assert_eq!(ExPacketType::from_nibble(0), ExPacketType::SequenceStart);
        assert_eq!(ExPacketType::from_nibble(1), ExPacketType::CodedFrames);
        assert_eq!(ExPacketType::from_nibble(2), ExPacketType::SequenceEnd);
        assert_eq!(ExPacketType::from_nibble(3), ExPacketType::CodedFramesX);
        assert_eq!(ExPacketType::from_nibble(9), ExPacketType::Other);
    }
}
