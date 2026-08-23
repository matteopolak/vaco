//! Shared pads, plane access and option parsing for this crate's filters.

use vaco_core::MediaType;
use vaco_filter_core::Pad;
use vaco_filter_graph::registry::Instantiate;
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

/// One video pad in, one out — every single-input filter in this crate
/// (`signalstats`, `blackdetect`, `blackframe`, `bbox`).
pub(crate) const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// `main`/`reference` in, `default` out — measured against `ffmpeg -h
/// filter=psnr` (and `ssim`/`identity`/`msad`, which share the same pad
/// names verbatim): `Inputs: #0: main (video) #1: reference (video)`,
/// `Outputs: #0: default (video)`.
pub(crate) const REFERENCE_PADS: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Video,
    },
    Pad {
        name: "reference",
        media_type: MediaType::Video,
    },
];

/// Copy everything but the pixel data, matching
/// `vaco-filter-temporal::video::copy_meta`'s shape.
pub(crate) fn copy_meta(dst: &mut Frame, src: &Frame) {
    dst.pts = src.pts;
    dst.time_base = src.time_base;
    dst.duration = src.duration;
    dst.color = src.color;
    dst.flags = src.flags;
    dst.sample_aspect_ratio = src.sample_aspect_ratio;
}

/// This frame's pixel format and plane count, or `None` for audio.
pub(crate) fn video_shape(frame: &Frame) -> Option<(PixFmt, u32, u32, usize)> {
    match &frame.data {
        FrameData::Video {
            format,
            width,
            height,
            planes,
        } => Some((*format, *width, *height, planes.len())),
        FrameData::Audio { .. } => None,
    }
}

/// The reference's per-plane tag label for `psnr`/`ssim` (ascending plane
/// order: `Y U V` for the planar YUV family, `G B R` for `gbrp`'s own plane
/// order, `Y` alone for `gray`), measured via `ffmpeg -h filter=psnr` /
/// `ffprobe -show_frames` on both pixel families.
///
/// `identity`/`msad` are measured (`docs/filter/vaco-filter-analysis.md`) to
/// use a *different*, non-ascending plane order for the same formats
/// (`V Y U` for yuv420p, `B R G` for gbrp) that this crate does not
/// reproduce — the tag *values* are byte-exact, the tag *order* is not, and
/// that divergence is recorded rather than silently matched by accident.
/// This function is therefore used by `psnr`/`ssim` only.
pub(crate) fn component_labels(format: PixFmt, plane_count: usize) -> &'static [&'static str] {
    match (format, plane_count) {
        (PixFmt::Gray8 | PixFmt::Gray16le, 1) => &["Y"],
        (PixFmt::Yuv420p | PixFmt::Yuv422p | PixFmt::Yuv444p, 3) => &["Y", "U", "V"],
        (PixFmt::Gbrp, 3) => &["G", "B", "R"],
        _ => match plane_count {
            1 => &["0"],
            2 => &["0", "1"],
            3 => &["0", "1", "2"],
            _ => &["0", "1", "2", "3"],
        },
    }
}

pub(crate) fn f64_opt(req: &Instantiate<'_>, key: &str, default: f64) -> f64 {
    req.named(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

pub(crate) fn u8_opt(req: &Instantiate<'_>, key: &str, default: u8) -> u8 {
    req.named(key)
        .and_then(|v| v.trim().parse::<u8>().ok())
        .unwrap_or(default)
}
