//! Codec identity for ASF's two standard media types.
//!
//! [\[ASF\] §9.1](crate) states the Audio Media Type's `Type-Specific Data` is
//! `WAVEFORMATEX`, byte-for-byte, and [\[ASF\] §9.2](crate) states the Video
//! Media Type's is `EncodedImageWidth/Height` + `ReservedFlags` +
//! `FormatDataSize` followed by a `BITMAPINFOHEADER`. Both structures already
//! have one home in the workspace — `vaco-format-riff`, a format crate and
//! therefore fair to depend on under D14.1 — so this module is a thin bridge
//! rather than a second parser: it reuses `vaco_format_riff::wave` and
//! `vaco_format_riff::bitmapinfo` for parsing and their `*_tags` modules for
//! the base codec identity, adding only the handful of codec-tag mappings
//! those modules do not carry because they are ASF/WMV-specific (`vc1`'s two
//! `FourCCs`, and Windows Media Audio's three format tags, which `CodecId` has
//! variants for that `vaco-format-riff::wave_tags::codec_id` does not use —
//! see that function's doc comment for why: it was written before
//! `Wmav1`/`Wmav2`/`Wmapro` existed and nothing has revisited it since).

use vaco_codec_core::CodecId;
use vaco_format_riff::bitmapinfo::Compression;
use vaco_format_riff::chunk::ChunkId;
use vaco_format_riff::wave::{WAVE_FORMAT_WMAUDIO1, WAVE_FORMAT_WMAUDIO2, WaveFormatEx};
use vaco_format_riff::{video_tags, wave_tags};

/// `WAVE_FORMAT_WMAUDIO3` (WMA 9/10 Professional). Not in
/// `vaco-format-riff::wave`, which only names the two tags its own tests
/// probe; ASF audio streams are the primary place this tag appears.
pub const WAVE_FORMAT_WMAUDIO3: u16 = 0x0162;
/// `WAVE_FORMAT_WMAUDIO_LOSSLESS` (WMA 9/10 Lossless). No [`CodecId`] variant
/// exists for it, so [`audio_codec_id`] maps it to `None`; kept here so the
/// value is named rather than a bare literal if a caller wants
/// [`audio_codec_name`]'s string form.
pub const WAVE_FORMAT_WMA_LOSSLESS: u16 = 0x0163;

/// The [`CodecId`] for an ASF audio stream's `WAVEFORMATEX`.
///
/// Tries `vaco-format-riff`'s general table first (PCM, MP3, AAC, …), then
/// the Windows Media Audio tags that table does not resolve to a `CodecId`.
#[must_use]
pub fn audio_codec_id(fmt: &WaveFormatEx) -> Option<CodecId> {
    if let Some(id) = wave_tags::codec_id(fmt) {
        return Some(id);
    }
    match fmt.format_tag {
        WAVE_FORMAT_WMAUDIO1 => Some(CodecId::Wmav1),
        WAVE_FORMAT_WMAUDIO2 => Some(CodecId::Wmav2),
        WAVE_FORMAT_WMAUDIO3 => Some(CodecId::Wmapro),
        // WMA Lossless has no CodecId variant; `None` rather than a
        // near-miss, per the same discipline `wave_tags` documents.
        _ => None,
    }
}

/// The `ffprobe`-style short name for an ASF audio stream's format tag, for
/// diagnostics or metadata that wants a string rather than a `CodecId`.
#[must_use]
pub fn audio_codec_name(fmt: &WaveFormatEx) -> Option<&'static str> {
    if let Some(name) = wave_tags::codec_name(fmt) {
        return Some(name);
    }
    match fmt.format_tag {
        WAVE_FORMAT_WMAUDIO1 => Some("wmav1"),
        WAVE_FORMAT_WMAUDIO2 => Some("wmav2"),
        WAVE_FORMAT_WMAUDIO3 => Some("wmapro"),
        WAVE_FORMAT_WMA_LOSSLESS => Some("wmalossless"),
        _ => None,
    }
}

/// The [`CodecId`] for an ASF video stream's `biCompression` value.
///
/// Tries `vaco-format-riff`'s general `FourCC` table first, then VC-1's two
/// `FourCCs` (`WMV3` for the simple/main profile, `WVC1` for the advanced
/// profile) — both decode through the single [`CodecId::Vc1`] variant, the
/// same one-codec-many-tags convention `vaco-format-riff::video_tags` already
/// uses for `mpeg4`'s several vendor `FourCCs`.
#[must_use]
pub fn video_codec_id(compression: Compression) -> Option<CodecId> {
    if let Some(id) = video_tags::codec_id(compression) {
        return Some(id);
    }
    let Compression::FourCc(id) = compression else {
        return None;
    };
    match &id.as_bytes() {
        b"WMV3" | b"WVC1" => Some(CodecId::Vc1),
        _ => None,
    }
}

/// The `ffprobe`-style short name for an ASF video stream's `biCompression`
/// `FourCC`.
#[must_use]
pub fn video_codec_name(compression: Compression) -> Option<&'static str> {
    if let Some(name) = video_tags::codec_name(compression) {
        return Some(name);
    }
    let Compression::FourCc(id) = compression else {
        return None;
    };
    match &id.as_bytes() {
        b"WMV3" => Some("wmv3"),
        b"WVC1" => Some("vc1"),
        _ => None,
    }
}

/// Convenience: build a [`Compression::FourCc`] from four raw bytes, for
/// callers that already have `biCompression`'s raw `u32` (as this crate's own
/// video-stream parsing does) without going through
/// [`vaco_format_riff::bitmapinfo::Compression::from_u32`]'s reserved-value
/// handling, which video type-specific data never needs (ASF's `Compression
/// ID` field description gives no equivalent to `BI_RGB`/`BI_RLE8`; every
/// value in practice is a `FourCC`).
#[must_use]
pub fn fourcc(bytes: [u8; 4]) -> Compression {
    Compression::FourCc(ChunkId::new(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wfx(format_tag: u16) -> WaveFormatEx {
        WaveFormatEx {
            format_tag,
            channels: 2,
            samples_per_sec: 44_100,
            avg_bytes_per_sec: 0,
            block_align: 0,
            bits_per_sample: 16,
            extra: Vec::new(),
        }
    }

    #[test]
    fn wma_tags_resolve_through_the_supplemental_table() {
        assert_eq!(
            audio_codec_id(&wfx(WAVE_FORMAT_WMAUDIO1)),
            Some(CodecId::Wmav1)
        );
        assert_eq!(
            audio_codec_id(&wfx(WAVE_FORMAT_WMAUDIO2)),
            Some(CodecId::Wmav2)
        );
        assert_eq!(
            audio_codec_id(&wfx(WAVE_FORMAT_WMAUDIO3)),
            Some(CodecId::Wmapro)
        );
        assert_eq!(audio_codec_id(&wfx(WAVE_FORMAT_WMA_LOSSLESS)), None);
        assert_eq!(
            audio_codec_name(&wfx(WAVE_FORMAT_WMA_LOSSLESS)),
            Some("wmalossless")
        );
    }

    #[test]
    fn pcm_still_resolves_through_the_shared_riff_table() {
        assert_eq!(
            audio_codec_id(&wfx(vaco_format_riff::wave::WAVE_FORMAT_PCM)),
            Some(CodecId::PcmS16le)
        );
    }

    #[test]
    fn vc1_fourccs_resolve_through_the_supplemental_table() {
        assert_eq!(video_codec_id(fourcc(*b"WMV3")), Some(CodecId::Vc1));
        assert_eq!(video_codec_id(fourcc(*b"WVC1")), Some(CodecId::Vc1));
        assert_eq!(video_codec_name(fourcc(*b"WMV3")), Some("wmv3"));
        assert_eq!(video_codec_name(fourcc(*b"WVC1")), Some("vc1"));
    }

    #[test]
    fn an_unknown_fourcc_is_none_not_a_guess() {
        assert_eq!(video_codec_id(fourcc(*b"ZZZZ")), None);
    }
}
