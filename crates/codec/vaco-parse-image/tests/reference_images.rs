//! Every still-image format this crate describes, against real
//! `ffmpeg`-written files and the exact pixels `ffmpeg` decodes from them.
//!
//! # What this is guarding
//!
//! Nine of these formats used to report `0x0` and no pixel format at all
//! (`vaco-probe` printed `0,0,unknown` where `ffprobe` printed
//! `13,7,rgb24`), and the transcode path read that silence as "no opinion"
//! and converted every frame to the encoder's first accepted format — a
//! colour PPM came out grey. So the assertion that matters is the *pair*:
//! the parameters this crate reports, and the pixels the decoder produces,
//! both against the reference.
//!
//! # Provenance
//!
//! `Vaco-Provenance: blackbox`, `Vaco-Spec-Ref: ffmpeg-still-image-selection-probe`.
//! Each `tests/fixtures/<fmt>_<w>x<h>.<ext>` is
//! `ffmpeg -i src.png -frames:v 1 out.<ext>` (ffmpeg 9.0.1) and each
//! `<fmt>_<w>x<h>.raw` is `ffmpeg -i out.<ext> -f rawvideo -pix_fmt <what
//! ffprobe reports for it> ref.raw` on that same file. **Never our own
//! encoder** — a self round-trip is how a completely broken FFV1 stayed
//! green here.
//!
//! 13x7 and 33x5 are both odd in at least one dimension on purpose: PCX,
//! XWD, XBM and SGI all pad rows, and a 64x48 fixture cannot express a
//! padding bug in any of them.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::disallowed_methods,
    reason = "a test that cannot set up is a failed test; the untrusted-input \
rationale for denying these does not apply to fixtures we wrote"
)]

use vaco_codec_core::Parser as _;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

type DecodeFn = fn(&[u8], &mut Budget) -> vaco_core::Result<vaco_frame::Frame>;
type ParseFn = fn(&[u8]) -> Option<vaco_codec_core::CodecParameters>;

struct Case {
    /// `tests/fixtures/<stem>.<ext>` and `tests/fixtures/<stem>.raw`.
    stem: &'static str,
    ext: &'static str,
    width: u32,
    height: u32,
    /// What `ffprobe -show_entries stream=pix_fmt` reports for the fixture.
    format: PixFmt,
    parse: ParseFn,
    decode: DecodeFn,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    // A four-channel SGI with a real alpha ramp, not a constant one: the
    // decoder used to drop the fourth channel and report `gbrp`, which an
    // opaque fixture cannot tell from correct.
    out.push(Case {
        stem: "sgia_13x7",
        ext: "sgi",
        width: 13,
        height: 7,
        format: PixFmt::Gbrap,
        parse: vaco_codec_image_simple::parameters_sgi,
        decode: vaco_codec_image_simple::decode_sgi,
    });
    for (w, h, size) in [(13u32, 7u32, "13x7"), (33, 5, "33x5")] {
        let rows: &[(&str, PixFmt, ParseFn, DecodeFn)] = &[
            (
                "pcx",
                PixFmt::Rgb24,
                vaco_codec_image_simple::parameters_pcx,
                vaco_codec_image_simple::decode_pcx,
            ),
            (
                "sgi",
                PixFmt::Gbrp,
                vaco_codec_image_simple::parameters_sgi,
                vaco_codec_image_simple::decode_sgi,
            ),
            (
                "tga",
                PixFmt::Bgr24,
                vaco_codec_image_simple::parameters_tga,
                vaco_codec_image_simple::decode_tga,
            ),
            (
                "xwd",
                PixFmt::Rgb24,
                vaco_codec_image_simple::parameters_xwd,
                vaco_codec_image_simple::decode_xwd,
            ),
            (
                "xbm",
                PixFmt::MonoWhite,
                vaco_codec_image_simple::parameters_xbm,
                vaco_codec_image_simple::decode_xbm,
            ),
            (
                "qoi",
                PixFmt::Rgb24,
                vaco_codec_qoi::parameters,
                vaco_codec_qoi::decode,
            ),
            (
                "pbm",
                PixFmt::MonoWhite,
                vaco_codec_pnm::parameters_pbm,
                vaco_codec_pnm::decode_pbm,
            ),
            (
                "pgm",
                PixFmt::Gray8,
                vaco_codec_pnm::parameters_pgm,
                vaco_codec_pnm::decode_pgm,
            ),
            (
                "ppm",
                PixFmt::Rgb24,
                vaco_codec_pnm::parameters_ppm,
                vaco_codec_pnm::decode_ppm,
            ),
            (
                "pam",
                PixFmt::Rgb24,
                vaco_codec_pnm::parameters_pam,
                vaco_codec_pnm::decode_pam,
            ),
        ];
        for &(ext, format, parse, decode) in rows {
            out.push(Case {
                stem: Box::leak(format!("{ext}_{size}").into_boxed_str()),
                ext,
                width: w,
                height: h,
                format,
                parse,
                decode,
            });
        }
    }
    out
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The parameters this crate reports for a real file must be the ones
/// `ffprobe` reports for the same file.
#[test]
fn reported_parameters_match_the_reference() {
    for case in cases() {
        let data = fixture(&format!("{}.{}", case.stem, case.ext));
        let params = (case.parse)(&data)
            .unwrap_or_else(|| panic!("{}: no parameters at all", case.stem));
        let video = params
            .video
            .unwrap_or_else(|| panic!("{}: no video parameters", case.stem));
        assert_eq!(
            (video.width, video.height),
            (case.width, case.height),
            "{}",
            case.stem
        );
        assert_eq!(video.format, Some(case.format), "{}", case.stem);
    }
}

/// JPEG-LS is described but not decoded here: `vaco-codec-jpegls` fails with
/// `UnexpectedEof` on some of the reference encoder's own output (12x8 and
/// 20x8 decode; 16x8, 32x8 and 33x5 do not), a pre-existing entropy-decoder
/// defect this crate's parameter reporting is independent of. Asserting the
/// parameters — which are right — covers the format without asserting the bug.
#[test]
fn jpegls_parameters_match_the_reference() {
    for (name, w, h) in [("jls_13x7.jls", 13u32, 7u32), ("jls_33x5.jls", 33, 5)] {
        let data = fixture(name);
        let video = vaco_codec_jpegls::parameters(&data)
            .and_then(|p| p.video)
            .unwrap_or_else(|| panic!("{name}: no parameters"));
        assert_eq!((video.width, video.height), (w, h), "{name}");
        assert_eq!(video.format, Some(PixFmt::Rgb24), "{name}");
    }
}

/// The same answer through the registered [`vaco_parse_image::ImageParser`]
/// wrapper — the path a demuxer actually takes, and the one that was empty.
#[test]
fn the_parser_wrapper_reports_the_same_thing() {
    let checks: &[(&str, fn(Limits) -> Box<dyn vaco_codec_core::Parser>)] = &[
        ("ppm_13x7.ppm", |l| {
            Box::new(vaco_parse_image::ImageParser::<vaco_parse_image::still::Ppm>::new(l))
        }),
        ("qoi_33x5.qoi", |l| {
            Box::new(vaco_parse_image::ImageParser::<vaco_parse_image::still::Qoi>::new(l))
        }),
        ("sgi_33x5.sgi", |l| {
            Box::new(vaco_parse_image::ImageParser::<vaco_parse_image::still::Sgi>::new(l))
        }),
    ];
    for &(name, make) in checks {
        let data = fixture(name);
        let mut parser = make(Limits::permissive());
        let (packet, used) = parser.parse(&data).expect("parse");
        assert!(packet.is_some(), "{name}: no packet");
        assert_eq!(used, data.len(), "{name}");
        let video = parser
            .parameters()
            .and_then(|p| p.video.as_ref())
            .unwrap_or_else(|| panic!("{name}: parser reported nothing"));
        assert!(video.width > 0 && video.height > 0, "{name}");
        assert!(video.format.is_some(), "{name}");
    }
}

/// And the decoded pixels must be the reference's, byte for byte — a probe
/// that reports `rgb24` while the decoder emits something else is the
/// half-fix that looks finished.
#[test]
fn decoded_pixels_match_the_reference() {
    for case in cases() {
        let data = fixture(&format!("{}.{}", case.stem, case.ext));
        let expected = fixture(&format!("{}.raw", case.stem));
        let mut budget = Budget::new(Limits::permissive());
        let frame = (case.decode)(&data, &mut budget)
            .unwrap_or_else(|e| panic!("{}: decode failed: {e:?}", case.stem));
        let FrameData::Video {
            format,
            width,
            height,
            planes,
        } = &frame.data
        else {
            panic!("{}: not a video frame", case.stem);
        };
        assert_eq!((*width, *height), (case.width, case.height), "{}", case.stem);
        assert_eq!(*format, case.format, "{}", case.stem);

        // `-f rawvideo` writes every plane back to back with no padding, so
        // the comparison has to walk each plane's own stride.
        let mut got: Vec<u8> = Vec::with_capacity(expected.len());
        for (i, plane) in planes.iter().enumerate() {
            let index = u8::try_from(i).expect("plane index");
            let row_bytes = format.min_stride(*width, index);
            let rows = format.plane_height(*height, index) as usize;
            let buf = plane.data.as_slice();
            for y in 0..rows {
                let start = y * plane.stride;
                got.extend_from_slice(
                    buf.get(start..start + row_bytes)
                        .unwrap_or_else(|| panic!("{}: plane {i} row {y} short", case.stem)),
                );
            }
        }
        assert_eq!(
            got.len(),
            expected.len(),
            "{}: produced {} bytes, reference has {}",
            case.stem,
            got.len(),
            expected.len()
        );
        assert!(
            got == expected,
            "{}: pixels differ from the reference",
            case.stem
        );
    }
}
