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
use vaco_sampfmt::SampleFmt;

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

/// Refine `A_PCM/*`'s generic [`CodecId::Pcm`] into the exact
/// `CodecId::Pcm*` variant a decoder can actually be found for, from the
/// `CodecID` string's family (`INT/LIT`, `INT/BIG`, `FLOAT/IEEE`) and the
/// `Audio.BitDepth` element -- `CodecID` alone states signedness and
/// endianness but never depth, and `BitDepth` alone states depth but not
/// endianness, so neither is sufficient on its own.
///
/// [`CodecId::Pcm`] itself has no registered decoder anywhere in this tree
/// (only a `PcmFormat` fallback `vaco-codec-pcm::resolve` documents as
/// unreachable in practice) -- so every real `A_PCM/*` track was landing on
/// a `CodecId` selection had never actually been able to decode, reported
/// `codec_name=pcm`/`sample_fmt=unknown` in `-show_streams`, and failed
/// outright ("this build has no decoder for the input codec") the moment
/// anything tried to decode it, which the reference's own `pcm_s16le`/
/// `sample_fmt=s16` never does.
///
/// Measured against real `ffmpeg -c:a pcm_*` Matroska fixtures at every
/// depth it will actually encode there (8/16/24/32-bit int, 32/64-bit
/// float): 8-bit is always unsigned regardless of the `INT/LIT`/`INT/BIG`
/// family (there is no endianness to state at one byte -- ffmpeg tags it
/// `INT/LIT` regardless), every other `INT` depth is signed, and
/// `FLOAT/IEEE` never appears as big-endian (`ffmpeg` refuses to write one
/// to Matroska at all: "Invalid argument").
///
/// `None` for anything this cannot place confidently -- an unexpected bit
/// depth, or a `CodecID` outside the three known families -- leaving the
/// caller's existing generic [`CodecId::Pcm`] in place. That is a clean
/// refusal at decode time rather than a guess at wire format: better to
/// report "no decoder for this codec" than to hand a decoder the wrong
/// sample width or endianness.
#[must_use]
pub fn resolve_pcm(codec_id: &str, bit_depth: Option<u8>) -> Option<CodecId> {
    if codec_id.starts_with("A_PCM/INT/") && bit_depth == Some(8) {
        return Some(CodecId::PcmU8);
    }
    match (codec_id, bit_depth) {
        ("A_PCM/INT/LIT", Some(16)) => Some(CodecId::PcmS16le),
        ("A_PCM/INT/BIG", Some(16)) => Some(CodecId::PcmS16be),
        ("A_PCM/INT/LIT", Some(24)) => Some(CodecId::PcmS24le),
        ("A_PCM/INT/BIG", Some(24)) => Some(CodecId::PcmS24be),
        ("A_PCM/INT/LIT", Some(32)) => Some(CodecId::PcmS32le),
        ("A_PCM/INT/BIG", Some(32)) => Some(CodecId::PcmS32be),
        ("A_PCM/FLOAT/IEEE", Some(32)) => Some(CodecId::PcmF32le),
        ("A_PCM/FLOAT/IEEE", Some(64)) => Some(CodecId::PcmF64le),
        _ => None,
    }
}

/// The decoded sample format [`resolve_pcm`]'s result decodes to, matching
/// `vaco-codec-pcm::table::PCM_FORMATS`'s `decoded` column for the same
/// `CodecId` -- duplicated rather than depended on (D14.1: a demux crate
/// does not reach into a codec crate for this), and only for the nine
/// variants `resolve_pcm` can actually return.
#[must_use]
pub const fn pcm_format(id: CodecId) -> Option<SampleFmt> {
    match id {
        CodecId::PcmU8 => Some(SampleFmt::U8),
        CodecId::PcmS16le | CodecId::PcmS16be => Some(SampleFmt::S16),
        CodecId::PcmS24le | CodecId::PcmS24be | CodecId::PcmS32le | CodecId::PcmS32be => {
            Some(SampleFmt::S32)
        }
        CodecId::PcmF32le => Some(SampleFmt::F32),
        CodecId::PcmF64le => Some(SampleFmt::F64),
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

    /// Every depth `ffmpeg`'s own Matroska muxer will actually write,
    /// measured directly (see `resolve_pcm`'s own doc). Regression for a
    /// real bug: before this table existed, every `A_PCM/*` track resolved
    /// to the generic `CodecId::Pcm`, which has no decoder anywhere in this
    /// tree -- `vaco -i pcm.mka -f s16le out.raw` failed outright with
    /// "this build has no decoder for the input codec", something the
    /// reference never does for real PCM.
    #[test]
    fn resolve_pcm_matches_every_measured_depth() {
        let cases = [
            ("A_PCM/INT/LIT", 8, CodecId::PcmU8),
            ("A_PCM/INT/BIG", 8, CodecId::PcmU8), // no endianness at one byte
            ("A_PCM/INT/LIT", 16, CodecId::PcmS16le),
            ("A_PCM/INT/BIG", 16, CodecId::PcmS16be),
            ("A_PCM/INT/LIT", 24, CodecId::PcmS24le),
            ("A_PCM/INT/BIG", 24, CodecId::PcmS24be),
            ("A_PCM/INT/LIT", 32, CodecId::PcmS32le),
            ("A_PCM/INT/BIG", 32, CodecId::PcmS32be),
            ("A_PCM/FLOAT/IEEE", 32, CodecId::PcmF32le),
            ("A_PCM/FLOAT/IEEE", 64, CodecId::PcmF64le),
        ];
        for (id, depth, want) in cases {
            assert_eq!(
                resolve_pcm(id, Some(depth)),
                Some(want),
                "{id} at {depth}-bit"
            );
        }
    }

    #[test]
    fn resolve_pcm_refuses_rather_than_guesses() {
        // No BitDepth at all: cannot place a width, must not guess one.
        assert_eq!(resolve_pcm("A_PCM/INT/LIT", None), None);
        // A depth this crate has never measured a real encoder produce.
        assert_eq!(resolve_pcm("A_PCM/INT/LIT", Some(12)), None);
        // `FLOAT/IEEE` big-endian: never observed, per the module doc --
        // there is no `CodecID` spelling for it to even reach this function
        // with, so a caller asking anyway must not get a wrong-endian guess.
        assert_eq!(resolve_pcm("A_PCM/FLOAT/IEEE", Some(16)), None);
        // Outside the three known families entirely.
        assert_eq!(resolve_pcm("A_PCM/UNKNOWN", Some(16)), None);
    }

    #[test]
    fn pcm_format_matches_the_codec_pcm_table() {
        let cases = [
            (CodecId::PcmU8, SampleFmt::U8),
            (CodecId::PcmS16le, SampleFmt::S16),
            (CodecId::PcmS16be, SampleFmt::S16),
            (CodecId::PcmS24le, SampleFmt::S32),
            (CodecId::PcmS24be, SampleFmt::S32),
            (CodecId::PcmS32le, SampleFmt::S32),
            (CodecId::PcmS32be, SampleFmt::S32),
            (CodecId::PcmF32le, SampleFmt::F32),
            (CodecId::PcmF64le, SampleFmt::F64),
        ];
        for (id, want) in cases {
            assert_eq!(pcm_format(id), Some(want), "{id:?}");
        }
        // The generic id `resolve_pcm` never returns, and anything this
        // module was not asked to place, must not claim a format either.
        assert_eq!(pcm_format(CodecId::Pcm), None);
        assert_eq!(pcm_format(CodecId::Aac), None);
    }
}
