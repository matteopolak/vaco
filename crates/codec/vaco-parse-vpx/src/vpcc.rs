//! MP4's `vpcC` — `VPCodecConfigurationRecord`, the `WebM` Project's ISOBMFF
//! binding for VP8/VP9 (`vp08`/`vp09` sample entries).
//!
//! # Measured layout, not assumed
//!
//! `vaco-format-isom` hands this crate the box's **payload** verbatim —
//! `data: b.payload` in `stsd.rs::find_config`, the same convention every
//! other `ConfigFlavour` uses — so this module reads the `FullBox` version
//! and flags itself rather than expecting them stripped. Confirmed by
//! encoding a real `libvpx-vp9` stream to MP4 and hex-dumping the box
//! (`ffmpeg -c:v libvpx-vp9 -profile:v 2 -pix_fmt yuv420p10le -level 4.0`):
//!
//! ```text
//! 00 00 00 14 76 70 63 43   size=20 "vpcC"
//! 01 00 00 00               version=1 flags=0
//! 02                        profile = 2
//! 0a                        level = 10 (level 1.0, ×10 encoding)
//! a2                        bitDepth=10 (0xa) chromaSubsampling=1 fullRange=0
//! 02 02 02                  colourPrimaries/transfer/matrix = 2 (unspecified)
//! 00 00                     codecIntializationDataSize = 0
//! ```
//!
//! which matches the `WebM` Project's own published `VPCodecConfigurationBox`
//! layout field for field, so this module implements that layout directly
//! rather than re-deriving it from more probes.
//!
//! # Why this exists alongside the in-band `color_config()` reader
//!
//! `WebM` carries no configuration record for VP8/VP9 at all — every field
//! comes from [`crate::vp9::parse_display_header`] instead — but MP4's
//! `vpcC` states `profile`/`level`/`bitDepth`/`chromaSubsampling` **and**
//! full ITU-T H.273 `colourPrimaries`/`transferCharacteristics`/
//! `matrixCoefficients` code points directly, which the bitstream itself
//! never carries (see `crate::vp9`'s module doc: VP9's own `color_space` is a
//! VP9-specific 3-bit enumeration, not an H.273 code point). So a `vpcC`
//! record is strictly more informative than the frame header for colour, and
//! [`codec_parameters`] passes those three fields through directly rather
//! than going through [`crate::vp9::matrix_coefficients`]'s table at all.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters, Level};
use vaco_color::{ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic};

use crate::profile;

/// One `vpcC` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VpCodecConfigurationRecord {
    pub profile: u8,
    /// Raw `level_idc`-style byte, level × 10. See
    /// [`crate::profile::level_from_vpcc`].
    pub level: u8,
    pub bit_depth: u8,
    /// 0..=7, the `WebM` Project's `chromaSubsampling` enumeration (0/1 = 4:2:0,
    /// 2 = 4:2:2, 3 = 4:4:4; 4..=7 are reserved).
    pub chroma_subsampling: u8,
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}

/// Parse a `vpcC` box payload (including its `FullBox` version/flags — see
/// the module doc for why). Returns `None` for anything too short to hold
/// the fixed-size fields; the trailing `codecIntializationData` is not read,
/// since VP8/VP9 never populate it.
#[must_use]
pub fn parse(data: &[u8]) -> Option<VpCodecConfigurationRecord> {
    let mut r = ByteReader::new(data);
    let _version = r.u8();
    let _flags = r.be24();
    let profile = r.u8();
    let level = r.u8();
    let packed = r.u8();
    let colour_primaries = r.u8();
    let transfer_characteristics = r.u8();
    let matrix_coefficients = r.u8();
    if r.overrun() {
        return None;
    }
    Some(VpCodecConfigurationRecord {
        profile,
        level,
        bit_depth: packed >> 4,
        chroma_subsampling: (packed >> 1) & 0x7,
        full_range: packed & 1 != 0,
        colour_primaries,
        transfer_characteristics,
        matrix_coefficients,
    })
}

/// The chroma subsampling a `vpcC` record's 3-bit field denotes, as
/// `(subsampling_x, subsampling_y)`. `WebM` Project's `VPCodecConfigurationBox`
/// spec: 0 = 4:2:0 vertical, 1 = 4:2:0 co-located, 2 = 4:2:2, 3 = 4:4:4;
/// unassigned values fall back to 4:2:0, the most common case, rather than
/// guessing at 4:4:4.
#[must_use]
pub const fn subsampling(chroma_subsampling: u8) -> (bool, bool) {
    match chroma_subsampling {
        2 => (true, false),
        3 => (false, false),
        _ => (true, true),
    }
}

/// The [`CodecParameters`] a `vpcC` payload describes, or `None` if it does
/// not even parse as one.
#[must_use]
pub fn codec_parameters(data: &[u8]) -> Option<CodecParameters> {
    let rec = parse(data)?;
    let mut params = CodecParameters::video().with_codec(CodecId::Vp9);
    params.profile = Some(profile::profile(rec.profile));
    params.level = Some(Level(i32::from(rec.level)));
    let (sx, sy) = subsampling(rec.chroma_subsampling);
    if let Some(v) = params.video.as_mut() {
        v.bits_per_raw_sample = Some(rec.bit_depth);
        v.format = crate::vp9::pixel_format(&crate::vp9::Vp9ColorConfig {
            bit_depth: rec.bit_depth,
            // `vpcC` has no VP9 `color_space` field of its own — it states
            // colour information directly as H.273 code points instead — so
            // this is only used to pick RGB vs. YUV, via the identity-matrix
            // convention `crate::vp9::pixel_format` already checks for.
            color_space: if rec.matrix_coefficients == 0 { 7 } else { 1 },
            full_range: rec.full_range,
            subsampling_x: sx,
            subsampling_y: sy,
        });
        v.color = ColorInfo {
            primaries: ColorPrimaries::from_u8(rec.colour_primaries).unwrap_or_default(),
            transfer: TransferCharacteristic::from_u8(rec.transfer_characteristics)
                .unwrap_or_default(),
            matrix: MatrixCoefficients::from_u8(rec.matrix_coefficients).unwrap_or_default(),
            range: if rec.full_range {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
            ..ColorInfo::default()
        };
    }
    Some(params)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// The exact bytes measured in the module doc: `libvpx-vp9`, profile 2,
    /// `yuv420p10le`, `-level 4.0` (which the encoder wrote as level 1.0).
    const MEASURED: [u8; 12] = [
        0x01, 0x00, 0x00, 0x00, // version=1, flags=0
        0x02, // profile
        0x0a, // level
        0xa2, // bitDepth=10, chromaSubsampling=1, fullRange=0
        0x02, 0x02, 0x02, // primaries/transfer/matrix
        0x00, 0x00, // codecIntializationDataSize
    ];

    #[test]
    fn the_measured_record_decodes_field_for_field() {
        let rec = parse(&MEASURED).unwrap();
        assert_eq!(rec.profile, 2);
        assert_eq!(rec.level, 10);
        assert_eq!(rec.bit_depth, 10);
        assert_eq!(rec.chroma_subsampling, 1);
        assert!(!rec.full_range);
        assert_eq!(
            (rec.colour_primaries, rec.transfer_characteristics, rec.matrix_coefficients),
            (2, 2, 2)
        );
    }

    #[test]
    fn codec_parameters_reports_profile_level_and_pix_fmt() {
        let params = codec_parameters(&MEASURED).unwrap();
        assert_eq!(params.profile.map(|p| p.value), Some(2));
        assert_eq!(params.level, Some(Level(10)));
        let v = params.video.unwrap();
        assert_eq!(v.bits_per_raw_sample, Some(10));
        assert_eq!(v.format, vaco_pixfmt::PixFmt::from_name("yuv420p10le").ok());
    }

    #[test]
    fn a_truncated_record_is_rejected_not_panicked() {
        assert!(parse(&[]).is_none());
        assert!(parse(&MEASURED[..5]).is_none());
    }
}
