//! Every number this crate reports, checked against what `ffprobe 8.1` reports
//! for the same bytes.
//!
//! # How the table was made
//!
//! Nineteen H.264 streams were generated with `ffmpeg 8.1` across the axes that
//! matter — resolution, chroma format, bit depth, colour signalling, aspect
//! ratio, frame/field coding, POC type, profile — then each stream's **SPS NAL
//! unit** was lifted out verbatim and `ffprobe` was asked what it made of the
//! file.
//!
//! The generator, so the corpus can be rebuilt when the pinned reference moves:
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=640x360:r=24:d=1" \
//!        -pix_fmt yuv420p -c:v libx264 -f h264 sd.264
//! ffprobe -v error -f h264 -show_entries \
//!   stream=width,height,coded_width,coded_height,sample_aspect_ratio,pix_fmt,\
//! profile,level,color_range,color_space,color_transfer,color_primaries,\
//! chroma_location,field_order,has_b_frames,r_frame_rate,bits_per_raw_sample \
//!   -of csv=p=0 sd.264
//! ```
//!
//! # Why the SPS is embedded rather than the file
//!
//! Twenty-six bytes against seven hundred kilobytes, and the test then needs no
//! media, no network and no `ffmpeg` on `PATH` — it runs in CI, on a fresh
//! checkout, in microseconds. Everything in the table above is derived from the
//! SPS alone; nothing here needs the slice data, which is exactly the point of
//! a header parser.
//!
//! # Probing discipline (plan 13 §1b)
//!
//! Read directly, through `-f h264` on the **raw Annex B demuxer** — one option,
//! straight to the parser, with no container supplying its own opinion. That
//! matters here: probing the same content inside MP4 gives `r_frame_rate=25/1`
//! from the container's `stts` rather than `48/1` from the VUI, and
//! `avg_frame_rate` in the raw path is the *demuxer's* `-framerate` default of
//! 25 rather than anything parsed at all. Neither is the parser's answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_bitstream::annexb;
use vaco_codec_core::FieldOrder;
use vaco_core::Rational;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{ChromaFormat, Sps, codec_parameters, params};

/// One row of the reference table.
struct Row {
    /// Which generated stream this SPS came from.
    name: &'static str,
    /// The SPS NAL unit, EBSP, exactly as it appears in the byte stream.
    sps: &'static [u8],
    /// `ffprobe` `profile`.
    profile: &'static str,
    /// `ffprobe` `level`.
    level: i32,
    /// `ffprobe` `width` and `height`.
    size: (u32, u32),
    /// `ffprobe` `pix_fmt`.
    pix_fmt: &'static str,
    /// `ffprobe` `sample_aspect_ratio`, or `None` for `N/A`.
    sar: Option<(i32, i32)>,
    /// `ffprobe` `r_frame_rate`.
    r_frame_rate: (i32, i32),
    /// `ffprobe` `has_b_frames`.
    has_b_frames: u8,
    /// `ffprobe` `bits_per_raw_sample`.
    bit_depth: u8,
    /// `ffprobe` `color_range`: `"unknown"`, `"tv"` or `"pc"`.
    color_range: &'static str,
    /// `ffprobe` `color_space`.
    color_space: &'static str,
    /// `ffprobe` `chroma_location`.
    chroma_location: &'static str,
    /// `ffprobe` `field_order`.
    field_order: &'static str,
    /// The macroblock-aligned height, which `ffprobe` does **not** print — see
    /// the note on `coded_height` below.
    coded_height: u32,
}

const TABLE: &[Row] = &[
    Row {
        name: "fhd — 1920x1080, the canonical 1088-and-cropped case",
        sps: &[
            0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00,
            0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc6, 0x58,
        ],
        profile: "High",
        level: 40,
        size: (1920, 1080),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 1088,
    },
    Row {
        name: "hd — 1280x720 at 30000/1001",
        sps: &[
            0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x10, 0x00, 0x00,
            0x3e, 0x90, 0x00, 0x0e, 0xa6, 0x00, 0xf1, 0x83, 0x19, 0x60,
        ],
        profile: "High",
        level: 31,
        size: (1280, 720),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (60_000, 1_001),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 720,
    },
    Row {
        name: "sd — 640x360 at 24, cropped from 368",
        sps: &[
            0x67, 0x64, 0x00, 0x1e, 0xac, 0xd9, 0x40, 0xa0, 0x2f, 0xf9, 0x70, 0x11, 0x00, 0x00,
            0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0f, 0x16, 0x2d, 0x96,
        ],
        profile: "High",
        level: 30,
        size: (640, 360),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (48, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 368,
    },
    Row {
        name: "odd — 1918x1078, cropped on both axes",
        sps: &[
            0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xa9, 0xb0, 0x11, 0x00,
            0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0f, 0x18, 0x31, 0x96,
        ],
        profile: "High",
        level: 40,
        size: (1918, 1078),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 1088,
    },
    Row {
        name: "p422 — 4:2:2, where CropUnitY is half what 4:2:0's is",
        sps: &[
            0x67, 0x7a, 0x00, 0x1e, 0xbc, 0xd9, 0x40, 0xa0, 0x2f, 0xf8, 0x9c, 0x04, 0x40, 0x00,
            0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x0c, 0x83, 0xc5, 0x8b, 0x65, 0x80,
        ],
        profile: "High 4:2:2",
        level: 30,
        size: (640, 360),
        pix_fmt: "yuv422p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 368,
    },
    Row {
        name: "p444 — 4:4:4, CropUnit 1x1",
        sps: &[
            0x67, 0xf4, 0x00, 0x1e, 0x91, 0x9b, 0x28, 0x14, 0x05, 0xff, 0x13, 0x80, 0x88, 0x00,
            0x00, 0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0x90, 0x78, 0xb1, 0x6c, 0xb0,
        ],
        profile: "High 4:4:4 Predictive",
        level: 30,
        size: (640, 360),
        pix_fmt: "yuv444p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 368,
    },
    Row {
        name: "p10 — 10-bit",
        sps: &[
            0x67, 0x6e, 0x00, 0x1e, 0xa6, 0xcd, 0x94, 0x0a, 0x02, 0xff, 0x97, 0x01, 0x10, 0x00,
            0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x62, 0xd9, 0x60,
        ],
        profile: "High 10",
        level: 30,
        size: (640, 360),
        pix_fmt: "yuv420p10le",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 10,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 368,
    },
    Row {
        // D17: chroma_format_idc is 0 and the reference still says 4:2:0.
        name: "mono — chroma_format_idc 0, reported as 4:2:0 and full range",
        sps: &[
            0x67, 0x64, 0x00, 0x1e, 0xf3, 0x65, 0x02, 0x80, 0xbf, 0xe2, 0x70, 0x16, 0xc8, 0x00,
            0x00, 0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0x90, 0x78, 0xb1, 0x6c, 0xb0,
        ],
        profile: "High",
        level: 30,
        size: (640, 360),
        pix_fmt: "yuvj420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 368,
    },
    Row {
        name: "ilace — frame_mbs_only_flag 0, MBAFF, 720x480",
        sps: &[
            0x67, 0x64, 0x00, 0x1e, 0xac, 0xd9, 0x40, 0xb4, 0x7b, 0x60, 0x22, 0x00, 0x00, 0x07,
            0xd2, 0x00, 0x01, 0xd4, 0xc0, 0x3e, 0x28, 0x53, 0x2c,
        ],
        profile: "High",
        level: 30,
        size: (720, 480),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (60_000, 1_001),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        // From the SEI `pic_timing`, not the SPS — see `field_order_needs_the_sei`.
        field_order: "tt",
        coded_height: 480,
    },
    Row {
        name: "poc2 — Constrained Baseline, pic_order_cnt_type 2, no B frames",
        sps: &[
            0x67, 0x42, 0xc0, 0x0d, 0xd9, 0x01, 0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
            0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0xa4, 0x80,
        ],
        profile: "Constrained Baseline",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 0,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "fr420 — full range 8-bit becomes yuvj",
        sps: &[
            0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x6c, 0x80, 0x00, 0x00,
            0x03, 0x00, 0x80, 0x00, 0x00, 0x19, 0x07, 0x8a, 0x14, 0xcb,
        ],
        profile: "High",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuvj420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "fr422 — full range 4:2:2",
        sps: &[
            0x67, 0x7a, 0x00, 0x0d, 0xbc, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x6c, 0x80, 0x00, 0x00,
            0x03, 0x00, 0x80, 0x00, 0x00, 0x19, 0x07, 0x8a, 0x14, 0xcb,
        ],
        profile: "High 4:2:2",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuvj422p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "fr444 — full range 4:4:4",
        sps: &[
            0x67, 0xf4, 0x00, 0x0d, 0x91, 0x9b, 0x28, 0x28, 0x3f, 0x60, 0x2d, 0x90, 0x00, 0x00,
            0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0x99, 0x60,
        ],
        profile: "High 4:4:4 Predictive",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuvj444p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        // D17: full range above 8 bits stays plain `yuv`, because no `yuvj`
        // format exists there.
        name: "fr10 — full range 10-bit stays yuv420p10le",
        sps: &[
            0x67, 0x6e, 0x00, 0x0d, 0xa6, 0xcd, 0x94, 0x14, 0x1f, 0xb0, 0x16, 0xc8, 0x00, 0x00,
            0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0x90, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        profile: "High 10",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p10le",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 10,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        // No `video_signal_type` at all, so the range is *unknown* rather than
        // limited — the distinction a single flag cannot express.
        name: "lr10 — limited range 10-bit, no video_signal_type",
        sps: &[
            0x67, 0x6e, 0x00, 0x0d, 0xa6, 0xcd, 0x94, 0x14, 0x1f, 0xb0, 0x11, 0x00, 0x00, 0x03,
            0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0f, 0x14, 0x29, 0x96,
        ],
        profile: "High 10",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p10le",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 10,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "bt709 — matrix_coefficients 1, primaries and transfer unspecified",
        sps: &[
            0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x6a, 0x04, 0x04, 0x02,
            0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x19, 0x07, 0x8a, 0x14, 0xcb,
        ],
        profile: "High",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "tv",
        color_space: "bt709",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "bt2020 — matrix_coefficients 9",
        sps: &[
            0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x6a, 0x04, 0x04, 0x12,
            0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x19, 0x07, 0x8a, 0x14, 0xcb,
        ],
        profile: "High",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "tv",
        color_space: "bt2020nc",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "sar43 — aspect_ratio_idc 14, Table E-1's 4:3",
        sps: &[
            0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x0e, 0x10, 0x00, 0x00, 0x03,
            0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0x99, 0x60,
        ],
        profile: "High",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((4, 3)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "sar_odd — Extended_SAR 99:85",
        sps: &[
            0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0xff, 0x00, 0x63, 0x00, 0x55,
            0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0x99,
            0x60,
        ],
        profile: "High",
        level: 13,
        size: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((99, 85)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 240,
    },
    Row {
        name: "main352 -- plain Main profile, not Constrained Baseline or High",
        sps: &[
            0x67, 0x4D, 0x40, 0x1E, 0xEC, 0xA0, 0xB0, 0x4B, 0x60, 0x22, 0x00, 0x00, 0x03, 0x00,
            0x02, 0x00, 0x00, 0x03, 0x00, 0x64, 0x1E, 0x2C, 0x5B, 0x2C,
        ],
        profile: "Main",
        level: 30,
        size: (352, 288),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (50, 1),
        has_b_frames: 2,
        bit_depth: 8,
        color_range: "unknown",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "progressive",
        coded_height: 288,
    },
];

fn parse(sps_ebsp: &[u8]) -> Sps {
    let mut scratch = Vec::new();
    let rbsp = annexb::to_rbsp(sps_ebsp, &mut scratch);
    let mut budget = Budget::new(Limits::permissive());
    Sps::parse(rbsp, &mut budget).expect("a real SPS must parse")
}

/// The `field_order` name the reference prints for a [`FieldOrder`].
///
/// The reference's five spellings, from `-show_streams`: `progressive`, `tt`,
/// `bb`, `tb`, `bt`, and `unknown` for a stream it could not classify.
fn field_order_name(f: FieldOrder) -> &'static str {
    match f {
        FieldOrder::Progressive => "progressive",
        FieldOrder::TopFirst => "tt",
        FieldOrder::BottomFirst => "bb",
        FieldOrder::TopCodedFirst => "tb",
        FieldOrder::BottomCodedFirst => "bt",
        FieldOrder::Unknown => "unknown",
    }
}

fn color_range_name(r: vaco_color::ColorRange) -> &'static str {
    match r {
        vaco_color::ColorRange::Unspecified => "unknown",
        vaco_color::ColorRange::Limited => "tv",
        vaco_color::ColorRange::Full => "pc",
    }
}

#[test]
fn every_reported_number_matches_the_reference() {
    for row in TABLE {
        let sps = parse(row.sps);
        let params = codec_parameters(&sps);
        let v = params.video.as_ref().expect("video parameters");

        assert_eq!(sps.dimensions(), Some(row.size), "{}: size", row.name);
        assert_eq!((v.width, v.height), row.size, "{}: reported size", row.name);
        assert_eq!(
            sps.coded_height(),
            row.coded_height,
            "{}: macroblock-aligned height",
            row.name
        );
        assert_eq!(
            sps.profile_name(),
            Some(row.profile),
            "{}: profile",
            row.name
        );
        assert_eq!(
            params.level.map(vaco_codec_core::Level::raw),
            Some(row.level),
            "{}: level",
            row.name
        );
        assert_eq!(
            v.format.map(vaco_pixfmt::PixFmt::name),
            Some(row.pix_fmt),
            "{}: pix_fmt",
            row.name
        );
        let sar = params::sample_aspect_ratio(&sps);
        match row.sar {
            Some((n, d)) => assert_eq!((sar.num, sar.den), (n, d), "{}: sar", row.name),
            None => assert!(sar.is_undefined(), "{}: sar should be N/A", row.name),
        }
        assert_eq!(
            (v.frame_rate.num, v.frame_rate.den),
            row.r_frame_rate,
            "{}: r_frame_rate — the *tick* rate, twice the picture rate",
            row.name
        );
        assert_eq!(
            v.has_b_frames, row.has_b_frames,
            "{}: has_b_frames",
            row.name
        );
        assert_eq!(sps.bit_depth_luma, row.bit_depth, "{}: bit depth", row.name);
        assert_eq!(
            color_range_name(v.color.range),
            row.color_range,
            "{}: color_range",
            row.name
        );
        assert_eq!(
            v.color.matrix.name(),
            row.color_space,
            "{}: color_space",
            row.name
        );
        assert_eq!(
            v.color.chroma_location.name(),
            row.chroma_location,
            "{}: chroma_location",
            row.name
        );
        // `field_order` comes from the SPS alone here; the interlaced row is
        // the one case where it does not, and it is checked separately.
        if row.field_order != "tt" {
            assert_eq!(
                field_order_name(v.field_order),
                row.field_order,
                "{}: field_order",
                row.name
            );
        }
    }
}

/// A real 4K SPS must parse under [`Limits::strict`], not just
/// [`Limits::permissive`].
///
/// Regression for the bug this session fixed: `Sps::parse_data`'s own
/// end-of-parse budget check assumed 4 bytes per pixel — the widest packed
/// 8-bit layout, right for the RGBA-ish image codecs that pattern is copied
/// from, wildly wrong for a YUV 4:2:0 video frame. At 3840x2160 that
/// overshoot (33.2 MB) blew straight through `Limits::strict`'s 16 MiB
/// `max_frame_bytes` cap even though the real frame this SPS describes is
/// 12.4 MB, so an ordinary 4K Main-profile stream failed to parse its own
/// SPS under the reference-mirroring "conservative caps for untrusted input"
/// default (`Discovery::new`'s own default, which is what `vaco-probe` and
/// `vaco-cli` build their parser from) — `vaco-probe` printed
/// `profile=unknown`, `pix_fmt=unknown`, `level=-99` on a file libx264 wrote
/// without complaint, and looked exactly like a rejected level 5.1 rather
/// than a resolution-triggered budget miscalculation. `level_idc` itself was
/// never involved: any codec-level table lookup keyed on it (see
/// [`vaco_parse_h264::LEVELS`]) accepts 51 outright, and this SPS parses
/// fine under [`Limits::permissive`] both before and after the fix — the
/// crossover is `pic_width_in_mbs * pic_height_in_map_units * 16 * 16 * 4 >
/// 16 MiB`, which 4:2:0 8-bit content the size of a real broadcast frame
/// reaches well before any level table does.
///
/// This SPS is the real bytes `libx264` wrote for `testsrc2=size=3840x2160`,
/// Main profile, `-bf 0 -refs 1`, CABAC, level 5.1 (`0x33`) — lifted the same
/// way every row in [`TABLE`] is.
#[test]
fn a_4k_sps_fits_the_strict_frame_byte_cap() {
    let ebsp: &[u8] = &[
        0x67, 0x4d, 0x40, 0x33, 0xda, 0x00, 0xf0, 0x01, 0x0f, 0xb0, 0x11, 0x00, 0x00, 0x03, 0x00,
        0x01, 0x00, 0x00, 0x03, 0x00, 0x32, 0x0f, 0x18, 0x32, 0xa0,
    ];
    let mut scratch = Vec::new();
    let rbsp = annexb::to_rbsp(ebsp, &mut scratch);

    for limits in [Limits::strict(), Limits::permissive()] {
        let mut budget = Budget::new(limits);
        let sps = Sps::parse(rbsp, &mut budget).expect("a real 4K Main SPS must parse");
        assert_eq!(sps.dimensions(), Some((3840, 2160)));
        assert_eq!(sps.profile_name(), Some("Main"));
        assert_eq!(sps.level_idc, 51);
        let params = codec_parameters(&sps);
        let v = params.video.as_ref().expect("video parameters");
        assert_eq!(v.format.map(vaco_pixfmt::PixFmt::name), Some("yuv420p"));
    }
}

/// The picture rate, as ITU-T H.264 §E.2.1 defines it, is *half* what the
/// reference prints as `r_frame_rate`.
///
/// Both are exposed and this test pins the relationship, so that a future
/// change to either surfaces here rather than as a doubled frame count.
#[test]
fn the_picture_rate_is_half_the_tick_rate() {
    for row in TABLE {
        let sps = parse(row.sps);
        let tick = sps
            .vui
            .as_ref()
            .expect("every row's stream has a VUI")
            .tick_rate();
        let frame = sps.frame_rate();
        assert_eq!((tick.num, tick.den), row.r_frame_rate, "{}", row.name);
        // frame_rate = tick_rate / 2, exactly, after reduction.
        let doubled = Rational::new(frame.num.checked_mul(2).expect("small"), frame.den);
        let (a, _) = Rational::reduce(
            i64::from(doubled.num),
            i64::from(doubled.den),
            i64::from(i32::MAX),
        );
        assert_eq!((a.num, a.den), (tick.num, tick.den), "{}", row.name);
    }
}

/// The interlaced stream's field order is in an SEI `pic_timing`, not the SPS.
///
/// `libx264` writes `pic_struct = 3` — "top field, bottom field, in that
/// order" — in every access unit, and `ffprobe` reports `field_order=tt`.
/// The SPS alone can only say that the stream *may* be field-coded.
#[test]
fn field_order_needs_the_sei() {
    let row = TABLE
        .iter()
        .find(|r| r.field_order == "tt")
        .expect("the interlaced row");
    let sps = parse(row.sps);
    assert!(!sps.frame_mbs_only, "the stream is not frame-only");
    assert!(sps.mb_adaptive_frame_field, "and it is MBAFF");
    let params = codec_parameters(&sps);
    assert_eq!(
        field_order_name(params.video.as_ref().unwrap().field_order),
        "unknown",
        "the SPS alone cannot say which field comes first"
    );

    // The `pic_timing` SEI from the same stream, verbatim: payload type 1,
    // size 1, `pic_struct = 3`.
    let sei_nal = [0x06u8, 0x01, 0x01, 0b0011_0010, 0x80];
    let mut budget = Budget::new(Limits::permissive());
    let msgs =
        vaco_parse_h264::sei::parse(&sei_nal, Some(&sps), &mut budget).expect("the SEI parses");
    let order = msgs
        .iter()
        .find_map(|m| match &m.payload {
            vaco_parse_h264::SeiPayload::PicTiming {
                pic_struct: Some(ps),
                ..
            } => Some((ps.0, ps.field_order())),
            _ => None,
        })
        .expect("a pic_timing message");
    assert_eq!(order.0, 3, "pic_struct");
    assert_eq!(field_order_name(order.1), "tt");
}

/// Cropping, spelled out per chroma format, because the crop *unit* differs.
///
/// The same `frame_crop_bottom_offset` removes a different number of luma rows
/// depending on `SubHeightC` and on `frame_mbs_only_flag`. Every one of these
/// four is a real stream from the table above.
#[test]
fn the_crop_unit_depends_on_the_chroma_format() {
    for (name, expect_unit) in [
        ("sd", (2u32, 2u32)), // 4:2:0 progressive
        ("p422", (2, 1)),     // 4:2:2 progressive: SubHeightC is 1
        ("p444", (1, 1)),     // 4:4:4
        ("mono", (1, 1)),     // ChromaArrayType 0
        ("ilace", (2, 4)),    // 4:2:0 field-coded: CropUnitY doubles
    ] {
        let row = TABLE
            .iter()
            .find(|r| r.name.starts_with(name))
            .unwrap_or_else(|| panic!("row {name}"));
        let sps = parse(row.sps);
        assert_eq!(sps.crop_unit(), expect_unit, "{name}: crop unit");
        // And the crop really does land on the reference's dimensions.
        assert_eq!(sps.dimensions(), Some(row.size), "{name}: dimensions");
    }
}

/// The monochrome stream really is `chroma_format_idc == 0`, so the `yuv420p`
/// in the table above is the reference's choice and not ours.
#[test]
fn the_monochrome_stream_is_genuinely_monochrome() {
    let row = TABLE
        .iter()
        .find(|r| r.name.starts_with("mono"))
        .expect("the mono row");
    let sps = parse(row.sps);
    assert_eq!(sps.chroma_format, ChromaFormat::Monochrome);
    assert_eq!(sps.chroma_array_type(), ChromaFormat::Monochrome);
    // ...and we report 4:2:0 anyway, per D17.
    assert_eq!(
        params::pixel_format(&sps).map(vaco_pixfmt::PixFmt::name),
        Some("yuvj420p")
    );
}

/// The sample-aspect-ratio rejection rule, at both boundaries.
///
/// Probed on a 320x240 stream by patching `sar_width` and `sar_height` in the
/// VUI. Every row here was read back from `ffprobe 8.1`.
#[test]
fn the_sar_rejection_boundary() {
    // (sar_width, sar_height, what ffprobe printed)
    let cases: &[(u16, u16, Option<(i32, i32)>)] = &[
        (1, 1, Some((1, 1))),
        (4, 2, Some((2, 1))),     // reduced
        (6, 4, Some((3, 2))),     // reduced
        (100, 10, Some((10, 1))), // reduced
        (3, 1, Some((3, 1))),
        (11, 1, Some((11, 1))),
        (239, 1, Some((239, 1))),
        (240, 1, Some((240, 1))), // 240*1/240 = 1, just usable
        (241, 1, None),           // 240*1/241 = 0, discarded
        (250, 1, None),
        (480, 2, Some((240, 1))), // reduces to 240:1 first
        (482, 2, None),           // reduces to 241:1
        (1, 319, Some((1, 319))),
        (1, 320, Some((1, 320))), // 320*1/320 = 1
        (1, 321, None),           // 320*1/321 = 0
        (2, 640, Some((1, 320))),
        (2, 642, None),
        (65_535, 65_535, Some((1, 1))),
        (65_534, 65_533, Some((65_534, 65_533))),
    ];
    let row = TABLE
        .iter()
        .find(|r| r.name.starts_with("sar_odd"))
        .expect("the extended-SAR row");
    let base = parse(row.sps);
    for &(w, h, expected) in cases {
        let mut sps = base.clone();
        sps.vui.as_mut().expect("a VUI").sar = Some((w, h));
        let got = params::sample_aspect_ratio(&sps);
        match expected {
            Some((n, d)) => assert_eq!((got.num, got.den), (n, d), "sar {w}:{h}"),
            None => assert!(got.is_undefined(), "sar {w}:{h} should be discarded"),
        }
    }
}

/// An SPS whose crop leaves nothing is rejected outright, as the reference
/// rejects it.
///
/// Probed: patching `frame_crop_right_offset` to 320 on the 640-wide stream
/// makes `ffprobe` print `crop values invalid 0 320 0 4 / 640 368` and drop the
/// stream entirely — `width=0 height=0 sample_aspect_ratio=N/A`.
#[test]
fn a_crop_that_leaves_nothing_is_rejected() {
    let row = TABLE
        .iter()
        .find(|r| r.name.starts_with("sd"))
        .expect("the sd row");
    let mut sps = parse(row.sps);
    let crop = sps.crop.as_mut().expect("the sd stream is cropped");
    crop.right = 320;
    assert_eq!(sps.dimensions(), None, "640 - 2*320 = 0 is not a picture");
    // ...and the check is in the parser, not only in the accessor.
    // (The parser rejects it while reading; see `sps::Sps::parse_data`.)
}
