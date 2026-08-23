//! [`CodecId`] to Matroska `CodecID`, and the `webm` codec allow-list.
//!
//! # Provenance
//!
//! The exact-string table is the write-side mirror of
//! `vaco_demux_matroska::codec::map` (itself transcribed from
//! `draft-ietf-cellar-codec`, not from an implementation — D7/D9). Only the
//! codecs [`vaco_codec_core::CodecId`] can currently name are listed; a codec
//! with no row is refused rather than guessed, since writing a `CodecID` this
//! crate's own demuxer would not read back is worse than refusing the file.
//!
//! # The `webm` restriction
//!
//! Measured against `ffmpeg 8.1`: `ffmpeg -f lavfi -i testsrc ... -c:v libx264
//! -f webm bad.webm` fails `write_header` with exactly the text in
//! [`WEBM_REJECTION`] (not repeated here so the two copies cannot drift), so
//! a caller sees the same message. `V_AV1` is included
//! on the video side per the current `WebM` Project container guidelines,
//! which added AV1 after the original VP8/VP9-only text; subtitles are out of
//! this crate's scope entirely (no subtitle track is muxed).

use vaco_codec_core::CodecId;

/// The message `ffmpeg 8.1` prints verbatim when a `webm` output is asked to
/// carry a codec outside the allow-list.
pub const WEBM_REJECTION: &str = "Only VP8 or VP9 or AV1 video and Vorbis or Opus audio and WebVTT subtitles are supported for WebM.";

/// The Matroska `CodecID` for `id`, or `None` when this crate has no mapping.
///
/// `None` is a refusal, not a guess: [`crate::mux::MatroskaMuxer::add_stream`]
/// turns it into [`vaco_core::Error::Unsupported`].
#[must_use]
pub const fn codec_id_str(id: CodecId) -> Option<&'static str> {
    match id {
        CodecId::H264 => Some("V_MPEG4/ISO/AVC"),
        CodecId::Hevc => Some("V_MPEGH/ISO/HEVC"),
        CodecId::Av1 => Some("V_AV1"),
        CodecId::Vp8 => Some("V_VP8"),
        CodecId::Vp9 => Some("V_VP9"),
        CodecId::Aac | CodecId::AacLatm => Some("A_AAC"),
        CodecId::Opus => Some("A_OPUS"),
        CodecId::Vorbis => Some("A_VORBIS"),
        CodecId::Flac => Some("A_FLAC"),
        CodecId::Mp3 => Some("A_MPEG/L3"),
        CodecId::SubRip => Some("S_TEXT/UTF8"),
        CodecId::Pcm | CodecId::PcmS16le | CodecId::PcmS24le | CodecId::PcmS32le => {
            Some("A_PCM/INT/LIT")
        }
        CodecId::PcmS16be | CodecId::PcmS24be | CodecId::PcmS32be => Some("A_PCM/INT/BIG"),
        CodecId::PcmU8 | CodecId::PcmS8 => Some("A_PCM/INT/LIT"),
        CodecId::PcmF32le | CodecId::PcmF64le => Some("A_PCM/FLOAT/IEEE"),
        _ => None,
    }
}

/// Whether `id` is one of the video codecs a `webm` `DocType` may carry.
#[must_use]
pub const fn webm_allows_video(id: CodecId) -> bool {
    matches!(id, CodecId::Vp8 | CodecId::Vp9 | CodecId::Av1)
}

/// Whether `id` is one of the audio codecs a `webm` `DocType` may carry.
#[must_use]
pub const fn webm_allows_audio(id: CodecId) -> bool {
    matches!(id, CodecId::Vorbis | CodecId::Opus)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_brief_named_codecs_all_map() {
        assert_eq!(codec_id_str(CodecId::H264), Some("V_MPEG4/ISO/AVC"));
        assert_eq!(codec_id_str(CodecId::Hevc), Some("V_MPEGH/ISO/HEVC"));
        assert_eq!(codec_id_str(CodecId::Av1), Some("V_AV1"));
        assert_eq!(codec_id_str(CodecId::Opus), Some("A_OPUS"));
        assert_eq!(codec_id_str(CodecId::Vorbis), Some("A_VORBIS"));
        assert_eq!(codec_id_str(CodecId::Aac), Some("A_AAC"));
    }

    #[test]
    fn webm_accepts_only_the_measured_allow_list() {
        for v in [CodecId::Vp8, CodecId::Vp9, CodecId::Av1] {
            assert!(webm_allows_video(v));
        }
        assert!(!webm_allows_video(CodecId::H264));
        for a in [CodecId::Vorbis, CodecId::Opus] {
            assert!(webm_allows_audio(a));
        }
        assert!(!webm_allows_audio(CodecId::Aac));
    }

    #[test]
    fn every_mapped_codec_id_round_trips_through_the_demuxers_table() {
        // D19 in spirit: the two tables are separate definitions (one reads
        // strings, one writes them) but must agree on every row this crate
        // claims to support.
        for id in [
            CodecId::H264,
            CodecId::Hevc,
            CodecId::Av1,
            CodecId::Vp8,
            CodecId::Vp9,
            CodecId::Aac,
            CodecId::Opus,
            CodecId::Vorbis,
            CodecId::Flac,
            CodecId::Mp3,
        ] {
            let s = codec_id_str(id).unwrap();
            let mapped = vaco_demux_matroska::codec::map(s).and_then(|m| m.codec);
            assert_eq!(mapped, Some(id), "{s} does not read back as {id:?}");
        }
    }
}
