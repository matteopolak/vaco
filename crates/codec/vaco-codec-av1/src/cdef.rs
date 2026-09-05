//! AV1 §7.15 constrained directional enhancement, from `aom-av1-spec`.
//!
//! Kernels consume an immutable neighborhood so already filtered blocks cannot
//! feed later blocks. The two-pixel border is absent at frame edges, not extended.
#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "fixed-size CDEF neighborhoods and direction bins; parameters are validated before indexing"
)]

use vaco_core::{Error, Result};

/// One primary and secondary strength pair, before bit-depth scaling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Strength {
    /// Primary strength in 8-bit sample units, 0 through 15.
    pub primary: u8,
    /// Secondary strength in 8-bit sample units: 0, 1, 2, or 4.
    pub secondary: u8,
}

/// Retained `cdef_params()` syntax; zero index bits still selects entry zero.
#[derive(Debug, Clone, Default)]
pub struct CdefParams {
    /// False only when the specification disables CDEF for the frame.
    pub enabled: bool,
    /// `CdefDamping`, including the syntax's implicit offset of three.
    pub damping: u8,
    /// Luma strength entries selected per 64x64 unit.
    pub y: [Strength; 8],
    /// Shared chroma strength entries selected per 64x64 unit.
    pub uv: [Strength; 8],
}

/// Parameters for a single 4x4, 4x8, 8x4, or 8x8 filtering operation.
#[derive(Debug, Clone, Copy)]
pub struct FilterParams {
    /// Active width; output rows always have stride eight.
    pub width: usize,
    /// Active height.
    pub height: usize,
    /// Available two-pixel borders: left=1, right=2, top=4, bottom=8.
    pub edges: u8,
    /// AV1 sample precision: 8, 10, or 12.
    pub bit_depth: u8,
    /// Direction ordinal in the AV1 specification, 0 through 7.
    pub direction: u8,
    /// Primary strength after variance adjustment and bit-depth scaling.
    pub primary: u16,
    /// Secondary strength after bit-depth scaling.
    pub secondary: u16,
    /// Plane damping, including bit-depth scaling and chroma adjustment.
    pub damping: u8,
}

// §7.15.3; stored as (row, column), not Cartesian coordinates.
const DIRECTIONS: [[(i32, i32); 2]; 8] = [
    [(-1, 1), (-2, 2)],
    [(0, 1), (-1, 2)],
    [(0, 1), (0, 2)],
    [(0, 1), (1, 2)],
    [(1, 1), (2, 2)],
    [(1, 0), (2, 1)],
    [(1, 0), (2, 0)],
    [(1, 0), (2, -1)],
];

fn coefficient_shift(bit_depth: u8) -> Result<u8> {
    match bit_depth {
        8 | 10 | 12 => Ok(bit_depth - 8),
        _ => Err(Error::InvalidData("CDEF bit depth must be 8, 10, or 12")),
    }
}

/// Search eight directions using the luma block, returning direction and variance.
///
/// # Errors
/// Rejects invalid bit depth or samples outside that bit depth.
pub fn find_direction(block: &[u16; 64], bit_depth: u8) -> Result<(u8, u32)> {
    let shift = coefficient_shift(bit_depth)?;
    let mut partial = [[0i32; 15]; 8];
    let mut counts = [[0u32; 15]; 8];
    for (index, &sample) in block.iter().enumerate() {
        if sample >= 1 << bit_depth {
            return Err(Error::InvalidData("CDEF sample exceeds bit depth"));
        }
        let (i, j) = (index / 8, index % 8);
        let bins = [
            i + j,
            i + j / 2,
            i,
            3 + i - j / 2,
            7 + i - j,
            3 + j - i / 2,
            j,
            i / 2 + j,
        ];
        let value = i32::from(sample >> shift) - 128;
        for (direction, bin) in bins.into_iter().enumerate() {
            partial[direction][bin] += value;
            counts[direction][bin] += 1;
        }
    }
    let mut costs = [0i64; 8];
    for direction in 0..8 {
        for bin in 0..15 {
            let count = counts[direction][bin];
            if let Some(weight) = 840u32.checked_div(count) {
                let sum = i64::from(partial[direction][bin]);
                costs[direction] += sum * sum * i64::from(weight);
            }
        }
    }
    let mut best = 0;
    for direction in 1..8 {
        if costs[direction] > costs[best] {
            best = direction;
        }
    }
    Ok((
        u8::try_from(best).unwrap_or(0),
        u32::try_from((costs[best] - costs[(best + 4) & 7]) >> 10).unwrap_or(0),
    ))
}

fn constrain(difference: i32, threshold: u16, damping: u8) -> i32 {
    if threshold == 0 {
        return 0;
    }
    let shift = u32::from(damping).saturating_sub(threshold.ilog2());
    let magnitude = difference.abs();
    difference.signum() * magnitude.min((i32::from(threshold) - (magnitude >> shift)).max(0))
}

/// Filter a block at (2,2) in a 12x12 immutable neighborhood.
///
/// The returned 8x8 array has zeroes outside the active width and height.
/// Unavailable border samples are ignored, even when their stored value differs.
///
/// # Errors
/// Rejects malformed geometry, direction, strengths, damping, or sample precision.
pub fn filter_block(input: &[u16; 144], params: FilterParams) -> Result<[u16; 64]> {
    let shift = coefficient_shift(params.bit_depth)?;
    if !matches!(params.width, 4 | 8)
        || !matches!(params.height, 4 | 8)
        || params.direction > 7
        || params.edges > 15
        || params.damping > 12
        || params.primary > 15 << shift
        || params.secondary > 4 << shift
        || input.iter().any(|&sample| sample >= 1 << params.bit_depth)
    {
        return Err(Error::InvalidData("invalid CDEF block parameters"));
    }
    let mut output = [0; 64];
    let primary_taps = if (params.primary >> shift) & 1 == 0 {
        [4, 2]
    } else {
        [3, 3]
    };
    let x_min = if params.edges & 1 != 0 { -2 } else { 0 };
    let x_max =
        i32::try_from(params.width).unwrap_or(0) + if params.edges & 2 != 0 { 2 } else { 0 };
    let y_min = if params.edges & 4 != 0 { -2 } else { 0 };
    let y_max =
        i32::try_from(params.height).unwrap_or(0) + if params.edges & 8 != 0 { 2 } else { 0 };
    for y in 0..params.height {
        for x in 0..params.width {
            let current = i32::from(input[(y + 2) * 12 + x + 2]);
            let (mut minimum, mut maximum, mut sum) = (current, current, 0);
            for (distance, primary_tap) in primary_taps.into_iter().enumerate() {
                for sign in [-1, 1] {
                    for (direction, strength, tap) in [
                        (params.direction, params.primary, primary_tap),
                        (
                            (params.direction + 6) & 7,
                            params.secondary,
                            2 - i32::try_from(distance).unwrap_or(0),
                        ),
                        (
                            (params.direction + 2) & 7,
                            params.secondary,
                            2 - i32::try_from(distance).unwrap_or(0),
                        ),
                    ] {
                        let (dy, dx) = DIRECTIONS[usize::from(direction)][distance];
                        let nx = i32::try_from(x).unwrap_or(0) + sign * dx;
                        let ny = i32::try_from(y).unwrap_or(0) + sign * dy;
                        if nx < x_min || nx >= x_max || ny < y_min || ny >= y_max {
                            continue;
                        }
                        let offset = usize::try_from((ny + 2) * 12 + nx + 2).unwrap_or(0);
                        let neighbor = i32::from(input[offset]);
                        minimum = minimum.min(neighbor);
                        maximum = maximum.max(neighbor);
                        sum += tap * constrain(neighbor - current, strength, params.damping);
                    }
                }
            }
            let filtered = current + ((8 + sum - i32::from(sum < 0)) >> 4);
            output[y * 8 + x] = u16::try_from(filtered.clamp(minimum, maximum)).unwrap_or(0);
        }
    }
    Ok(output)
}

/// Luma primary strength adjustment from directional variance, §7.15.1.
#[must_use]
pub fn adjust_strength(primary: u16, variance: u32) -> u16 {
    if variance == 0 {
        return 0;
    }
    let scaled = variance >> 6;
    let log = if scaled == 0 {
        0
    } else {
        scaled.ilog2().min(12)
    };
    u16::try_from((u32::from(primary) * (4 + log) + 8) >> 4).unwrap_or(0)
}

/// Map a luma direction to a chroma plane's sampling grid, §7.15.1.
#[must_use]
pub fn chroma_direction(direction: u8, sub_x: bool, sub_y: bool) -> u8 {
    match (sub_x, sub_y) {
        (true, false) => [7, 0, 2, 4, 5, 6, 6, 6][usize::from(direction & 7)],
        (false, true) => [1, 2, 2, 2, 3, 4, 6, 0][usize::from(direction & 7)],
        _ => direction & 7,
    }
}
