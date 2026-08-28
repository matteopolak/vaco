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
/// This table must name everything [`codec_name`] names *and* the shared enum
/// can represent, because `vaco-probe` prints `codec_name` from the
/// `CodecId`, not from this crate's string. A `FourCC` that has a spelling
/// here but no id prints `unknown` — which is how `FMP4` came to probe as
/// `codec_name=unknown` while this very file knew it was `mpeg4`
/// (CONFORMANCE-FINDINGS 24).
///
/// The doc comment that used to sit here said MPEG-4, MS-MPEG4, Huffyuv and
/// raw video had no variant in the shared enum. That was true when it was
/// written and had stopped being true by the time anyone read it — the
/// hazard `AGENT-CONSTRAINTS.md` calls "never pin the absence of something
/// the project is building". Only `msmpeg4v2`, `cinepak`, `msvideo1`, `wmv1`
/// and `wmv2` are still genuinely unrepresentable.
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
        b"FMP4" | b"XVID" | b"DIVX" | b"DX50" | b"mp4v" => Some(CodecId::Mpeg4),
        b"MP43" | b"DIV3" => Some(CodecId::Msmpeg4v3),
        b"FFV1" => Some(CodecId::Ffv1),
        b"HFYU" => Some(CodecId::Huffyuv),
        b"I420" | b"IYUV" => Some(CodecId::Rawvideo),
        // `MP42`, `cvid`, `MSVC`/`CRAM`, `WMV1`, `WMV2` have a spelling in
        // `codec_name` and no variant in the shared enum. `None` rather than a
        // near-miss, and `codec_name` keeps the string so the gap is visible.
        _ => None,
    }
}

/// Whether a video `FourCC` follows the ISO-BMFF `avc1`/`hvc1` sample-entry
/// convention, where the bytes after the fixed header are an
/// `avcC`/`hvcC`-style configuration record — as opposed to `H264`/`HEVC`
/// and their aliases, which carry Annex B in-band and nothing to extract.
///
/// Measured on `avc1`: `ffmpeg -c copy -f avi` writes a 45-byte
/// `AVCDecoderConfigurationRecord` immediately after `BITMAPINFOHEADER`.
/// `hvc1`/`hev1` are included by the same ISO-BMFF convention, unconfirmed
/// against a fixture for lack of one.
#[must_use]
pub fn carries_config_record(compression: Compression) -> bool {
    let Compression::FourCc(id) = compression else {
        return false;
    };
    matches!(&id.as_bytes(), b"avc1" | b"AVC1" | b"hvc1" | b"hev1")
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
    fn only_the_isobmff_style_fourccs_carry_a_config_record() {
        assert!(carries_config_record(fourcc(*b"avc1")));
        assert!(carries_config_record(fourcc(*b"AVC1")));
        assert!(carries_config_record(fourcc(*b"hvc1")));
        assert!(carries_config_record(fourcc(*b"hev1")));
        assert!(!carries_config_record(fourcc(*b"H264")));
        assert!(!carries_config_record(fourcc(*b"X264")));
        assert!(!carries_config_record(fourcc(*b"HEVC")));
        assert!(!carries_config_record(Compression::Rgb));
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
        assert_eq!(codec_id(fourcc(*b"FMP4")), Some(CodecId::Mpeg4));
        assert_eq!(codec_id(fourcc(*b"HFYU")), Some(CodecId::Huffyuv));
        // Still genuinely unrepresentable — no variant exists for these.
        assert_eq!(codec_id(fourcc(*b"MP42")), None);
        assert_eq!(codec_id(fourcc(*b"cvid")), None);
        assert_eq!(codec_id(fourcc(*b"WMV1")), None);
    }

    /// Every `FourCC` with a spelling and a representable codec has an id.
    ///
    /// `vaco-probe` prints `codec_name` from the `CodecId`, so a `FourCC` in
    /// one table and not the other probes as `unknown` while this crate is
    /// holding the right answer. The list below is the set that has no variant
    /// in the shared enum; anything else that regresses fails here.
    #[test]
    fn the_two_tables_disagree_only_where_the_enum_cannot_follow() {
        const NO_VARIANT: &[&[u8; 4]] = &[b"MP42", b"cvid", b"MSVC", b"CRAM", b"WMV1", b"WMV2"];
        for cc in [
            &b"H264"[..],
            b"X264",
            b"x264",
            b"avc1",
            b"AVC1",
            b"hvc1",
            b"hev1",
            b"HEVC",
            b"H265",
            b"VP80",
            b"VP90",
            b"MJPG",
            b"mjpg",
            b"FFV1",
            b"HFYU",
            b"FMP4",
            b"XVID",
            b"DIVX",
            b"DX50",
            b"mp4v",
            b"MP42",
            b"MP43",
            b"DIV3",
            b"MPNG",
            b"I420",
            b"IYUV",
            b"cvid",
            b"MSVC",
            b"CRAM",
            b"WMV1",
            b"WMV2",
        ] {
            let Ok(bytes) = <[u8; 4]>::try_from(cc) else {
                unreachable!("every literal above is four bytes")
            };
            let c = fourcc(bytes);
            let name = codec_name(c);
            assert!(name.is_some(), "{:?} has no spelling", core::str::from_utf8(cc));
            let expected_none = NO_VARIANT.iter().any(|n| n.as_slice() == cc);
            assert_eq!(
                codec_id(c).is_none(),
                expected_none,
                "{:?}: codec_name={name:?} but codec_id={:?}",
                core::str::from_utf8(cc),
                codec_id(c)
            );
        }
    }
}
