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
//! # E2E-GAPS 2 (2026-08-29): a table that stopped tracking the codecs this
//! build actually has
//!
//! `Ffv1`, `Theora`, `Mpeg1video`, `Mpeg2video`, `Prores`, `Jpeg` (video) and
//! `Ac3`, `Alac`, `Mp1`, `Mp2` (audio) all had real decoders and/or encoders
//! registered elsewhere in the tree but no row here, so `-c:v ffv1 out.mkv`
//! (and a bare `-c:v copy`/`-c:a copy` remux of any of the others) refused the
//! stream with "matroska: codec has no `CodecID` mapping" even though this
//! build could produce or already held the bitstream. Every added string is
//! the write-side twin of an already-transcribed, already-tested row in
//! `vaco_demux_matroska::codec::EXACT`; `V_FFV1`, `V_THEORA`, `V_MPEG1`,
//! `V_MPEG2`, `A_AC3`, `A_ALAC`, `A_MPEG/L1`, `A_MPEG/L2` were additionally
//! confirmed against real `ffmpeg 8.1 -c:v ffv1`/`-c:a ac3`/`-c:a alac`/`-c:a
//! mp2` output muxed to `.mkv` (`strings` on the file shows the `CodecID`
//! element's own bytes; `libtheora`/an MP1 encoder were not available in the
//! probing environment, so `V_THEORA` and `A_MPEG/L1` rest on the spec table
//! alone, same as every other row above them that predates this change).
//! `V_MJPEG` and `V_PRORES` are the demuxer table's own strings for `Jpeg`/
//! `Prores` unchanged. (`Flac` already had a row — `A_FLAC` was never
//! actually broken; the gap report's repro command failed earlier, on AAC
//! decode, and never reached this table at all.)
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
        CodecId::Ffv1 => Some("V_FFV1"),
        CodecId::Theora => Some("V_THEORA"),
        CodecId::Mpeg1video => Some("V_MPEG1"),
        CodecId::Mpeg2video => Some("V_MPEG2"),
        CodecId::Prores => Some("V_PRORES"),
        CodecId::Jpeg => Some("V_MJPEG"),
        CodecId::Aac | CodecId::AacLatm => Some("A_AAC"),
        CodecId::Opus => Some("A_OPUS"),
        CodecId::Vorbis => Some("A_VORBIS"),
        CodecId::Flac => Some("A_FLAC"),
        CodecId::Mp1 => Some("A_MPEG/L1"),
        CodecId::Mp2 => Some("A_MPEG/L2"),
        CodecId::Mp3 => Some("A_MPEG/L3"),
        CodecId::Ac3 => Some("A_AC3"),
        CodecId::Alac => Some("A_ALAC"),
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

/// Whether the track whose Matroska `CodecID` is `codec_id_str` needs a
/// non-empty out-of-band Configuration Record to be decodable at all — the
/// bytes [`crate::mux::MatroskaMuxer`] writes into `CodecPrivate`.
///
/// This is deliberately keyed on the *Matroska* string rather than
/// [`CodecId`]: `TrackOut` only ever holds the string (the `CodecID` this
/// crate committed to at `add_stream`), and the question is about what that
/// specific `CodecID` needs, which is a fact about Matroska/WebM (RFC 9559),
/// not about the codec in the abstract — the same codec can have a
/// `CodecID` that embeds enough in-band signalling to need nothing here in
/// principle, but every string this table emits is the one measured against
/// `ffmpeg 8.1` (module docs above), which always writes one for these.
///
/// # Why this exists
///
/// Nothing before it distinguished "no `CodecPrivate` because this codec
/// does not need one" (`V_VP9`, `A_PCM/INT/LIT`, ...) from "no
/// `CodecPrivate` because the encoder never got a chance to supply the real
/// one" — so a stream that fell into the second case was written anyway,
/// producing a file that mounts and reports a stream but that no decoder can
/// actually open. [`crate::mux::MatroskaMuxer::flush_header_bytes`] calls
/// this right before it commits `Tracks` for good, and refuses the write
/// outright rather than let that distinction stay invisible.
///
/// `V_MPEG4/ISO/AVC`/`V_MPEGH/ISO/HEVC` need their `avcC`/`hvcC`; `V_FFV1`
/// needs RFC 9043's Configuration Record (this crate's own encoder always
/// emits version 3, which mandates one); `A_VORBIS`/`A_OPUS`/`A_FLAC`/
/// `A_ALAC`/`A_AAC` each need their own header structure
/// (identification+setup packets, `OpusHead`, `STREAMINFO`,
/// `ALACSpecificConfig`, `AudioSpecificConfig`). Every other mapped `CodecID`
/// is a self-contained bitstream (`V_VP8`/`V_VP9`/`V_AV1`/`V_THEORA`/
/// `V_MPEG1`/`V_MPEG2`/`V_PRORES`/`V_MJPEG`, every `A_PCM/*`, `A_MPEG/L*`,
/// `A_AC3`) and needs nothing here even though several of them (Theora,
/// Vorbis' own sibling) traditionally travel *with* private headers in other
/// containers — Matroska specifically does not require it for these.
#[must_use]
pub fn requires_extradata_str(codec_id_str: &str) -> bool {
    matches!(
        codec_id_str,
        "V_MPEG4/ISO/AVC"
            | "V_MPEGH/ISO/HEVC"
            | "V_FFV1"
            | "A_VORBIS"
            | "A_OPUS"
            | "A_FLAC"
            | "A_ALAC"
            | "A_AAC"
    )
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

    /// E2E-GAPS 2: the codecs this build actually implements that the table
    /// used to refuse. `Flac` is deliberately re-asserted here too — it was
    /// never missing; see the module docs on why the original report named it
    /// anyway.
    #[test]
    fn e2e_gaps_2_codecs_now_map() {
        assert_eq!(codec_id_str(CodecId::Ffv1), Some("V_FFV1"));
        assert_eq!(codec_id_str(CodecId::Theora), Some("V_THEORA"));
        assert_eq!(codec_id_str(CodecId::Mpeg1video), Some("V_MPEG1"));
        assert_eq!(codec_id_str(CodecId::Mpeg2video), Some("V_MPEG2"));
        assert_eq!(codec_id_str(CodecId::Prores), Some("V_PRORES"));
        assert_eq!(codec_id_str(CodecId::Jpeg), Some("V_MJPEG"));
        assert_eq!(codec_id_str(CodecId::Ac3), Some("A_AC3"));
        assert_eq!(codec_id_str(CodecId::Alac), Some("A_ALAC"));
        assert_eq!(codec_id_str(CodecId::Mp1), Some("A_MPEG/L1"));
        assert_eq!(codec_id_str(CodecId::Mp2), Some("A_MPEG/L2"));
        assert_eq!(codec_id_str(CodecId::Flac), Some("A_FLAC"));
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
            CodecId::Ffv1,
            CodecId::Theora,
            CodecId::Mpeg1video,
            CodecId::Mpeg2video,
            CodecId::Prores,
            CodecId::Jpeg,
            CodecId::Ac3,
            CodecId::Alac,
            CodecId::Mp1,
            CodecId::Mp2,
        ] {
            let s = codec_id_str(id).unwrap();
            let mapped = vaco_demux_matroska::codec::map(s).and_then(|m| m.codec);
            assert_eq!(mapped, Some(id), "{s} does not read back as {id:?}");
        }
    }
}
