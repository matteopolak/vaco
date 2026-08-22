//! Every number this crate reports, checked against what `ffprobe 8.1` reports
//! for the same bytes.
//!
//! # How the table was made
//!
//! Nineteen HEVC streams were generated with `ffmpeg 8.1` / `libx265` across the
//! axes that matter — resolution, coded-versus-displayed size, chroma format,
//! bit depth, colour signalling, aspect ratio, frame rate, reorder depth — then
//! each stream's **SPS NAL unit** was lifted out verbatim and `ffprobe` was
//! asked what it made of the file.
//!
//! The generator, so the corpus can be rebuilt when the pinned reference moves:
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=640x360:r=24:d=0.4" \
//!        -pix_fmt yuv420p -c:v libx265 -x265-params log-level=error -f hevc sd.265
//! ffprobe -v error -f hevc -show_entries \
//!   stream=width,height,coded_width,coded_height,sample_aspect_ratio,pix_fmt,\
//! profile,level,color_range,color_space,color_transfer,color_primaries,\
//! chroma_location,field_order,has_b_frames,r_frame_rate \
//!   -of csv=p=0 sd.265
//! ```
//!
//! # Why the SPS is embedded rather than the file
//!
//! Forty-odd bytes against half a megabyte, and the test then needs no media, no
//! network and no `ffmpeg` on `PATH` — it runs in CI, on a fresh checkout, in
//! microseconds. Everything in the table is derived from the SPS alone; nothing
//! here needs the slice data, which is exactly the point of a header parser.
//!
//! # Probing discipline (plan 13 §1b)
//!
//! Read directly, through `-f hevc` on the **raw Annex B demuxer** — one option,
//! straight to the parser, with no container supplying its own opinion. That
//! matters: probing the same content inside MP4 gives `r_frame_rate` from the
//! container's `stts` rather than from the VUI.
//!
//! # The four rows where HEVC's answer differs from H.264's
//!
//! Each is asserted here and documented at the code that produces it:
//!
//! | | H.264 (`vaco-parse-h264`) | HEVC (measured here) |
//! |---|---|---|
//! | `coded_width`/`coded_height` | equal to the *cropped* size | the **coded** size |
//! | `r_frame_rate` | twice the picture rate | the picture rate |
//! | monochrome `pix_fmt` | `yuv420p` | `gray` |
//! | `chroma_location`, no VUI info | `left` at every chroma format | `left` for 4:2:0 only |

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
use vaco_parse_hevc::{ChromaFormat, Sps, codec_parameters, params};

/// One row of the reference table.
struct Row {
    /// Which generated stream this SPS came from.
    name: &'static str,
    /// The SPS NAL unit, EBSP, exactly as it appears in the byte stream.
    sps: &'static [u8],
    /// `ffprobe` `profile`.
    profile: &'static str,
    /// `ffprobe` `level`, which for HEVC is `general_level_idc` unscaled.
    level: i32,
    /// `ffprobe` `width` and `height` — after the conformance window.
    size: (u32, u32),
    /// `ffprobe` `coded_width` and `coded_height` — `pic_*_in_luma_samples`.
    coded: (u32, u32),
    /// `ffprobe` `pix_fmt`.
    pix_fmt: &'static str,
    /// `ffprobe` `sample_aspect_ratio`, or `None` for `N/A`.
    sar: Option<(i32, i32)>,
    /// `ffprobe` `r_frame_rate`.
    r_frame_rate: (i32, i32),
    /// `ffprobe` `has_b_frames`.
    has_b_frames: u8,
    /// `ffprobe` `color_range`: `"unknown"`, `"tv"` or `"pc"`.
    color_range: &'static str,
    /// `ffprobe` `color_space`.
    color_space: &'static str,
    /// `ffprobe` `chroma_location`.
    chroma_location: &'static str,
    /// `ffprobe` `field_order`.
    field_order: &'static str,
}

const TABLE: &[Row] = &[
    Row {
        name: "fhd — 1920x1080, the size HEVC codes exactly",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x10, 0xe5, 0x96, 0x56, 0x69, 0x24,
            0xca, 0xf0, 0x16, 0x80, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0c, 0x84,
        ],
        profile: "Main",
        level: 120,
        size: (1920, 1080),
        coded: (1920, 1080),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "hd — 1280x720 at 30000/1001",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x5d, 0xa0, 0x02, 0x80, 0x80, 0x2d, 0x16, 0x59, 0x59, 0xa4, 0x93,
            0x2b, 0xc0, 0x5a, 0x02, 0x00, 0x00, 0x07, 0xd2, 0x00, 0x00, 0xea, 0x60, 0x10,
        ],
        profile: "Main",
        level: 93,
        size: (1280, 720),
        coded: (1280, 720),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (30_000, 1001),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "sd — 640x360 at 24, the fixture every other test uses",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
            0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
        ],
        profile: "Main",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (24, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "odd — 1918x1078 coded as 1920x1080, the conformance-window case",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x10, 0xe7, 0x55, 0x96, 0x56, 0x69,
            0x24, 0xca, 0xf0, 0x16, 0x80, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0c,
            0x84,
        ],
        profile: "Main",
        level: 120,
        size: (1918, 1078),
        coded: (1920, 1080),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "tiny — 66x34 coded as 72x40, a six-column window",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x1e, 0xa0, 0x24, 0x82, 0x9c, 0x92, 0x65, 0x95, 0x9a, 0xe4, 0xca,
            0xf0, 0x16, 0x80, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0c, 0x84,
        ],
        profile: "Main",
        level: 30,
        size: (66, 34),
        coded: (72, 40),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "p10 — Main 10",
        sps: &[
            0x42, 0x01, 0x01, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x36, 0x59, 0x59, 0xa4, 0x93,
            0x2b, 0xc0, 0x5a, 0x02, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x32,
            0x10,
        ],
        profile: "Main 10",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p10le",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "p12 — 12-bit 4:2:0, which is Rext rather than Main 10",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x99, 0x88, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x14, 0xa5, 0x95, 0x9a, 0x49, 0x32,
            0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p12le",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "p422 — 4:2:2, where the chroma location goes unspecified",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9d, 0x08, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0xb0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32, 0xbc,
            0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv422p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "unspecified",
        field_order: "unknown",
    },
    Row {
        name: "p444 — 4:4:4",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9e, 0x08, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0x90, 0x00, 0xa0, 0x40, 0x2d, 0x2c, 0xb2, 0xb3, 0x49, 0x26, 0x57,
            0x80, 0xb4, 0x04, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x64, 0x20,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv444p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "unspecified",
        field_order: "unknown",
    },
    Row {
        name: "p42210 — 10-bit 4:2:2",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9d, 0x08, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0xb0, 0x05, 0x02, 0x01, 0x69, 0x36, 0x59, 0x59, 0xa4, 0x93, 0x2b,
            0xc0, 0x5a, 0x02, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x32, 0x10,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv422p10le",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "unspecified",
        field_order: "unknown",
    },
    Row {
        name: "p44412 — 12-bit 4:4:4",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x98, 0x08, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0x90, 0x00, 0xa0, 0x40, 0x2d, 0x22, 0x94, 0xb2, 0xb3, 0x49, 0x26,
            0x57, 0x80, 0xb4, 0x04, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x64,
            0x20,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv444p12le",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "unspecified",
        field_order: "unknown",
    },
    Row {
        name: "mono — chroma_format_idc 0, reported as gray and NOT as 4:2:0",
        sps: &[
            0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9f, 0xc8, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x3f, 0xc0, 0x14, 0x08, 0x05, 0xa5, 0x96, 0x56, 0x69, 0x24, 0xca, 0xf0,
            0x16, 0xc0, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0c, 0x84,
        ],
        profile: "Rext",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "gray",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "unspecified",
        field_order: "unknown",
    },
    Row {
        name: "bt709 — a full colour description",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
            0xbc, 0x05, 0xa8, 0x10, 0x10, 0x08, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00,
            0x03, 0x03, 0x21,
        ],
        profile: "Main",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "bt709",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "bt2020 — HDR signalling on a 10-bit stream",
        sps: &[
            0x42, 0x01, 0x01, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x36, 0x59, 0x59, 0xa4, 0x93,
            0x2b, 0xc0, 0x5a, 0x81, 0x01, 0x04, 0x82, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00,
            0x03, 0x00, 0x32, 0x10,
        ],
        profile: "Main 10",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p10le",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "bt2020nc",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "pcrange — full range at 8 bits, the only yuvj HEVC produces",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
            0xbc, 0x05, 0xb0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21,
        ],
        profile: "Main",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuvj420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "pc",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "sar43 — Extended_SAR, 3:4",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
            0xbf, 0xfc, 0x00, 0x0c, 0x00, 0x11, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00,
            0x00, 0x03, 0x03, 0x21,
        ],
        profile: "Main",
        level: 63,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p",
        sar: Some((3, 4)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "anamorphic — 720x576 with a Table E-1 aspect ratio of 16:11",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x5a, 0xa0, 0x05, 0xa2, 0x00, 0x90, 0x59, 0x65, 0x66, 0x92, 0x4c,
            0xaf, 0x04, 0x68, 0x08, 0x00, 0x00, 0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x00, 0xc8,
            0x40,
        ],
        profile: "Main",
        level: 90,
        size: (720, 576),
        coded: (720, 576),
        pix_fmt: "yuv420p",
        sar: Some((16, 11)),
        r_frame_rate: (25, 1),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "fps5994 — 60000/1001, the highest tick rate in the corpus",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x5a, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
            0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x7d, 0x20, 0x00, 0x1d, 0x4c, 0x01,
        ],
        profile: "Main",
        level: 90,
        size: (640, 360),
        coded: (640, 360),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (60_000, 1001),
        has_b_frames: 2,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
    Row {
        name: "nob — bframes=0, so the reorder depth really is zero",
        sps: &[
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x3c, 0xa0, 0x0a, 0x08, 0x0f, 0x16, 0x59, 0x2a, 0x49, 0x32, 0xbc,
            0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21,
        ],
        profile: "Main",
        level: 60,
        size: (320, 240),
        coded: (320, 240),
        pix_fmt: "yuv420p",
        sar: Some((1, 1)),
        r_frame_rate: (25, 1),
        has_b_frames: 0,
        color_range: "tv",
        color_space: "unknown",
        chroma_location: "left",
        field_order: "unknown",
    },
];

fn parse(sps_ebsp: &[u8]) -> Sps {
    let mut scratch = Vec::new();
    let rbsp = annexb::to_rbsp(sps_ebsp, &mut scratch);
    let mut budget = Budget::new(Limits::permissive());
    Sps::parse(rbsp, &mut budget).expect("a real SPS must parse")
}

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

/// Every field of every row, against the reference.
#[test]
fn the_whole_table_matches_the_reference() {
    for row in TABLE {
        let sps = parse(row.sps);
        let params = codec_parameters(&sps);
        let v = params.video.as_ref().expect("video parameters");

        assert_eq!(
            params.profile.map(|p| p.name),
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
        assert_eq!((v.width, v.height), row.size, "{}: size", row.name);
        assert_eq!(
            (v.coded_width, v.coded_height),
            row.coded,
            "{}: coded size",
            row.name
        );
        assert_eq!(
            v.format.map(vaco_pixfmt::PixFmt::name),
            Some(row.pix_fmt),
            "{}: pix_fmt",
            row.name
        );
        match row.sar {
            Some((n, d)) => assert_eq!(
                (v.sample_aspect_ratio.num, v.sample_aspect_ratio.den),
                (n, d),
                "{}: sample_aspect_ratio",
                row.name
            ),
            None => assert!(
                v.sample_aspect_ratio.is_undefined(),
                "{}: sample_aspect_ratio should be N/A",
                row.name
            ),
        }
        assert_eq!(
            (v.frame_rate.num, v.frame_rate.den),
            row.r_frame_rate,
            "{}: r_frame_rate",
            row.name
        );
        assert_eq!(
            v.has_b_frames, row.has_b_frames,
            "{}: has_b_frames",
            row.name
        );
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
        assert_eq!(
            field_order_name(v.field_order),
            row.field_order,
            "{}: field_order",
            row.name
        );
    }
}

/// **The most user-visible number**, isolated: `coded_*` is HEVC's coded size
/// and `width`/`height` is what the conformance window leaves.
///
/// This is the row of the table that differs from H.264 most sharply —
/// `vaco-parse-h264` measured `coded_width` equal to the *cropped* width, and
/// pins that. Both are the reference's behaviour and they disagree, so both
/// crates pin their own.
#[test]
fn the_conformance_window_is_the_difference_between_the_two_sizes() {
    for (name, size, coded, window) in [
        (
            "odd",
            (1918u32, 1078u32),
            (1920u32, 1080u32),
            (0u32, 1, 0, 1),
        ),
        ("tiny", (66, 34), (72, 40), (0, 3, 0, 3)),
        ("sd", (640, 360), (640, 360), (0, 0, 0, 0)),
    ] {
        let row = TABLE
            .iter()
            .find(|r| r.name.starts_with(name))
            .unwrap_or_else(|| panic!("row {name}"));
        let sps = parse(row.sps);
        assert_eq!(sps.dimensions(), Some(size), "{name}: displayed size");
        assert_eq!(
            (sps.coded_width(), sps.coded_height()),
            coded,
            "{name}: coded size"
        );
        let win = sps.conformance_window.unwrap_or_default();
        assert_eq!(
            (win.left, win.right, win.top, win.bottom),
            window,
            "{name}: conformance window offsets"
        );
        // The offsets are in CHROMA units: for 4:2:0 a right offset of 1
        // removes two luma columns, which is what makes 1920 into 1918.
        assert_eq!(sps.chroma_format, ChromaFormat::Yuv420);
        assert_eq!(
            coded.0 - (win.left + win.right) * 2,
            size.0,
            "{name}: SubWidthC is 2"
        );
        assert_eq!(
            coded.1 - (win.top + win.bottom) * 2,
            size.1,
            "{name}: SubHeightC is 2"
        );
    }
}

/// The frame rate is **not** halved, unlike H.264's.
///
/// The reference prints `r_frame_rate = vui_time_scale / vui_num_units_in_tick`
/// for HEVC. `vaco-parse-h264` pins the opposite for H.264, where the same
/// division gives twice the picture rate.
#[test]
fn the_frame_rate_is_the_tick_rate_undivided() {
    for row in TABLE {
        let sps = parse(row.sps);
        let vui = sps.vui.as_ref().expect("every row's stream has a VUI");
        let timing = vui.timing.expect("and timing info");
        let (num, den) = row.r_frame_rate;
        let (reduced, _) = Rational::reduce(
            i64::from(timing.time_scale),
            i64::from(timing.num_units_in_tick),
            i64::from(i32::MAX),
        );
        assert_eq!(
            (reduced.num, reduced.den),
            (num, den),
            "{}: time_scale / num_units_in_tick",
            row.name
        );
        assert_eq!(
            (sps.frame_rate().num, sps.frame_rate().den),
            (num, den),
            "{}: and no halving anywhere",
            row.name
        );
    }
}

/// The monochrome stream really is `chroma_format_idc == 0`, so the `gray` in
/// the table is what the reference chose and not an artefact of the fixture.
#[test]
fn the_monochrome_stream_is_genuinely_monochrome() {
    let row = TABLE
        .iter()
        .find(|r| r.name.starts_with("mono"))
        .expect("the mono row");
    let sps = parse(row.sps);
    assert_eq!(sps.chroma_format, ChromaFormat::Monochrome);
    assert_eq!(sps.chroma_array_type(), ChromaFormat::Monochrome);
    assert_eq!(
        params::pixel_format(&sps).map(vaco_pixfmt::PixFmt::name),
        Some("gray"),
        "and gray is what the reference prints — where for H.264 it prints yuv420p"
    );
}

/// The chroma-location inference applies to 4:2:0 only, which is the second
/// place HEVC and H.264 disagree in the reference.
#[test]
fn the_chroma_location_inference_is_four_two_zero_only() {
    for row in TABLE {
        let sps = parse(row.sps);
        let vui = sps.vui.as_ref().expect("a VUI");
        assert!(
            vui.chroma_sample_loc.is_none(),
            "{}: x265 writes no chroma_loc_info, so every row tests the inference",
            row.name
        );
        let expected = if sps.chroma_format == ChromaFormat::Yuv420 {
            "left"
        } else {
            "unspecified"
        };
        assert_eq!(
            sps.color_info().chroma_location.name(),
            expected,
            "{}: chroma_location for {:?}",
            row.name,
            sps.chroma_format
        );
    }
}

/// `general_level_idc` is thirty times the level number, and the reference
/// prints it unscaled.
#[test]
fn the_level_is_thirty_times_the_level_number() {
    for (name, idc, level_name) in [
        ("tiny", 30i32, "1"),
        ("nob", 60, "2"),
        ("sd", 63, "2.1"),
        ("anamorphic", 90, "3"),
        ("hd", 93, "3.1"),
        ("fhd", 120, "4"),
    ] {
        let row = TABLE
            .iter()
            .find(|r| r.name.starts_with(name))
            .unwrap_or_else(|| panic!("row {name}"));
        assert_eq!(row.level, idc, "{name}: the reference's raw value");
        let sps = parse(row.sps);
        assert_eq!(i32::from(sps.ptl.general_level_idc), idc);
        assert_eq!(
            vaco_parse_hevc::profile::level_name(sps.ptl.general_level_idc),
            Some(level_name),
            "{name}: display name"
        );
    }
}

/// Every SPS in the corpus survives every single-bit corruption and every
/// truncation without panicking — the property the fuzzer generalises.
#[test]
fn no_corruption_of_a_real_sps_panics() {
    for row in TABLE {
        let mut scratch = Vec::new();
        let rbsp = annexb::to_rbsp(row.sps, &mut scratch).to_vec();
        for n in 0..rbsp.len() {
            let mut budget = Budget::new(Limits::strict());
            let _ = Sps::parse(&rbsp[..n], &mut budget);
        }
        for byte in 0..rbsp.len() {
            for bit in 0..8 {
                let mut data = rbsp.clone();
                data[byte] ^= 1 << bit;
                let mut budget = Budget::new(Limits::strict());
                if let Ok(sps) = Sps::parse(&data, &mut budget) {
                    // Whatever comes out must still be self-consistent.
                    if let Some((w, h)) = sps.dimensions() {
                        assert!(w > 0 && h > 0, "{}: a zero dimension escaped", row.name);
                        assert!(w <= sps.coded_width(), "{}: window widened", row.name);
                        assert!(h <= sps.coded_height(), "{}: window heightened", row.name);
                    }
                    let _ = codec_parameters(&sps);
                }
            }
        }
    }
}
