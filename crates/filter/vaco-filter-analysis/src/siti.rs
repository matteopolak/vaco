//! `siti` — spatial and temporal information for 8-bit planar luma.
//!
//! The current reference filter follows the legacy P.910 calculation: a
//! clamped 8-bit range expansion, Sobel magnitude over the interior, and
//! population standard deviations. It does not apply a transfer-dependent
//! BT.1886, inverse-sRGB, or PQ conversion: probes with each of those transfer
//! tags produce the same score. That behaviour is intentionally distinct from
//! P.910 (07/2022)'s newer HDR-aware calculation pipeline.
//!
//! # Reference probes
//!
//! On a 16x16 vertical `100`/`120` step, full range reports `27.99` while
//! limited and unspecified range report `33.59`. The latter comes from the
//! reference's clamped integer expansion, where the two input codes become
//! `97` and `121`, not from a floating-point `255 / 219` scale. Repeating the
//! probe with `bt709`, `iec61966-2-1`, `linear`, and `smpte2084` transfer tags
//! leaves each score unchanged. A `0 -> half-right-100 -> half-right-30`
//! full-range sequence reports `(SI, TI)` of `(0.00, 0.00)`, `(139.97,
//! 50.00)`, and `(41.99, 35.00)`, pinning predecessor state and population
//! variance independently of the Sobel path.

use vaco_color::ColorRange;
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_graph::registry::{Instance, Instantiate};
use vaco_frame::Frame;
use vaco_pixfmt::PixFmt;

use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "siti",
    description: "Calculate spatial information and temporal information.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const SUPPORTED_FORMATS: &[PixFmt] = &[
    PixFmt::Gray8,
    PixFmt::Yuv420p,
    PixFmt::Yuv422p,
    PixFmt::Yuv444p,
];

#[derive(Debug, Clone)]
struct LumaPlane {
    width: usize,
    height: usize,
    samples: Vec<f64>,
}

fn luma_plane(frame: &Frame) -> Option<LumaPlane> {
    let format = frame.pixel_format()?;
    if !SUPPORTED_FORMATS.contains(&format) {
        return None;
    }
    let (width, height) = frame.dimensions()?;
    let (width, height) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    let plane = frame.plane(0)?;
    let mut samples = Vec::new();
    for y in 0..height {
        let row = plane.row(y)?.get(..width)?;
        samples.extend(
            row.iter()
                .copied()
                .map(|sample| expand_luma(sample, frame.color.range)),
        );
    }
    Some(LumaPlane {
        width,
        height,
        samples,
    })
}

/// The reference's legacy range conversion, observed to truncate before
/// computing SI/TI. `Unspecified` follows the decoder's limited-range default.
fn expand_luma(sample: u8, range: ColorRange) -> f64 {
    match range {
        ColorRange::Full => f64::from(sample),
        ColorRange::Limited | ColorRange::Unspecified => {
            #[allow(
                clippy::integer_division,
                reason = "the reference truncates this legacy range expansion before SI/TI"
            )]
            let expanded = u16::from(sample.saturating_sub(16)) * 255 / 219;
            f64::from(expanded.min(255))
        }
    }
}

fn population_stddev(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "frame dimensions fit exactly in f64"
    )]
    let count = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / count;
    (samples
        .iter()
        .map(|sample| (sample - mean).powi(2))
        .sum::<f64>()
        / count)
        .sqrt()
}

fn spatial_information(plane: &LumaPlane) -> f64 {
    if plane.width == 1 || plane.height == 1 {
        return 0.0;
    }
    let mut magnitudes = Vec::new();
    for y in 1..plane.height.saturating_sub(1) {
        for x in 1..plane.width.saturating_sub(1) {
            let at = |row: usize, col: usize| {
                row.checked_mul(plane.width)
                    .and_then(|start| start.checked_add(col))
                    .and_then(|index| plane.samples.get(index))
                    .copied()
            };
            let (
                Some(top_left),
                Some(top),
                Some(top_right),
                Some(left),
                Some(right),
                Some(bottom_left),
                Some(bottom),
                Some(bottom_right),
            ) = (
                at(y - 1, x - 1),
                at(y - 1, x),
                at(y - 1, x + 1),
                at(y, x - 1),
                at(y, x + 1),
                at(y + 1, x - 1),
                at(y + 1, x),
                at(y + 1, x + 1),
            )
            else {
                continue;
            };
            let horizontal =
                top_right + 2.0 * right + bottom_right - top_left - 2.0 * left - bottom_left;
            let vertical =
                bottom_left + 2.0 * bottom + bottom_right - top_left - 2.0 * top - top_right;
            magnitudes.push(horizontal.hypot(vertical));
        }
    }
    population_stddev(&magnitudes)
}

fn temporal_information(current: &LumaPlane, previous: &LumaPlane) -> Option<f64> {
    if (current.width, current.height) != (previous.width, previous.height) {
        return None;
    }
    Some(population_stddev(
        &current
            .samples
            .iter()
            .zip(&previous.samples)
            .map(|(current, previous)| current - previous)
            .collect::<Vec<_>>(),
    ))
}

fn fixed2(value: f64) -> String {
    if value.is_nan() {
        "nan".to_owned()
    } else {
        format!("{value:.2}")
    }
}

#[derive(Debug, Default)]
pub(crate) struct Filter {
    previous: Option<LumaPlane>,
}

impl Filter {
    fn step(&mut self, mut frame: Frame) -> Result<Frame> {
        let current = luma_plane(&frame).ok_or(Error::Unsupported(
            "siti: supported formats are gray8, yuv420p, yuv422p, and yuv444p",
        ))?;
        let si = spatial_information(&current);
        let ti = self
            .previous
            .as_ref()
            .map(|previous| {
                temporal_information(&current, previous).ok_or(Error::InvalidData(
                    "siti: frame dimensions changed within one filter instance",
                ))
            })
            .transpose()?
            .unwrap_or(0.0);
        frame.set_metadata("lavfi.siti.si", fixed2(si));
        frame.set_metadata("lavfi.siti.ti", fixed2(ti));
        self.previous = Some(current);
        Ok(frame)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        self.step(frame).map(FrameOut::One)
    }

    fn flush_state(&mut self) {
        self.previous = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let set = FormatSet {
        pixel_formats: Some(Constraint::OneOf(SUPPORTED_FORMATS.to_vec())),
        ..FormatSet::default()
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter::default())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_color::TransferCharacteristic;
    use vaco_frame::FramePool;

    fn step_frame(left: u8, right: u8, range: ColorRange) -> Frame {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 16, 16).unwrap();
        frame.color.range = range;
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    row[..8].fill(left);
                    row[8..].fill(right);
                }
            }
        }
        frame
    }

    fn patterned_frame(n: u8, range: ColorRange) -> Frame {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 16, 16).unwrap();
        frame.color.range = range;
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for (x, sample) in row.iter_mut().enumerate() {
                        let x = u16::try_from(x).unwrap();
                        let y = u16::try_from(y).unwrap();
                        *sample =
                            (16 + (17 * x + 29 * y + 3 * x * y + 11 * u16::from(n)) % 220) as u8;
                    }
                }
            }
        }
        frame
    }

    #[test]
    fn full_and_limited_range_follow_the_reference_integer_expansion() {
        let mut full = Filter::default();
        let full = full.step(step_frame(100, 120, ColorRange::Full)).unwrap();
        assert_eq!(full.metadata_get("lavfi.siti.si"), Some("27.99"));

        let mut limited = Filter::default();
        let limited = limited
            .step(step_frame(100, 120, ColorRange::Limited))
            .unwrap();
        assert_eq!(limited.metadata_get("lavfi.siti.si"), Some("33.59"));

        let mut unspecified = Filter::default();
        let unspecified = unspecified
            .step(step_frame(100, 120, ColorRange::Unspecified))
            .unwrap();
        assert_eq!(unspecified.metadata_get("lavfi.siti.si"), Some("33.59"));
    }

    #[test]
    fn transfer_tags_do_not_change_the_legacy_reference_score() {
        let mut bt709 = step_frame(100, 120, ColorRange::Limited);
        bt709.color.transfer = TransferCharacteristic::Bt709;
        let mut srgb = step_frame(100, 120, ColorRange::Limited);
        srgb.color.transfer = TransferCharacteristic::Iec61966_2_1;
        let mut pq = step_frame(100, 120, ColorRange::Limited);
        pq.color.transfer = TransferCharacteristic::Smpte2084;

        for frame in [bt709, srgb, pq] {
            let mut filter = Filter::default();
            let out = filter.step(frame).unwrap();
            assert_eq!(out.metadata_get("lavfi.siti.si"), Some("33.59"));
        }
    }

    #[test]
    fn stateful_temporal_information_uses_the_predecessor() {
        let mut filter = Filter::default();
        let first = filter.step(step_frame(0, 0, ColorRange::Full)).unwrap();
        let second = filter.step(step_frame(0, 100, ColorRange::Full)).unwrap();
        let third = filter.step(step_frame(0, 30, ColorRange::Full)).unwrap();
        assert_eq!(first.metadata_get("lavfi.siti.si"), Some("0.00"));
        assert_eq!(first.metadata_get("lavfi.siti.ti"), Some("0.00"));
        assert_eq!(second.metadata_get("lavfi.siti.si"), Some("139.97"));
        assert_eq!(second.metadata_get("lavfi.siti.ti"), Some("50.00"));
        assert_eq!(third.metadata_get("lavfi.siti.si"), Some("41.99"));
        assert_eq!(third.metadata_get("lavfi.siti.ti"), Some("35.00"));
    }

    #[test]
    fn textured_limited_range_sequence_matches_the_black_box_oracle() {
        let mut filter = Filter::default();
        let first = filter
            .step(patterned_frame(0, ColorRange::Limited))
            .unwrap();
        let second = filter
            .step(patterned_frame(1, ColorRange::Limited))
            .unwrap();
        let third = filter
            .step(patterned_frame(2, ColorRange::Limited))
            .unwrap();

        // Independent oracle: ffprobe through the same `geq` recurrence
        // reports (151.62, 0.00), (150.77, 71.78), (154.32, 47.24).
        assert_eq!(first.metadata_get("lavfi.siti.si"), Some("151.62"));
        assert_eq!(first.metadata_get("lavfi.siti.ti"), Some("0.00"));
        assert_eq!(second.metadata_get("lavfi.siti.si"), Some("150.77"));
        assert_eq!(second.metadata_get("lavfi.siti.ti"), Some("71.78"));
        assert_eq!(third.metadata_get("lavfi.siti.si"), Some("154.32"));
        assert_eq!(third.metadata_get("lavfi.siti.ti"), Some("47.24"));
    }

    #[test]
    fn flush_discards_the_temporal_predecessor() {
        let mut filter = Filter::default();
        let _ = filter.step(step_frame(0, 0, ColorRange::Full)).unwrap();
        filter.flush_state();
        let out = filter.step(step_frame(0, 100, ColorRange::Full)).unwrap();
        assert_eq!(out.metadata_get("lavfi.siti.ti"), Some("0.00"));
    }
}
