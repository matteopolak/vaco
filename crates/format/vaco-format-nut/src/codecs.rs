//! `fourcc` <-> [`CodecId`], for the two codec pairs this crate has actually
//! measured against real `ffmpeg -f nut` output (video: MPEG-4 -> `"FMP4"`;
//! audio: MP3 -> the 4-byte little-endian encoding of WAV format tag
//! `0x0055`). NUT's own text says video `FourCC`s reuse AVI's; measured here
//! is that ffmpeg's NUT muxer encodes an audio codec the same way AVI's
//! `WAVEFORMATEX.wFormatTag` would, zero-extended to 4 bytes rather than
//! kept at 2 — so a 2-byte value that data alone would not disambiguate
//! from a genuine 2-byte video-style `FourCC`.
//!
//! Other codecs are deliberately not guessed at: H.264 (`"H264"`) and PCM
//! (WAV tag `0x0001`) are included because they are unambiguous, standard,
//! and already used the same way elsewhere in this workspace
//! (`vaco-format-riff`'s AVI `FourCC` table), not because they were measured
//! against a real NUT file the way MPEG-4/MP3 were.

use vaco_codec_core::CodecId;

/// `fourcc` -> [`CodecId`] for video streams (`stream_class == video`).
#[must_use]
pub fn video_codec_from_fourcc(fourcc: &[u8]) -> Option<CodecId> {
    match fourcc {
        b"FMP4" | b"XVID" | b"DIVX" | b"DX50" | b"mp4v" => Some(CodecId::Mpeg4),
        b"H264" | b"X264" | b"avc1" => Some(CodecId::H264),
        b"MJPG" => Some(CodecId::Jpeg),
        _ => None,
    }
}

/// The inverse of [`video_codec_from_fourcc`], for muxing. Measured
/// (`FMP4`) where noted above; `H264` is the spec-mandated AVI spelling for
/// an unambiguous standard, not a guess.
#[must_use]
pub fn video_fourcc_for_codec(codec: CodecId) -> Option<&'static [u8]> {
    match codec {
        CodecId::Mpeg4 => Some(b"FMP4"),
        CodecId::H264 => Some(b"H264"),
        CodecId::Jpeg => Some(b"MJPG"),
        _ => None,
    }
}

/// The WAV `wFormatTag` values this crate recognises when zero-extended
/// into a 4-byte `fourcc`, as ffmpeg's NUT muxer measurably does for MP3
/// (tag `0x0055`).
const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_MP3: u16 = 0x0055;

/// `fourcc` -> [`CodecId`] for audio streams (`stream_class == audio`).
#[must_use]
pub fn audio_codec_from_fourcc(fourcc: &[u8]) -> Option<CodecId> {
    let tag = match *fourcc {
        [a, b, 0, 0] | [a, b] => u16::from_le_bytes([a, b]),
        _ => return None,
    };
    match tag {
        WAVE_FORMAT_PCM => Some(CodecId::PcmS16le),
        WAVE_FORMAT_MP3 => Some(CodecId::Mp3),
        _ => None,
    }
}

/// The inverse of [`audio_codec_from_fourcc`]. Always writes the full
/// 4-byte form, matching the measured MP3 sample rather than the 2-byte
/// form the spec's general `FourCC` text would also permit.
#[must_use]
pub fn audio_fourcc_for_codec(codec: CodecId) -> Option<[u8; 4]> {
    let tag = match codec {
        CodecId::PcmS16le => WAVE_FORMAT_PCM,
        CodecId::Mp3 => WAVE_FORMAT_MP3,
        _ => return None,
    };
    let [lo, hi] = tag.to_le_bytes();
    Some([lo, hi, 0, 0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_video_fourcc_maps_both_ways() {
        assert_eq!(video_codec_from_fourcc(b"FMP4"), Some(CodecId::Mpeg4));
        assert_eq!(video_fourcc_for_codec(CodecId::Mpeg4), Some(&b"FMP4"[..]));
    }

    #[test]
    fn the_measured_audio_fourcc_maps_both_ways() {
        assert_eq!(
            audio_codec_from_fourcc(&[0x55, 0x00, 0x00, 0x00]),
            Some(CodecId::Mp3)
        );
        assert_eq!(
            audio_fourcc_for_codec(CodecId::Mp3),
            Some([0x55, 0x00, 0x00, 0x00])
        );
    }

    #[test]
    fn an_unrecognised_fourcc_is_none_not_a_guess() {
        assert_eq!(video_codec_from_fourcc(b"ZZZZ"), None);
        assert_eq!(audio_codec_from_fourcc(&[0xFF, 0xFF, 0, 0]), None);
    }
}
