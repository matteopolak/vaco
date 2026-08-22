//! The `CodecID` string to [`CodecId`] mapping.
//!
//! # Provenance
//!
//! Every row is transcribed from the "Codec ID" line of the corresponding
//! section of *Matroska Media Container Codec Specifications*
//! (`draft-ietf-cellar-codec`, the `[MatroskaCodec]` reference RFC 9559 section
//! 5.1.4.1.21 points at for `CodecID`). Sections 3.3, 3.4 and 3.5 enumerate the
//! video, audio and subtitle IDs respectively. Nothing here was taken from an
//! implementation (D7/D9).
//!
//! # What "unmapped" means
//!
//! The draft defines 84 codec IDs; `vaco_codec_core::CodecId` currently has 14
//! variants. A row that the enum cannot name still resolves its **media type**,
//! which is what decides the stream's `codec_type` and its position in the
//! stream list — so an MKV with an AC-3 track reports the right number of
//! streams in the right order, with the audio stream's codec left unknown. The
//! table is complete against the draft, so the day `CodecId` grows a variant the
//! only change here is one `Some(...)`.

use vaco_codec_core::CodecId;
use vaco_core::MediaType;

/// What a Matroska `CodecID` resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub media: MediaType,
    /// `None` when the codec is defined by the draft but has no `CodecId`
    /// variant yet. The stream is still reported.
    pub codec: Option<CodecId>,
}

const fn v(codec: Option<CodecId>) -> Mapping {
    Mapping {
        media: MediaType::Video,
        codec,
    }
}
const fn a(codec: Option<CodecId>) -> Mapping {
    Mapping {
        media: MediaType::Audio,
        codec,
    }
}
const fn s() -> Mapping {
    Mapping {
        media: MediaType::Subtitle,
        codec: None,
    }
}

/// Codec IDs matched in full, longest first is not required because the match is
/// exact.
static EXACT: &[(&str, Mapping)] = &[
    // draft-ietf-cellar-codec section 3.3 — video
    ("V_AV1", v(Some(CodecId::Av1))),
    ("V_AVS2", v(None)),
    ("V_AVS3", v(None)),
    ("V_CAVS", v(None)),
    ("V_DIRAC", v(None)),
    ("V_FFV1", v(None)),
    ("V_MJPEG", v(Some(CodecId::Jpeg))),
    ("V_MPEGH/ISO/HEVC", v(Some(CodecId::Hevc))),
    ("V_MPEGI/ISO/VVC", v(None)),
    ("V_MPEG1", v(None)),
    ("V_MPEG2", v(None)),
    ("V_MPEG4/ISO/AVC", v(Some(CodecId::H264))),
    ("V_MPEG4/ISO/AP", v(None)),
    ("V_MPEG4/ISO/ASP", v(None)),
    ("V_MPEG4/ISO/SP", v(None)),
    ("V_MPEG4/MS/V3", v(None)),
    ("V_MS/VFW/FOURCC", v(None)),
    ("V_QUICKTIME", v(None)),
    ("V_PRORES", v(None)),
    ("V_REAL/RV10", v(None)),
    ("V_REAL/RV20", v(None)),
    ("V_REAL/RV30", v(None)),
    ("V_REAL/RV40", v(None)),
    ("V_THEORA", v(None)),
    ("V_UNCOMPRESSED", v(None)),
    ("V_VP8", v(Some(CodecId::Vp8))),
    ("V_VP9", v(Some(CodecId::Vp9))),
    // section 3.4 — audio
    ("A_AAC", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG2/LC", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG2/LC/SBR", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG2/MAIN", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG2/SSR", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG4/LC", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG4/LC/SBR", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG4/LTP", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG4/MAIN", a(Some(CodecId::Aac))),
    ("A_AAC/MPEG4/SSR", a(Some(CodecId::Aac))),
    ("A_AC3", a(None)),
    ("A_AC3/BSID9", a(None)),
    ("A_AC3/BSID10", a(None)),
    ("A_ALAC", a(None)),
    ("A_ATRAC/AT1", a(None)),
    ("A_DTS", a(None)),
    ("A_DTS/EXPRESS", a(None)),
    ("A_DTS/LOSSLESS", a(None)),
    ("A_EAC3", a(None)),
    ("A_FLAC", a(Some(CodecId::Flac))),
    ("A_MLP", a(None)),
    ("A_MPC", a(None)),
    ("A_MPEG/L1", a(None)),
    ("A_MPEG/L2", a(None)),
    ("A_MPEG/L3", a(Some(CodecId::Mp3))),
    ("A_MS/ACM", a(None)),
    ("A_REAL/14_4", a(None)),
    ("A_REAL/28_8", a(None)),
    ("A_REAL/ATRC", a(None)),
    ("A_REAL/COOK", a(None)),
    ("A_REAL/RALF", a(None)),
    ("A_REAL/SIPR", a(None)),
    ("A_OPUS", a(Some(CodecId::Opus))),
    ("A_PCM/FLOAT/IEEE", a(Some(CodecId::Pcm))),
    ("A_PCM/INT/BIG", a(Some(CodecId::Pcm))),
    ("A_PCM/INT/LIT", a(Some(CodecId::Pcm))),
    ("A_QUICKTIME", a(None)),
    ("A_QUICKTIME/QDMC", a(None)),
    ("A_QUICKTIME/QDM2", a(None)),
    ("A_TRUEHD", a(None)),
    ("A_TTA1", a(None)),
    ("A_VORBIS", a(Some(CodecId::Vorbis))),
    ("A_WAVPACK4", a(None)),
    // section 3.5 — subtitles, and the one button type
    ("S_ARIBSUB", s()),
    ("S_DVBSUB", s()),
    ("S_HDMV/PGS", s()),
    ("S_HDMV/TEXTST", s()),
    ("S_KATE", s()),
    ("S_IMAGE/BMP", s()),
    ("S_TEXT/ASS", s()),
    ("S_TEXT/ASCII", s()),
    ("S_TEXT/SSA", s()),
    ("S_TEXT/USF", s()),
    ("S_TEXT/UTF8", s()),
    ("S_TEXT/WEBVTT", s()),
    ("S_VOBSUB", s()),
    (
        "B_VOBBTN",
        Mapping {
            media: MediaType::Data,
            codec: None,
        },
    ),
];

/// Resolve a `CodecID`.
///
/// Falls back to the `V_`/`A_`/`S_`/`B_` prefix when the exact string is not in
/// the draft, because an unknown codec in a known media type is still a stream
/// the file declares and dropping it would change the stream count — which is
/// the first number `ffprobe` prints.
#[must_use]
pub fn map(codec_id: &str) -> Option<Mapping> {
    if let Some(&(_, m)) = EXACT.iter().find(|(k, _)| *k == codec_id) {
        return Some(m);
    }
    // Some muxers append a profile suffix to a defined prefix, and the draft's
    // own AAC rows are exactly that shape. Longest defined prefix wins.
    let by_prefix = EXACT
        .iter()
        .filter(|(k, _)| codec_id.starts_with(k) && codec_id.as_bytes().get(k.len()) == Some(&b'/'))
        .max_by_key(|(k, _)| k.len());
    if let Some(&(_, m)) = by_prefix {
        return Some(m);
    }
    match codec_id.as_bytes().first()? {
        b'V' => Some(v(None)),
        b'A' => Some(a(None)),
        b'S' => Some(s()),
        b'B' => Some(Mapping {
            media: MediaType::Data,
            codec: None,
        }),
        _ => None,
    }
}

/// Whether this codec's `CodecPrivate` is the codec's extradata verbatim.
///
/// True for everything we map today: `V_MPEG4/ISO/AVC` stores an
/// `AVCDecoderConfigurationRecord`, `V_MPEGH/ISO/HEVC` an
/// `HEVCDecoderConfigurationRecord`, `A_OPUS` an `OpusHead`, `A_VORBIS` the
/// Xiph-packed headers and `A_FLAC` the `fLaC` stream — each of which is exactly
/// what a decoder for that codec expects to be handed. The exceptions are
/// `V_MS/VFW/FOURCC` and `A_MS/ACM`, whose `CodecPrivate` is a
/// `BITMAPINFOHEADER` or `WAVEFORMATEX` that has to be unwrapped first; neither
/// is mapped yet, and unwrapping them needs `vaco-format-riff`.
#[must_use]
pub fn private_is_extradata(codec_id: &str) -> bool {
    !matches!(codec_id, "V_MS/VFW/FOURCC" | "A_MS/ACM")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn exact_rows_resolve() {
        assert_eq!(
            map("V_MPEG4/ISO/AVC").map(|m| m.codec),
            Some(Some(CodecId::H264))
        );
        assert_eq!(map("A_OPUS").map(|m| m.codec), Some(Some(CodecId::Opus)));
        assert_eq!(map("V_VP9").map(|m| m.codec), Some(Some(CodecId::Vp9)));
    }

    #[test]
    fn media_type_survives_an_unmapped_codec() {
        let m = map("A_AC3").expect("A_AC3 is in the draft");
        assert_eq!(m.media, MediaType::Audio);
        assert_eq!(m.codec, None);
    }

    #[test]
    fn unknown_id_falls_back_to_its_prefix() {
        assert_eq!(
            map("V_SOMETHING_NEW").map(|m| m.media),
            Some(MediaType::Video)
        );
        assert_eq!(
            map("S_TEXT/FUTURE").map(|m| m.media),
            Some(MediaType::Subtitle)
        );
        assert_eq!(map(""), None);
        assert_eq!(map("X_NONSENSE"), None);
    }

    #[test]
    fn longest_defined_prefix_wins() {
        // Not a defined row, but `A_AAC/MPEG4/LC` is, and the suffix must not
        // demote it to the bare `A_` fallback.
        assert_eq!(
            map("A_AAC/MPEG4/LC/EXTRA").map(|m| m.codec),
            Some(Some(CodecId::Aac))
        );
    }

    #[test]
    fn every_row_has_a_prefix_consistent_media_type() {
        for &(name, m) in EXACT {
            let expected = match name.as_bytes().first() {
                Some(b'V') => MediaType::Video,
                Some(b'A') => MediaType::Audio,
                Some(b'S') => MediaType::Subtitle,
                _ => MediaType::Data,
            };
            assert_eq!(m.media, expected, "{name}");
        }
    }
}
