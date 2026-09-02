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
use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};

use crate::profile;
use crate::vp9::Vp9Header;

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

/// Serialise a [`VpCodecConfigurationRecord`] back into a `vpcC` box
/// **payload** (`FullBox` version/flags included, matching [`parse`]'s own
/// convention of reading them rather than expecting them stripped — see the
/// module doc's hex dump). Always 12 bytes: this crate never writes a
/// non-empty `codecIntializationData`, matching every real `vpcC` this
/// module doc's own measurement and [`from_vp9_header`]'s own doc found —
/// VP8/VP9 never populate that field.
#[must_use]
pub fn build(rec: &VpCodecConfigurationRecord) -> [u8; 12] {
    let packed = (rec.bit_depth << 4) | (rec.chroma_subsampling << 1) | u8::from(rec.full_range);
    [
        1, // version
        0,
        0,
        0, // flags
        rec.profile,
        rec.level,
        packed,
        rec.colour_primaries,
        rec.transfer_characteristics,
        rec.matrix_coefficients,
        0,
        0, // codecIntializationDataSize = 0
    ]
}

/// The `vpcC` `chromaSubsampling` code point for `(subsampling_x,
/// subsampling_y)`, the inverse of [`subsampling`]. `(true, true)` (4:2:0)
/// picks `1` ("co-located") rather than `0` ("vertical"), matching what a
/// real `libvpx-vp9` encode's own `vpcC` states for the same input — VP9's
/// `color_config()` never actually distinguishes the two chroma-siting
/// conventions itself (see [`crate::vp9`]'s module doc), so `1` is the only
/// value this crate has ever observed a real encoder choose.
#[must_use]
const fn chroma_subsampling_code(subsampling_x: bool, subsampling_y: bool) -> u8 {
    match (subsampling_x, subsampling_y) {
        (true, false) => 2,
        (false, false) => 3,
        _ => 1,
    }
}

/// Derive a [`VpCodecConfigurationRecord`] from a VP9 frame header, for a
/// container (`WebM`/Matroska) that carries no `vpcC`-shaped `CodecPrivate`
/// of its own — the gap `vaco-mux-mp4`'s own module doc names: `extradata`
/// is muxed **verbatim**, never inspected, so a stream arriving with none
/// needs one derived upstream, through the same `BsfProvider` seam
/// `extract_extradata` already uses for H.264/HEVC (`vaco-bsf-vpx`'s
/// `vp9_extract_vpcc` is the caller).
///
/// Returns `None` when `header` carries no [`crate::vp9::Vp9ColorConfig`] —
/// an ordinary inter frame, which §6.2 defines to carry none at all (see
/// [`crate::vp9`]'s module doc) — so a caller must keep offering later
/// frames until a key frame (or an intra-only frame past profile 0) answers
/// `Some`, exactly as a real encoder's own first frame always is.
///
/// # What is fabricated, and why that is still honest
///
/// - `level`: VP9's bitstream states no level syntax anywhere (measured
///   against `ffprobe 8.1`, see [`crate::profile`]'s module doc) — `0`,
///   the `WebM` Project's own documented "Level, unknown or unspecified"
///   value for this field, not a guess at a real one.
/// - `colour_primaries`/`transfer_characteristics`: VP9's `color_config()`
///   states only a combined `color_space` enum (§6.2), which this crate's
///   own [`crate::vp9::matrix_coefficients`] already documents as mapping
///   cleanly to a matrix coefficient and *not* to primaries or transfer
///   individually — so both are written as H.273 code point 2
///   ("Unspecified"), matching what a real `libvpx-vp9` stream's own `vpcC`
///   states for the identical input (measured: encoding the same clip with
///   `-colorspace bt709` changes only the matrix byte of a real `ffmpeg -c
///   copy` remux's `vpcC`, never the primaries/transfer bytes, which stay
///   `02 02` throughout).
///
/// Everything else — `profile`, `bit_depth`, `chroma_subsampling`,
/// `full_range`, and `matrix_coefficients` via
/// [`crate::vp9::matrix_coefficients`] — comes straight off the bitstream,
/// not fabricated at all.
#[must_use]
pub fn from_vp9_header(header: &Vp9Header) -> Option<VpCodecConfigurationRecord> {
    let color = header.color?;
    Some(VpCodecConfigurationRecord {
        profile: header.profile,
        level: 0, // "Level is unspecified" per the WebM Project's own vpcC spec.
        bit_depth: color.bit_depth,
        chroma_subsampling: chroma_subsampling_code(color.subsampling_x, color.subsampling_y),
        full_range: color.full_range,
        colour_primaries: 2,   // Unspecified — see this function's own doc.
        transfer_characteristics: 2, // Unspecified — see this function's own doc.
        matrix_coefficients: crate::vp9::matrix_coefficients(color.color_space).to_u8(),
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
        // `bitDepth` is only the top 4 bits of a packed byte (0..=15), so it
        // can never trip the "1..=64" sanity range a probe checks — but VP9
        // itself only ever codes 8, 10 or 12 bits (`crate::vp9::color_config`),
        // so an out-of-band nibble like 0 is still a fabricated value, just
        // one narrow enough that fuzzing had not reached it through this
        // record. Same class of bug as JPEG's unchecked `precision`
        // (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0).
        v.bits_per_raw_sample = matches!(rec.bit_depth, 8 | 10 | 12).then_some(rec.bit_depth);
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
            (
                rec.colour_primaries,
                rec.transfer_characteristics,
                rec.matrix_coefficients
            ),
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

    /// `bitDepth` is only the top nibble of a packed byte (0..=15), so it can
    /// never trip a probe's outer "1..=64" sanity check — but VP9 only ever
    /// codes 8/10/12-bit samples, so an out-of-band nibble like 0 is still a
    /// fabricated value, the same class of bug
    /// `fuzz/fuzz_targets/registry_discovery.rs` found in JPEG's `precision`
    /// (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0), just one the corpus
    /// had not reached through this record.
    #[test]
    fn an_implausible_bit_depth_nibble_is_not_reported() {
        let mut data = MEASURED;
        data[6] = 0x02; // bitDepth nibble = 0, chromaSubsampling=1, fullRange=0
        let rec = parse(&data).unwrap();
        assert_eq!(rec.bit_depth, 0, "the raw nibble is preserved");
        let params = codec_parameters(&data).unwrap();
        let v = params.video.unwrap();
        assert_eq!(v.bits_per_raw_sample, None);
    }

    #[test]
    fn build_is_the_exact_inverse_of_parse_on_the_measured_record() {
        let rec = parse(&MEASURED).unwrap();
        assert_eq!(build(&rec), MEASURED);
    }

    /// The exact case this function exists for: a real `libvpx-vp9` key
    /// frame from a `WebM` file with no `CodecPrivate` at all. Bytes taken
    /// directly off the wire (`ffmpeg -f lavfi -i testsrc2 ... -c:v
    /// libvpx-vp9 -b:v 200k out.webm`, first frame, `uncompressed_header()`
    /// through `color_config()`): profile 0, 8-bit, 4:2:0, limited range,
    /// `color_space` unspecified (0).
    #[test]
    fn from_vp9_header_matches_a_real_key_frame_measured_against_ffmpeg() {
        use crate::vp9::Vp9ColorConfig;

        let header = Vp9Header {
            profile: 0,
            show_existing_frame: false,
            is_key_frame: true,
            show_frame: true,
            color: Some(Vp9ColorConfig {
                bit_depth: 8,
                color_space: 0, // CS_UNKNOWN
                full_range: false,
                subsampling_x: true,
                subsampling_y: true,
            }),
            size: Some((64, 64)),
        };
        let rec = from_vp9_header(&header).unwrap();
        assert_eq!(rec.profile, 0);
        assert_eq!(rec.bit_depth, 8);
        assert_eq!(rec.chroma_subsampling, 1);
        assert!(!rec.full_range);
        assert_eq!(rec.colour_primaries, 2);
        assert_eq!(rec.transfer_characteristics, 2);
        // Real `ffmpeg -c copy` of this exact clip writes `82 02 02 02` for
        // the packed byte plus the three colour bytes — see the module doc.
        assert_eq!(
            build(&rec)[6..10],
            [0x82, 0x02, 0x02, 0x02],
            "must match ffmpeg's own measured vpcC bytes for this input"
        );
    }

    /// The other half of the same measurement: `-colorspace bt709` changes
    /// only the matrix byte of a real remux's `vpcC`, never
    /// primaries/transfer, which stay `02 02`. `color_space = 2` is VP9's
    /// `CS_BT_709` (`crate::vp9::matrix_coefficients`'s own table).
    #[test]
    fn from_vp9_header_derives_only_the_matrix_byte_from_color_space() {
        use crate::vp9::Vp9ColorConfig;

        let header = Vp9Header {
            profile: 0,
            show_existing_frame: false,
            is_key_frame: true,
            show_frame: true,
            color: Some(Vp9ColorConfig {
                bit_depth: 8,
                color_space: 2, // CS_BT_709
                full_range: false,
                subsampling_x: true,
                subsampling_y: true,
            }),
            size: Some((64, 64)),
        };
        let rec = from_vp9_header(&header).unwrap();
        assert_eq!(
            (rec.colour_primaries, rec.transfer_characteristics, rec.matrix_coefficients),
            (2, 2, 1),
            "matrix=1 (BT.709) per ffmpeg's own measured vpcC; primaries/transfer stay unspecified"
        );
    }

    /// An ordinary inter frame carries no `color_config()` at all (§6.2) —
    /// a caller must keep waiting for a frame that does, not fabricate one.
    #[test]
    fn from_vp9_header_refuses_a_frame_with_no_color_config() {
        let header = Vp9Header {
            profile: 0,
            show_existing_frame: false,
            is_key_frame: false,
            show_frame: true,
            color: None,
            size: None,
        };
        assert!(from_vp9_header(&header).is_none());
    }
}
