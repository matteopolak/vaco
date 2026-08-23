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
//! The draft defines 84 codec IDs; `vaco_codec_core::CodecId` has grown since
//! this was written (84 variants as of finding 4's fix — see
//! `planning/CONFORMANCE-FINDINGS.md`). A row that the enum cannot name still
//! resolves its **media type**, which is what decides the stream's
//! `codec_type` and its position in the stream list — so an MKV with an AC-3
//! track reports the right number of streams in the right order, with the
//! audio stream's codec left unknown. The table is complete against the
//! draft, so the day `CodecId` grows a variant the only change here is one
//! `Some(...)`.
//!
//! Finding 4 found this table mapping far fewer rows than `CodecId` could
//! already support — `V_MPEG1`, `A_AC3`, `A_TRUEHD` and others sat on `None`
//! while a matching variant existed unused. Every row below that changed to
//! `Some(...)` was checked against an existing variant; nothing was added to
//! `CodecId` to make this table more complete. `V_AVS2` and `V_AVS3` were the two
//! exceptions, reported rather than worked around; `vaco-codec-core` gained
//! `Avs2` and `Avs3` shortly afterwards and both now map.

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
/// A subtitle track whose codec `CodecId` can name.
///
/// Most cannot yet, which is why [`s`] exists and answers `None` — reporting a
/// media type without a codec is honest, and better than guessing a name the
/// reference does not print.
const fn sc(codec: CodecId) -> Mapping {
    Mapping {
        media: MediaType::Subtitle,
        codec: Some(codec),
    }
}

/// Codec IDs matched in full, longest first is not required because the match is
/// exact.
static EXACT: &[(&str, Mapping)] = &[
    // draft-ietf-cellar-codec section 3.3 — video
    ("V_AV1", v(Some(CodecId::Av1))),
    // AVS2/AVS3 have no `CodecId` variant yet — see the module docs.
    ("V_AVS2", v(Some(CodecId::Avs2))),
    ("V_AVS3", v(Some(CodecId::Avs3))),
    ("V_CAVS", v(Some(CodecId::Cavs))),
    ("V_DIRAC", v(Some(CodecId::Dirac))),
    ("V_FFV1", v(Some(CodecId::Ffv1))),
    ("V_MJPEG", v(Some(CodecId::Jpeg))),
    ("V_MPEGH/ISO/HEVC", v(Some(CodecId::Hevc))),
    ("V_MPEGI/ISO/VVC", v(Some(CodecId::Vvc))),
    ("V_MPEG1", v(Some(CodecId::Mpeg1video))),
    ("V_MPEG2", v(Some(CodecId::Mpeg2video))),
    ("V_MPEG4/ISO/AVC", v(Some(CodecId::H264))),
    // The three ISO MPEG-4 part 2 profiles are one codec at the `CodecId`
    // level, the same collapse this table already made for AAC's profile
    // suffixes below.
    ("V_MPEG4/ISO/AP", v(Some(CodecId::Mpeg4))),
    ("V_MPEG4/ISO/ASP", v(Some(CodecId::Mpeg4))),
    ("V_MPEG4/ISO/SP", v(Some(CodecId::Mpeg4))),
    ("V_MPEG4/MS/V3", v(Some(CodecId::Msmpeg4v3))),
    // The codec is named by an arbitrary FOURCC/BITMAPINFOHEADER carried in
    // `CodecPrivate`, not by the `CodecID` string itself — genuinely
    // unresolvable from this table alone, and doing it right needs
    // `vaco-format-riff` to unwrap the structure first (see
    // `private_is_extradata`'s docs).
    ("V_MS/VFW/FOURCC", v(None)),
    ("V_QUICKTIME", v(None)),
    ("V_PRORES", v(Some(CodecId::Prores))),
    // RealVideo has no `CodecId` variant yet.
    ("V_REAL/RV10", v(None)),
    ("V_REAL/RV20", v(None)),
    ("V_REAL/RV30", v(None)),
    ("V_REAL/RV40", v(None)),
    ("V_THEORA", v(Some(CodecId::Theora))),
    // Same shape as `V_MS/VFW/FOURCC`: the pixel format is in `CodecPrivate`,
    // not in the `CodecID` string.
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
    // The three AC-3 rows and the three DTS rows are bitstream-ID/profile
    // variants of one codec at the `CodecId` level, the same collapse this
    // table already made for AAC's profile suffixes above.
    ("A_AC3", a(Some(CodecId::Ac3))),
    ("A_AC3/BSID9", a(Some(CodecId::Ac3))),
    ("A_AC3/BSID10", a(Some(CodecId::Ac3))),
    ("A_ALAC", a(Some(CodecId::Alac))),
    // RealAudio (ATRAC/14_4/28_8/COOK/RALF/SIPR) has no `CodecId` variant yet.
    ("A_ATRAC/AT1", a(None)),
    ("A_DTS", a(Some(CodecId::Dts))),
    ("A_DTS/EXPRESS", a(Some(CodecId::Dts))),
    ("A_DTS/LOSSLESS", a(Some(CodecId::Dts))),
    ("A_EAC3", a(Some(CodecId::Eac3))),
    ("A_FLAC", a(Some(CodecId::Flac))),
    // MLP (Meridian Lossless Packing, the layer TrueHD is built on) has no
    // `CodecId` variant yet.
    ("A_MLP", a(None)),
    // Musepack has no `CodecId` variant yet.
    ("A_MPC", a(None)),
    ("A_MPEG/L1", a(Some(CodecId::Mp1))),
    ("A_MPEG/L2", a(Some(CodecId::Mp2))),
    ("A_MPEG/L3", a(Some(CodecId::Mp3))),
    // The codec is named by the `WAVEFORMATEX` tag in `CodecPrivate`, the
    // audio counterpart of `V_MS/VFW/FOURCC` above — same reason, same fix.
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
    ("A_TRUEHD", a(Some(CodecId::Truehd))),
    // True Audio (TTA) has no `CodecId` variant yet.
    ("A_TTA1", a(None)),
    ("A_VORBIS", a(Some(CodecId::Vorbis))),
    // WavPack has no `CodecId` variant yet.
    ("A_WAVPACK4", a(None)),
    // section 3.5 — subtitles, and the one button type
    // ARIB, DVB and Kate subtitles, and HDMV TextST, have no `CodecId`
    // variant yet.
    ("S_ARIBSUB", s()),
    ("S_DVBSUB", s()),
    ("S_HDMV/PGS", sc(CodecId::HdmvPgsSubtitle)),
    ("S_HDMV/TEXTST", s()),
    ("S_KATE", s()),
    // The bitmap format is carried in `CodecPrivate`/the block data, not
    // named unambiguously enough by the bare `CodecID` string to commit to a
    // single `CodecId` here.
    ("S_IMAGE/BMP", s()),
    ("S_TEXT/ASS", sc(CodecId::Ass)),
    ("S_TEXT/ASCII", s()),
    ("S_TEXT/SSA", sc(CodecId::Ssa)),
    ("S_TEXT/USF", s()),
    // Measured: `ffprobe` prints `codec_name=subrip` for this track.
    ("S_TEXT/UTF8", sc(CodecId::SubRip)),
    ("S_TEXT/WEBVTT", sc(CodecId::Webvtt)),
    ("S_VOBSUB", sc(CodecId::DvdSubtitle)),
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
        // `V_MS/VFW/FOURCC` is still genuinely unmapped: the codec it names
        // lives in `CodecPrivate`, not in the `CodecID` string, so this one
        // stays `None` even after finding 4's fix (unlike `A_AC3`, which
        // this test used to name and which now resolves to `CodecId::Ac3`).
        let m = map("V_MS/VFW/FOURCC").expect("V_MS/VFW/FOURCC is in the draft");
        assert_eq!(m.media, MediaType::Video);
        assert_eq!(m.codec, None);
    }

    /// Finding 4: rows that used to sit on `None` despite `CodecId` already
    /// having a matching variant. Each assertion here is the positive
    /// mapping, not a still-missing row's absence, per
    /// `planning/AGENT-CONSTRAINTS.md` "Never pin the absence of something
    /// the project is building".
    #[test]
    fn previously_unmapped_rows_now_resolve() {
        let cases = [
            ("V_CAVS", CodecId::Cavs),
            ("V_DIRAC", CodecId::Dirac),
            ("V_FFV1", CodecId::Ffv1),
            ("V_MPEGI/ISO/VVC", CodecId::Vvc),
            ("V_MPEG1", CodecId::Mpeg1video),
            ("V_MPEG2", CodecId::Mpeg2video),
            ("V_MPEG4/ISO/AP", CodecId::Mpeg4),
            ("V_MPEG4/ISO/ASP", CodecId::Mpeg4),
            ("V_MPEG4/ISO/SP", CodecId::Mpeg4),
            ("V_MPEG4/MS/V3", CodecId::Msmpeg4v3),
            ("V_PRORES", CodecId::Prores),
            ("V_THEORA", CodecId::Theora),
            ("A_AC3", CodecId::Ac3),
            ("A_AC3/BSID9", CodecId::Ac3),
            ("A_AC3/BSID10", CodecId::Ac3),
            ("A_ALAC", CodecId::Alac),
            ("A_DTS", CodecId::Dts),
            ("A_DTS/EXPRESS", CodecId::Dts),
            ("A_DTS/LOSSLESS", CodecId::Dts),
            ("A_EAC3", CodecId::Eac3),
            ("A_MPEG/L1", CodecId::Mp1),
            ("A_MPEG/L2", CodecId::Mp2),
            ("A_TRUEHD", CodecId::Truehd),
            ("S_HDMV/PGS", CodecId::HdmvPgsSubtitle),
            ("S_TEXT/ASS", CodecId::Ass),
            ("S_TEXT/SSA", CodecId::Ssa),
            ("S_TEXT/WEBVTT", CodecId::Webvtt),
            ("S_VOBSUB", CodecId::DvdSubtitle),
        ];
        for (id, want) in cases {
            assert_eq!(map(id).and_then(|m| m.codec), Some(want), "{id}");
        }
    }

    /// Written when `V_AVS2`/`V_AVS3` had no `CodecId` to map onto, and
    /// deliberately *not* asserting `None` as a permanent fact — that is the
    /// "pin the absence" anti-pattern that has now cost this project six
    /// tests, each of which failed on the day its gap was closed. The variants
    /// arrived an hour later and this test needed no edit, which is the whole
    /// argument for writing them this way.
    #[test]
    fn avs_rows_resolve_to_a_media_type_regardless_of_codec_id() {
        for id in ["V_AVS2", "V_AVS3"] {
            let m = map(id).expect("V_AVS2/V_AVS3 are in the draft");
            assert_eq!(m.media, MediaType::Video, "{id}");
        }
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
