//! The [`CodecParameters`] a sequence header implies, and the pixel-format
//! mapping it depends on.

use vaco_codec_core::{CodecId, CodecParameters, FieldOrder, Level};
use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_pixfmt::PixFmt;

use crate::profile::profile;
use crate::seq::{ColorConfig, SequenceHeader};

/// The pixel format a sequence header's `color_config()` implies.
///
/// # Measured, not assumed — AV1 does not have H.264's/HEVC's `yuvj` family
///
/// `// D17:` `ffmpeg 8.1`'s H.264 reader maps full-range 4:2:0/4:2:2/4:4:4 at 8
/// bits to `yuvj420p`/`yuvj422p`/`yuvj444p`; HEVC narrows that to 4:2:0 only
/// (`vaco-parse-hevc`'s `pixel_format`, itself measured). **AV1 has neither.**
/// Probed:
///
/// ```text
/// ffmpeg -f lavfi -i testsrc -color_range pc -pix_fmt yuv420p -c:v libsvtav1 full.mp4
/// ffprobe -show_entries stream=pix_fmt,color_range full.mp4
/// # pix_fmt=yuv420p  color_range=pc
/// ```
///
/// Full range stays `yuv420p` with `color_range` reported separately. This is
/// exactly the "treat any AV1-is-like-H.264-or-HEVC claim as a hypothesis"
/// warning the brief for this crate carries, borne out: three formats probed,
/// zero `yuvj` answers.
///
/// # Untested by measurement: monochrome, 4:2:2, 4:4:4, 12-bit
///
/// `libsvtav1` is the only encoder available in this environment (D10/D15's
/// royalty-free posture makes `libaom-av1` unavailable in the sandbox this
/// crate was built in — `ffmpeg -codecs` lists no `libaom-av1` encoder here)
/// and it accepts only `yuv420p`/`yuv420p10le` as input, silently converting
/// anything else — a `-pix_fmt gray` source was fed through and the stream it
/// produced still read `mono_chrome = 0`. So the `gray`/`gray10le`/`gray12le`
/// and 4:2:2/4:4:4 mappings below follow `ffmpeg`'s pixel-format naming
/// convention (the same names `vaco-pixfmt` already defines for every other
/// codec) rather than a black-box measurement, and are the one part of this
/// crate's `ffprobe` fidelity claim that is unverified. Flagged in the report.
#[must_use]
pub fn pixel_format(c: &ColorConfig) -> Option<PixFmt> {
    if c.mono_chrome {
        let name = match c.bit_depth {
            8 => "gray".to_string(),
            10 | 12 => format!("gray{}le", c.bit_depth),
            _ => return None,
        };
        return PixFmt::from_name(&name).ok();
    }
    let chroma = match (c.subsampling_x, c.subsampling_y) {
        (true, true) => "420",
        (true, false) => "422",
        // (false, false), and the invalid (false, true) the specification
        // never produces (subsampling_y can only be set when subsampling_x
        // is) — both fall through to 4:4:4, which is the closest honest
        // answer for the latter rather than reporting a format the stream
        // did not declare.
        _ => "444",
    };
    let name = match c.bit_depth {
        8 => format!("yuv{chroma}p"),
        10 | 12 => format!("yuv{chroma}p{}le", c.bit_depth),
        _ => return None,
    };
    PixFmt::from_name(&name).ok()
}

/// The [`ColorInfo`] a `color_config()` implies.
///
/// AV1's `color_primaries`/`transfer_characteristics`/`matrix_coefficients`
/// are H.273 code points read directly off the wire (§6.4.2), so this is a
/// narrowing cast plus the two-value range flag — nothing like H.264's or
/// HEVC's VUI defaulting rules, because AV1 has no equivalent of
/// `video_signal_type_present_flag`: the three code points are always
/// present, defaulting to code point 2 ("unspecified") in the syntax itself
/// when `color_description_present_flag` is 0.
#[must_use]
pub fn color_info(c: &ColorConfig) -> ColorInfo {
    ColorInfo {
        primaries: ColorPrimaries::from_u8(c.color_primaries).unwrap_or_default(),
        transfer: TransferCharacteristic::from_u8(c.transfer_characteristics).unwrap_or_default(),
        matrix: MatrixCoefficients::from_u8(c.matrix_coefficients).unwrap_or_default(),
        range: ColorRange::from_full_range_flag(c.color_range),
        // AV1 signals `chroma_sample_position` only when both subsampling
        // flags are set (§5.5.2), and its two low bits are defined to match
        // H.273's `CSP_UNKNOWN`/`CSP_VERTICAL`/`CSP_COLOCATED` values 0..2;
        // `CSP_RESERVED` (3) has no H.273 counterpart and maps to
        // `Unspecified` rather than a fabricated location.
        chroma_location: if c.subsampling_x && c.subsampling_y {
            match c.chroma_sample_position {
                1 => vaco_color::ChromaLocation::Left,
                2 => vaco_color::ChromaLocation::Center,
                // 0 is `CSP_UNKNOWN`; 3 is `CSP_RESERVED`, which has no H.273
                // counterpart. Both map to `Unspecified`.
                _ => vaco_color::ChromaLocation::Unspecified,
            }
        } else {
            vaco_color::ChromaLocation::Unspecified
        },
    }
}

/// The [`CodecParameters`] a sequence header describes, from its primary
/// operating point (§5.5.1's operating point 0 — see
/// [`SequenceHeader::primary_operating_point`]).
///
/// Resolution is `max_frame_width`/`max_frame_height` — the sequence header's
/// own coded size — because that is what every fixture this crate measured
/// reports through to `ffprobe` (`docs/codec/vaco-parse-av1.md`): ordinary
/// encoder output leaves `frame_size_override_flag` at 0, so no frame ever
/// codes a size other than the sequence header's. A caller that has also
/// parsed a keyframe's `frame_header_obu()` and found an override should
/// prefer that frame's [`crate::frame_header::FrameSize`] instead — this
/// function only has the sequence header to go on.
#[must_use]
pub fn codec_parameters(seq: &SequenceHeader) -> CodecParameters {
    let mut params = CodecParameters::video().with_codec(CodecId::Av1);
    params.profile = Some(profile(seq.seq_profile));
    if let Some(op) = seq.primary_operating_point() {
        params.level = Some(Level(i32::from(op.seq_level_idx)));
    }
    if let Some(v) = params.video.as_mut() {
        v.width = seq.max_frame_width;
        v.height = seq.max_frame_height;
        v.coded_width = seq.max_frame_width;
        v.coded_height = seq.max_frame_height;
        v.format = pixel_format(&seq.color_config);
        v.color = color_info(&seq.color_config);
        v.field_order = FieldOrder::Progressive; // AV1 has no interlace syntax at all (§7.1).
        if let Some(fr) = seq.frame_rate() {
            v.frame_rate = fr;
        }
    }
    params
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn real_seq() -> SequenceHeader {
        let payload = [
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ];
        let mut b = Budget::new(Limits::strict());
        SequenceHeader::parse(&payload, &mut b).unwrap()
    }

    #[test]
    fn a_real_sequence_header_matches_the_measured_ffprobe_output() {
        let seq = real_seq();
        let params = codec_parameters(&seq);
        let v = params.video.as_ref().unwrap();
        // Measured: width=642 height=358 coded_width=642 coded_height=358
        // pix_fmt=yuv420p profile=Main level=1 color_range=tv.
        assert_eq!((v.width, v.height), (642, 358));
        assert_eq!((v.coded_width, v.coded_height), (642, 358));
        assert_eq!(v.format, PixFmt::from_name("yuv420p").ok());
        assert_eq!(v.color.range, ColorRange::Limited);
        assert_eq!(params.profile.map(|p| p.name), Some("Main"));
        assert_eq!(params.level.map(vaco_codec_core::Level::raw), Some(1));
    }

    #[test]
    fn full_range_420_8bit_stays_plain_yuv420p() {
        // D17, measured against `ffmpeg 8.1`: no `yuvj420p` for AV1.
        let c = ColorConfig {
            bit_depth: 8,
            mono_chrome: false,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            color_range: true,
            subsampling_x: true,
            subsampling_y: true,
            chroma_sample_position: 0,
            separate_uv_delta_q: false,
        };
        assert_eq!(pixel_format(&c), PixFmt::from_name("yuv420p").ok());
    }

    #[test]
    fn monochrome_maps_to_the_gray_family() {
        let c = ColorConfig {
            bit_depth: 10,
            mono_chrome: true,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            color_range: false,
            subsampling_x: true,
            subsampling_y: true,
            chroma_sample_position: 0,
            separate_uv_delta_q: false,
        };
        assert_eq!(pixel_format(&c), PixFmt::from_name("gray10le").ok());
    }
}
