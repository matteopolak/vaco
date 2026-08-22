//! `biCompression` → codec identity, for the AVI video `FourCC` family.
//!
//! Same split as [`crate::wave_tags`]: [`codec_name`] is only ever a spelling
//! this crate's author reproduced against `ffprobe` 8.1, with the exact
//! command recorded next to the entry. There is no separate "description"
//! table here the way there is for WAVE tags — a video `FourCC` is, unlike a
//! WAVE format tag, *always* either one of the four `wingdi.h` integer
//! constants (handled by [`crate::bitmapinfo::Compression`] directly) or a
//! vendor-assigned four-character code with no independent registry to draw
//! a structural-fact table from, so there is nothing to put in an
//! unverified column.
//!
//! Probed with `ffmpeg -f lavfi -i testsrc=... -c:v <encoder> [-tag:v
//! <fourcc>] -pix_fmt yuv420p out.avi`, then `ffprobe -show_entries
//! stream=codec_name,codec_tag_string`.

use vaco_codec_core::CodecId;

use crate::bitmapinfo::Compression;
use crate::chunk::ChunkId;

/// The `ffprobe` 8.1 `codec_name` for a `biCompression` value, or `None` for
/// a `FourCC` this crate has not verified a spelling for.
#[must_use]
pub fn codec_name(compression: Compression) -> Option<&'static str> {
    match compression {
        Compression::Rgb | Compression::BitFields => Some("rawvideo"),
        // `ffmpeg`'s `msrle` encoder writes `BI_RLE8`/`BI_RLE4` with no
        // FourCC at all; both decode through the one `msrle` codec.
        Compression::Rle8 | Compression::Rle4 => Some("msrle"),
        Compression::FourCc(id) => fourcc_codec_name(id),
        Compression::Other(_) => None,
    }
}

fn fourcc_codec_name(id: ChunkId) -> Option<&'static str> {
    match &id.as_bytes() {
        b"H264" | b"X264" | b"x264" | b"avc1" | b"AVC1" => Some("h264"),
        b"hvc1" | b"hev1" | b"HEVC" | b"H265" => Some("hevc"),
        b"VP80" => Some("vp8"),
        b"VP90" => Some("vp9"),
        b"MJPG" | b"mjpg" => Some("mjpeg"),
        b"FFV1" => Some("ffv1"),
        b"HFYU" => Some("huffyuv"),
        // mpeg4 accepts several vendor FourCCs for the same bitstream;
        // `FMP4` is ffmpeg's own default, `XVID`/`DIVX` are the two other
        // encoders this crate confirmed round-trip to the same codec_name.
        b"FMP4" | b"XVID" | b"DIVX" | b"DX50" | b"mp4v" => Some("mpeg4"),
        b"MP42" => Some("msmpeg4v2"),
        // `MP43` and `DIV3` are two FourCCs ffmpeg's own msmpeg4v3 encoder
        // writes depending on `-tag:v`; both probed to the same codec_name.
        b"MP43" | b"DIV3" => Some("msmpeg4v3"),
        b"MPNG" => Some("png"),
        b"I420" | b"IYUV" => Some("rawvideo"),
        b"cvid" => Some("cinepak"),
        b"MSVC" | b"CRAM" => Some("msvideo1"),
        b"WMV1" => Some("wmv1"),
        b"WMV2" => Some("wmv2"),
        _ => None,
    }
}

/// A best-effort [`CodecId`] for a `biCompression` value.
///
/// As with [`crate::wave_tags::codec_id`], `CodecId` does not have a variant
/// for most video `FourCCs` this table names — no MPEG-4 part 2, MS-MPEG4,
/// WMV, Huffyuv, Cinepak or raw-video variant exists in the shared enum
/// today — so those map to `None` rather than a guessed near-miss.
#[must_use]
pub fn codec_id(compression: Compression) -> Option<CodecId> {
    let Compression::FourCc(id) = compression else {
        return None;
    };
    match &id.as_bytes() {
        b"H264" | b"X264" | b"x264" | b"avc1" | b"AVC1" => Some(CodecId::H264),
        b"hvc1" | b"hev1" | b"HEVC" | b"H265" => Some(CodecId::Hevc),
        b"VP80" => Some(CodecId::Vp8),
        b"VP90" => Some(CodecId::Vp9),
        b"MJPG" | b"mjpg" => Some(CodecId::Jpeg),
        b"MPNG" => Some(CodecId::Png),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fourcc(bytes: [u8; 4]) -> Compression {
        Compression::FourCc(ChunkId::new(&bytes))
    }

    #[test]
    fn reserved_integers_name_rawvideo_and_msrle() {
        assert_eq!(codec_name(Compression::Rgb), Some("rawvideo"));
        assert_eq!(codec_name(Compression::BitFields), Some("rawvideo"));
        assert_eq!(codec_name(Compression::Rle8), Some("msrle"));
        assert_eq!(codec_name(Compression::Rle4), Some("msrle"));
    }

    #[test]
    fn probed_fourccs_name_their_codec() {
        assert_eq!(codec_name(fourcc(*b"H264")), Some("h264"));
        assert_eq!(codec_name(fourcc(*b"X264")), Some("h264"));
        assert_eq!(codec_name(fourcc(*b"hvc1")), Some("hevc"));
        assert_eq!(codec_name(fourcc(*b"VP80")), Some("vp8"));
        assert_eq!(codec_name(fourcc(*b"VP90")), Some("vp9"));
        assert_eq!(codec_name(fourcc(*b"MJPG")), Some("mjpeg"));
        assert_eq!(codec_name(fourcc(*b"FFV1")), Some("ffv1"));
        assert_eq!(codec_name(fourcc(*b"HFYU")), Some("huffyuv"));
        assert_eq!(codec_name(fourcc(*b"FMP4")), Some("mpeg4"));
        assert_eq!(codec_name(fourcc(*b"XVID")), Some("mpeg4"));
        assert_eq!(codec_name(fourcc(*b"DIVX")), Some("mpeg4"));
        assert_eq!(codec_name(fourcc(*b"MP42")), Some("msmpeg4v2"));
        assert_eq!(codec_name(fourcc(*b"MP43")), Some("msmpeg4v3"));
        assert_eq!(codec_name(fourcc(*b"DIV3")), Some("msmpeg4v3"));
        assert_eq!(codec_name(fourcc(*b"MPNG")), Some("png"));
        assert_eq!(codec_name(fourcc(*b"I420")), Some("rawvideo"));
        assert_eq!(codec_name(fourcc(*b"cvid")), Some("cinepak"));
        assert_eq!(codec_name(fourcc(*b"MSVC")), Some("msvideo1"));
        assert_eq!(codec_name(fourcc(*b"WMV1")), Some("wmv1"));
        assert_eq!(codec_name(fourcc(*b"WMV2")), Some("wmv2"));
    }

    #[test]
    fn an_unrecognised_fourcc_is_none_not_a_guess() {
        assert_eq!(codec_name(fourcc(*b"ZZZZ")), None);
        assert_eq!(codec_id(fourcc(*b"ZZZZ")), None);
    }

    #[test]
    fn codec_id_only_covers_what_the_shared_enum_represents() {
        assert_eq!(codec_id(fourcc(*b"H264")), Some(CodecId::H264));
        assert_eq!(codec_id(fourcc(*b"hvc1")), Some(CodecId::Hevc));
        // mpeg4/msmpeg4/huffyuv/ffv1/cinepak/wmv have no CodecId variant yet.
        assert_eq!(codec_id(fourcc(*b"FMP4")), None);
        assert_eq!(codec_id(fourcc(*b"HFYU")), None);
    }
}
