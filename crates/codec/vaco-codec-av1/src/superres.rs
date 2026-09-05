//! AV1 §7.16 scalar super-resolution, from `aom-av1-spec`.
//!
//! The reconstruction planes remain Mi-padded until this stage. Sampling
//! phase is derived from the visible coded width, while the eight-tap edge
//! extension clamps against the padded plane width; those two bounds are
//! deliberately distinct in the specification.
#![allow(
    clippy::integer_division,
    reason = "AV1 §7.16 specifies these fixed-point integer divisions"
)]

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::framebuf::{Picture, Plane};

const SCALE_BITS: u32 = 14;
const SCALE_MASK: i64 = (1 << SCALE_BITS) - 1;
const EXTRA_BITS: u32 = 8;
const FILTER_OFFSET: i64 = 3;
const FILTER_BITS: u32 = 7;

// AV1 §7.16, Upscale_Filter[64][8]. The independently generated dav1d
// fixture tests this table and the fixed-point phase arithmetic together.
const UPSCALE_FILTER: [[i16; 8]; 64] = [
    [0, 0, 0, 128, 0, 0, 0, 0],
    [0, 0, -1, 128, 2, -1, 0, 0],
    [0, 1, -3, 127, 4, -2, 1, 0],
    [0, 1, -4, 127, 6, -3, 1, 0],
    [0, 2, -6, 126, 8, -3, 1, 0],
    [0, 2, -7, 125, 11, -4, 1, 0],
    [-1, 2, -8, 125, 13, -5, 2, 0],
    [-1, 3, -9, 124, 15, -6, 2, 0],
    [-1, 3, -10, 123, 18, -6, 2, -1],
    [-1, 3, -11, 122, 20, -7, 3, -1],
    [-1, 4, -12, 121, 22, -8, 3, -1],
    [-1, 4, -13, 120, 25, -9, 3, -1],
    [-1, 4, -14, 118, 28, -9, 3, -1],
    [-1, 4, -15, 117, 30, -10, 4, -1],
    [-1, 5, -16, 116, 32, -11, 4, -1],
    [-1, 5, -16, 114, 35, -12, 4, -1],
    [-1, 5, -17, 112, 38, -12, 4, -1],
    [-1, 5, -18, 111, 40, -13, 5, -1],
    [-1, 5, -18, 109, 43, -14, 5, -1],
    [-1, 6, -19, 107, 45, -14, 5, -1],
    [-1, 6, -19, 105, 48, -15, 5, -1],
    [-1, 6, -19, 103, 51, -16, 5, -1],
    [-1, 6, -20, 101, 53, -16, 6, -1],
    [-1, 6, -20, 99, 56, -17, 6, -1],
    [-1, 6, -20, 97, 58, -17, 6, -1],
    [-1, 6, -20, 95, 61, -18, 6, -1],
    [-2, 7, -20, 93, 64, -18, 6, -2],
    [-2, 7, -20, 91, 66, -19, 6, -1],
    [-2, 7, -20, 88, 69, -19, 6, -1],
    [-2, 7, -20, 86, 71, -19, 6, -1],
    [-2, 7, -20, 84, 74, -20, 7, -2],
    [-2, 7, -20, 81, 76, -20, 7, -1],
    [-2, 7, -20, 79, 79, -20, 7, -2],
    [-1, 7, -20, 76, 81, -20, 7, -2],
    [-2, 7, -20, 74, 84, -20, 7, -2],
    [-1, 6, -19, 71, 86, -20, 7, -2],
    [-1, 6, -19, 69, 88, -20, 7, -2],
    [-1, 6, -19, 66, 91, -20, 7, -2],
    [-2, 6, -18, 64, 93, -20, 7, -2],
    [-1, 6, -18, 61, 95, -20, 6, -1],
    [-1, 6, -17, 58, 97, -20, 6, -1],
    [-1, 6, -17, 56, 99, -20, 6, -1],
    [-1, 6, -16, 53, 101, -20, 6, -1],
    [-1, 5, -16, 51, 103, -19, 6, -1],
    [-1, 5, -15, 48, 105, -19, 6, -1],
    [-1, 5, -14, 45, 107, -19, 6, -1],
    [-1, 5, -14, 43, 109, -18, 5, -1],
    [-1, 5, -13, 40, 111, -18, 5, -1],
    [-1, 4, -12, 38, 112, -17, 5, -1],
    [-1, 4, -12, 35, 114, -16, 5, -1],
    [-1, 4, -11, 32, 116, -16, 5, -1],
    [-1, 4, -10, 30, 117, -15, 4, -1],
    [-1, 3, -9, 28, 118, -14, 4, -1],
    [-1, 3, -9, 25, 120, -13, 4, -1],
    [-1, 3, -8, 22, 121, -12, 4, -1],
    [-1, 3, -7, 20, 122, -11, 3, -1],
    [-1, 2, -6, 18, 123, -10, 3, -1],
    [0, 2, -6, 15, 124, -9, 3, -1],
    [0, 2, -5, 13, 125, -8, 2, -1],
    [0, 1, -4, 11, 125, -7, 2, 0],
    [0, 1, -3, 8, 126, -6, 2, 0],
    [0, 1, -3, 6, 127, -4, 1, 0],
    [0, 1, -2, 4, 127, -3, 1, 0],
    [0, 0, -1, 2, 128, -1, 0, 0],
];

/// Visible geometry and sample precision for one plane.
#[derive(Clone, Copy, Debug)]
pub struct PlaneConfig {
    /// Coded, unpadded width used to derive the sampling phase.
    pub visible_width: usize,
    /// Final width after super-resolution.
    pub output_width: usize,
    /// Visible plane rows to resample.
    pub height: usize,
    /// AV1 sample precision: 8, 10, or 12 bits.
    pub bit_depth: u8,
}

/// Luma geometry and colour layout for a reconstructed picture.
#[derive(Clone, Copy, Debug)]
pub struct PictureConfig {
    /// Coded luma width before super-resolution.
    pub coded_width: usize,
    /// Final luma width after super-resolution.
    pub upscaled_width: usize,
    /// Visible luma height.
    pub coded_height: usize,
    /// Horizontal chroma subsampling flag.
    pub subsampling_x: bool,
    /// Vertical chroma subsampling flag.
    pub subsampling_y: bool,
    /// True when U and V are absent.
    pub monochrome: bool,
    /// AV1 sample precision: 8, 10, or 12 bits.
    pub bit_depth: u8,
}

fn as_i64(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::InvalidData("AV1 superres dimension exceeds i64"))
}

fn validate(config: PlaneConfig, input: &Plane) -> Result<()> {
    if !matches!(config.bit_depth, 8 | 10 | 12) {
        return Err(Error::InvalidData(
            "AV1 superres bit depth must be 8, 10, or 12",
        ));
    }
    if config.visible_width == 0
        || config.output_width <= config.visible_width
        || config.height == 0
        || config.visible_width > input.width()
        || config.height > input.height()
    {
        return Err(Error::InvalidData("invalid AV1 superres plane geometry"));
    }
    Ok(())
}

fn subpel_start(visible_width: i64, output_width: i64, step: i64) -> i64 {
    let error = output_width * step - (visible_width << SCALE_BITS);
    let start = (-((output_width - visible_width) << (SCALE_BITS - 1)) + output_width / 2)
        / output_width
        + (1 << (EXTRA_BITS - 1))
        - error / 2;
    start & SCALE_MASK
}

/// Apply the specification's horizontal eight-tap filter to one plane.
///
/// `input.width()` is the Mi-padded reconstruction width used for edge
/// extension; [`PlaneConfig::visible_width`] is intentionally smaller at a
/// right edge that needed padding during tile reconstruction.
///
/// # Errors
/// Returns an error for invalid geometry, precision, limits, or an input sample
/// outside the declared bit depth.
pub fn upscale_plane(input: &Plane, config: PlaneConfig, budget: &mut Budget) -> Result<Plane> {
    validate(config, input)?;
    let visible_width = as_i64(config.visible_width)?;
    let output_width = as_i64(config.output_width)?;
    let padded_width = as_i64(input.width())?;
    let step = ((visible_width << SCALE_BITS) + output_width / 2) / output_width;
    let initial_subpel_x = subpel_start(visible_width, output_width, step);
    let max_sample = (1i64 << config.bit_depth) - 1;
    let mut output = Plane::new(budget, config.output_width, config.height)?;

    for y in 0..config.height {
        budget.consume_fuel(u64::try_from(config.output_width).unwrap_or(u64::MAX))?;
        for x in 0..config.output_width {
            let x = as_i64(x)?;
            let src_x = -(1 << SCALE_BITS) + initial_subpel_x + x * step;
            let src_x_px = src_x >> SCALE_BITS;
            let phase = usize::try_from((src_x & SCALE_MASK) >> EXTRA_BITS)
                .map_err(|_| Error::InvalidData("AV1 superres phase exceeds table"))?;
            let taps = UPSCALE_FILTER
                .get(phase)
                .ok_or(Error::InvalidData("AV1 superres phase exceeds table"))?;
            let mut sum = 0i64;
            for (tap, &coefficient) in taps.iter().enumerate() {
                let tap = as_i64(tap)?;
                let sample_x = (src_x_px + tap - FILTER_OFFSET).clamp(0, padded_width - 1);
                let sample_x = usize::try_from(sample_x)
                    .map_err(|_| Error::InvalidData("AV1 superres sample index exceeds usize"))?;
                let index = y
                    .checked_mul(input.width())
                    .and_then(|base| base.checked_add(sample_x))
                    .ok_or(Error::InvalidData("AV1 superres source index overflows"))?;
                let sample = input
                    .as_slice()
                    .get(index)
                    .copied()
                    .ok_or(Error::InvalidData(
                        "AV1 superres source index exceeds plane",
                    ))?;
                if i64::from(sample) > max_sample {
                    return Err(Error::InvalidData("AV1 superres sample exceeds bit depth"));
                }
                sum += i64::from(sample) * i64::from(coefficient);
            }
            let rounded = (sum + (1 << (FILTER_BITS - 1))) >> FILTER_BITS;
            output.set(
                usize::try_from(x)
                    .map_err(|_| Error::InvalidData("AV1 superres output index exceeds usize"))?,
                y,
                u16::try_from(rounded.clamp(0, max_sample)).unwrap_or(u16::MAX),
            );
        }
    }
    Ok(output)
}

/// Super-resolve all present planes of a reconstructed picture.
///
/// Chroma widths and heights follow the specification's `Round2` division,
/// implemented as ceiling division for a positive integer dimension.
///
/// # Errors
/// Propagates allocation, fuel, precision, geometry, and sample errors from
/// [`upscale_plane`].
pub fn upscale_picture(
    input: &Picture,
    config: PictureConfig,
    budget: &mut Budget,
) -> Result<Picture> {
    let luma = PlaneConfig {
        visible_width: config.coded_width,
        output_width: config.upscaled_width,
        height: config.coded_height,
        bit_depth: config.bit_depth,
    };
    let y = upscale_plane(&input.y, luma, budget)?;
    if config.monochrome {
        return Ok(Picture {
            y,
            u: None,
            v: None,
        });
    }
    let shift_x = usize::from(config.subsampling_x);
    let shift_y = usize::from(config.subsampling_y);
    let chroma = PlaneConfig {
        visible_width: config.coded_width.div_ceil(1 << shift_x),
        output_width: config.upscaled_width.div_ceil(1 << shift_x),
        height: config.coded_height.div_ceil(1 << shift_y),
        bit_depth: config.bit_depth,
    };
    let u = upscale_plane(
        input
            .u
            .as_ref()
            .ok_or(Error::InvalidData("AV1 superres missing U plane"))?,
        chroma,
        budget,
    )?;
    let v = upscale_plane(
        input
            .v
            .as_ref()
            .ok_or(Error::InvalidData("AV1 superres missing V plane"))?,
        chroma,
        budget,
    )?;
    Ok(Picture {
        y,
        u: Some(u),
        v: Some(v),
    })
}
