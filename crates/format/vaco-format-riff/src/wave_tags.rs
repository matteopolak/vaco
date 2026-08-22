//! `wFormatTag` → codec identity.
//!
//! Two separate tables, deliberately not merged, because they carry different
//! kinds of claim:
//!
//! - [`codec_name`] returns the exact string `ffprobe` 8.1 prints as
//!   `codec_name`, and every entry is backed by a command in this file's
//!   tests that reproduces it. Get one of these spellings wrong and every
//!   consumer that reports it as an interface fact (D9) is wrong the same way.
//! - [`tag_description`] returns the MS/RFC 2361 registered name for the
//!   value — a structural fact about the format, not a claim about what any
//!   particular tool prints. It is intentionally smaller than the full RFC
//!   2361 registry: every entry here is one this crate's author was already
//!   confident of independent of any tool, rather than transcribed wholesale
//!   from memory against the brief's own warning about exactly that mistake.
//!   `docs/format/vaco-format-riff.md` names RFC 2361 for the rest.
//!
//! # Why `wFormatTag == 1` is not enough by itself
//!
//! `WAVE_FORMAT_PCM` and `WAVE_FORMAT_IEEE_FLOAT` do not name a codec on
//! their own — `ffprobe` reports a different `codec_name` for 8/16/24/32-bit
//! integer PCM and for 32/64-bit float, all sharing one of two tags. Probed:
//!
//! | encoder | `wFormatTag` | `wBitsPerSample` | `codec_name` |
//! |---|---|---|---|
//! | `pcm_u8` | 1 | 8 | `pcm_u8` |
//! | `pcm_s16le` | 1 | 16 | `pcm_s16le` |
//! | `pcm_s24le` | 1 (as `WAVEFORMATEXTENSIBLE`) | 24 | `pcm_s24le` |
//! | `pcm_s32le` | 1 (as `WAVEFORMATEXTENSIBLE`) | 32 | `pcm_s32le` |
//! | `pcm_f32le` | 3 | 32 | `pcm_f32le` |
//! | `pcm_f64le` | 3 | 64 | `pcm_f64le` |
//!
//! [`codec_name`] therefore takes the whole [`crate::wave::WaveFormatEx`],
//! not just the tag, and resolves the extensible sub-format first.

use vaco_codec_core::CodecId;

use crate::wave::{
    WAVE_FORMAT_AAC, WAVE_FORMAT_ADPCM, WAVE_FORMAT_ALAW, WAVE_FORMAT_DOLBY_AC3_SPDIF,
    WAVE_FORMAT_DVI_ADPCM, WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_MPEG, WAVE_FORMAT_MPEGLAYER3,
    WAVE_FORMAT_MULAW, WAVE_FORMAT_PCM, WAVE_FORMAT_WMAUDIO1, WAVE_FORMAT_WMAUDIO2, WaveFormatEx,
};

/// Integer PCM by container bit depth, all under `WAVE_FORMAT_PCM` (or its
/// `WAVEFORMATEXTENSIBLE` `KSDATAFORMAT_SUBTYPE_PCM` spelling).
///
/// Probed: `ffmpeg -f lavfi -i sine=... -c:a <name> out.wav`, then
/// `ffprobe -show_entries stream=codec_name`.
fn pcm_codec_name(bits_per_sample: u16) -> Option<&'static str> {
    match bits_per_sample {
        8 => Some("pcm_u8"),
        16 => Some("pcm_s16le"),
        24 => Some("pcm_s24le"),
        32 => Some("pcm_s32le"),
        _ => None,
    }
}

/// IEEE float PCM by container bit depth, under `WAVE_FORMAT_IEEE_FLOAT`.
fn float_codec_name(bits_per_sample: u16) -> Option<&'static str> {
    match bits_per_sample {
        32 => Some("pcm_f32le"),
        64 => Some("pcm_f64le"),
        _ => None,
    }
}

/// The `ffprobe` 8.1 `codec_name` for a parsed `fmt` structure, or `None` for
/// a tag this crate has not verified a spelling for.
///
/// Resolves `WAVEFORMATEXTENSIBLE` to its `SubFormat` tag first (falling back
/// to the raw `0xFFFE` only if the GUID is not one of the standard Microsoft
/// media subtypes, in which case there is nothing to name).
#[must_use]
pub fn codec_name(fmt: &WaveFormatEx) -> Option<&'static str> {
    let tag = fmt
        .extensible()
        .and_then(|e| e.sub_format_tag())
        .unwrap_or(fmt.format_tag);
    match tag {
        WAVE_FORMAT_PCM => pcm_codec_name(fmt.bits_per_sample),
        WAVE_FORMAT_IEEE_FLOAT => float_codec_name(fmt.bits_per_sample),
        WAVE_FORMAT_ADPCM => Some("adpcm_ms"),
        WAVE_FORMAT_ALAW => Some("pcm_alaw"),
        WAVE_FORMAT_MULAW => Some("pcm_mulaw"),
        WAVE_FORMAT_DVI_ADPCM => Some("adpcm_ima_wav"),
        WAVE_FORMAT_MPEG => Some("mp2"),
        WAVE_FORMAT_MPEGLAYER3 => Some("mp3"),
        WAVE_FORMAT_WMAUDIO1 => Some("wmav1"),
        WAVE_FORMAT_WMAUDIO2 => Some("wmav2"),
        WAVE_FORMAT_DOLBY_AC3_SPDIF => Some("ac3"),
        WAVE_FORMAT_AAC => Some("aac"),
        // Probed via `ffmpeg -c:a g722 -f wav`: ffprobe reports `codec_name`
        // `adpcm_g722` for `wFormatTag = 0x028f`. Not an RFC 2361 name (the
        // registry does not list 0x028f at all); this is purely the observed
        // reference spelling.
        0x028f => Some("adpcm_g722"),
        _ => None,
    }
}

/// A best-effort [`CodecId`] for a parsed `fmt` structure.
///
/// `CodecId` is a small, hand-maintained enum (`vaco-codec-core`, not owned
/// by this crate) that does not yet have a variant for most WAVE format
/// tags — there is one `CodecId::Pcm` bucket for every integer/float/A-law/
/// µ-law PCM variant, and no variant at all for ADPCM, AC-3 or WMA. This
/// mirrors `vaco-format-isom::stsd::sample_entry_codec`'s documented choice:
/// a tag with no representable `CodecId` maps to `None` rather than to a
/// guessed near-miss.
#[must_use]
pub fn codec_id(fmt: &WaveFormatEx) -> Option<CodecId> {
    let tag = fmt
        .extensible()
        .and_then(|e| e.sub_format_tag())
        .unwrap_or(fmt.format_tag);
    match tag {
        WAVE_FORMAT_PCM | WAVE_FORMAT_IEEE_FLOAT | WAVE_FORMAT_ALAW | WAVE_FORMAT_MULAW => {
            Some(CodecId::Pcm)
        }
        WAVE_FORMAT_MPEGLAYER3 => Some(CodecId::Mp3),
        WAVE_FORMAT_AAC => Some(CodecId::Aac),
        _ => None,
    }
}

/// The MS/RFC 2361 registered name for a `wFormatTag` value, independent of
/// what any particular decoder calls it.
///
/// Deliberately a small, high-confidence subset of the registry rather than
/// a full transcription of it — see the module docs.
#[must_use]
pub fn tag_description(tag: u16) -> Option<&'static str> {
    match tag {
        0x0000 => Some("WAVE_FORMAT_UNKNOWN"),
        WAVE_FORMAT_PCM => Some("WAVE_FORMAT_PCM"),
        WAVE_FORMAT_ADPCM => Some("WAVE_FORMAT_ADPCM (Microsoft)"),
        WAVE_FORMAT_IEEE_FLOAT => Some("WAVE_FORMAT_IEEE_FLOAT"),
        WAVE_FORMAT_ALAW => Some("WAVE_FORMAT_ALAW"),
        WAVE_FORMAT_MULAW => Some("WAVE_FORMAT_MULAW"),
        WAVE_FORMAT_DVI_ADPCM => Some("WAVE_FORMAT_DVI_ADPCM / WAVE_FORMAT_IMA_ADPCM"),
        0x0020 => Some("WAVE_FORMAT_YAMAHA_ADPCM"),
        0x0031 => Some("WAVE_FORMAT_GSM610"),
        WAVE_FORMAT_MPEG => Some("WAVE_FORMAT_MPEG"),
        WAVE_FORMAT_MPEGLAYER3 => Some("WAVE_FORMAT_MPEGLAYER3"),
        WAVE_FORMAT_WMAUDIO1 => Some("WAVE_FORMAT_WMAUDIO1"),
        WAVE_FORMAT_WMAUDIO2 => Some("WAVE_FORMAT_WMAUDIO2"),
        WAVE_FORMAT_DOLBY_AC3_SPDIF => Some("WAVE_FORMAT_DOLBY_AC3_SPDIF"),
        WAVE_FORMAT_AAC => Some("WAVE_FORMAT_AAC (unofficial, widely used)"),
        crate::wave::WAVE_FORMAT_EXTENSIBLE => Some("WAVE_FORMAT_EXTENSIBLE"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn fmt(format_tag: u16, bits_per_sample: u16) -> WaveFormatEx {
        WaveFormatEx {
            format_tag,
            channels: 1,
            samples_per_sec: 44_100,
            avg_bytes_per_sec: 0,
            block_align: 0,
            bits_per_sample,
            extra: Vec::new(),
        }
    }

    fn extensible_pcm(bits_per_sample: u16) -> WaveFormatEx {
        let mut extra = vec![0u8; 22];
        extra[0..2].copy_from_slice(&bits_per_sample.to_le_bytes());
        extra[2..6].copy_from_slice(&4u32.to_le_bytes());
        extra[6..22].copy_from_slice(&[
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]);
        WaveFormatEx {
            format_tag: crate::wave::WAVE_FORMAT_EXTENSIBLE,
            channels: 1,
            samples_per_sec: 44_100,
            avg_bytes_per_sec: 0,
            block_align: 0,
            bits_per_sample,
            extra,
        }
    }

    #[test]
    fn integer_pcm_names_by_bit_depth() {
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_PCM, 8)), Some("pcm_u8"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_PCM, 16)), Some("pcm_s16le"));
        // 24/32-bit PCM is written as WAVEFORMATEXTENSIBLE by ffmpeg; a
        // plain tag=1 fmt claiming 24/32 bits is not something ffmpeg's own
        // encoder produces, so this only asserts the extensible path.
        assert_eq!(codec_name(&extensible_pcm(24)), Some("pcm_s24le"));
        assert_eq!(codec_name(&extensible_pcm(32)), Some("pcm_s32le"));
    }

    #[test]
    fn float_pcm_names_by_bit_depth() {
        assert_eq!(
            codec_name(&fmt(WAVE_FORMAT_IEEE_FLOAT, 32)),
            Some("pcm_f32le")
        );
        assert_eq!(
            codec_name(&fmt(WAVE_FORMAT_IEEE_FLOAT, 64)),
            Some("pcm_f64le")
        );
    }

    #[test]
    fn compressed_tags_name_directly() {
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_ADPCM, 4)), Some("adpcm_ms"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_ALAW, 8)), Some("pcm_alaw"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_MULAW, 8)), Some("pcm_mulaw"));
        assert_eq!(
            codec_name(&fmt(WAVE_FORMAT_DVI_ADPCM, 4)),
            Some("adpcm_ima_wav")
        );
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_MPEG, 0)), Some("mp2"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_MPEGLAYER3, 0)), Some("mp3"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_WMAUDIO1, 16)), Some("wmav1"));
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_WMAUDIO2, 16)), Some("wmav2"));
        assert_eq!(
            codec_name(&fmt(WAVE_FORMAT_DOLBY_AC3_SPDIF, 16)),
            Some("ac3")
        );
        assert_eq!(codec_name(&fmt(WAVE_FORMAT_AAC, 0)), Some("aac"));
        assert_eq!(codec_name(&fmt(0x028f, 0)), Some("adpcm_g722"));
    }

    #[test]
    fn an_unrecognised_tag_is_none_not_a_guess() {
        assert_eq!(codec_name(&fmt(0x9999, 16)), None);
        assert_eq!(codec_id(&fmt(0x9999, 16)), None);
    }

    #[test]
    fn codec_id_only_covers_what_the_shared_enum_represents() {
        assert_eq!(codec_id(&fmt(WAVE_FORMAT_PCM, 16)), Some(CodecId::Pcm));
        assert_eq!(
            codec_id(&fmt(WAVE_FORMAT_MPEGLAYER3, 0)),
            Some(CodecId::Mp3)
        );
        assert_eq!(codec_id(&fmt(WAVE_FORMAT_AAC, 0)), Some(CodecId::Aac));
        // ADPCM and AC-3 have no CodecId variant yet.
        assert_eq!(codec_id(&fmt(WAVE_FORMAT_ADPCM, 4)), None);
        assert_eq!(codec_id(&fmt(WAVE_FORMAT_DOLBY_AC3_SPDIF, 16)), None);
    }
}
