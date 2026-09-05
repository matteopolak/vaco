//! AV1 §7.17 scalar loop restoration, with separate pre-CDEF stripe borders.
//!
//! The decoder supplies tile entropy syntax and preserves the separate source
//! images required at restoration stripe boundaries.
#![allow(
    clippy::integer_division,
    reason = "AV1 section 7.17 specifies truncating integer division"
)]

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::framebuf::Plane;

/// A frame-level mode; the two-bit header mapping differs from unit symbol order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RestorationType {
    /// Preserve CDEF output for this plane.
    #[default]
    None,
    /// Each unit can select either filter or no restoration.
    Switchable,
    /// Each unit can select Wiener or no restoration.
    Wiener,
    /// Each unit can select self-guided or no restoration.
    SelfGuided,
}

/// Retained `lr_params()` values, AV1 §5.9.20.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRestoration {
    /// Y, U, V restoration modes, including the header's remapping.
    pub types: [RestorationType; 3],
    /// Unit side lengths in samples of each plane; zero when disabled.
    pub unit_size: [usize; 3],
}

impl FrameRestoration {
    /// Whether any plane needs restoration-unit syntax and filtering.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.types.iter().any(|mode| *mode != RestorationType::None)
    }
}

/// Decoded parameters for one restoration unit, AV1 §5.11.58.
#[derive(Clone, Copy, Debug)]
pub enum RestorationUnit {
    /// Preserve the input pixels in this unit.
    None,
    /// Symmetric taps, outermost first; vertical taps precede horizontal in syntax.
    Wiener {
        /// The three transmitted vertical coefficients.
        vertical: [i16; 3],
        /// The three transmitted horizontal coefficients.
        horizontal: [i16; 3],
    },
    /// The parameter-set index and two transmitted projection coefficients.
    SelfGuided {
        /// `lr_sgr_set`, in `0..16`.
        set: u8,
        /// `LrSgrXqd`, before deriving the second filtered-output weight.
        xqd: [i16; 2],
    },
}

impl RestorationUnit {
    fn validate(self) -> Result<()> {
        let valid = match self {
            Self::None => true,
            Self::Wiener {
                vertical,
                horizontal,
            } => [vertical, horizontal].iter().all(|taps| {
                taps.iter()
                    .zip([(-5, 10), (-23, 8), (-17, 46)])
                    .all(|(&tap, (min, max))| (min..=max).contains(&tap))
            }),
            Self::SelfGuided {
                set,
                xqd: [first, second],
            } => set < 16 && (-96..=31).contains(&first) && (-32..=95).contains(&second),
        };
        if !valid {
            return Err(Error::InvalidData(
                "AV1 restoration coefficient out of range",
            ));
        }
        Ok(())
    }
}

/// Visible plane geometry; padding in the reconstruction buffer is never sampled.
#[derive(Clone, Copy, Debug)]
pub struct PlaneConfig {
    /// Visible width, after superresolution and chroma rounding.
    pub width: usize,
    /// Visible height, after chroma rounding.
    pub height: usize,
    /// AV1 sample precision: 8, 10 or 12.
    pub bit_depth: u8,
    /// Restoration unit side length, 32, 64, 128 or 256 plane samples.
    pub unit_size: usize,
    /// Whether plane rows represent two luma rows.
    pub subsampling_y: bool,
}

impl PlaneConfig {
    /// Unit columns and rows, merging tails shorter than half a unit (§5.11.57).
    #[must_use]
    pub fn unit_counts(self) -> (usize, usize) {
        let count = |extent: usize| {
            extent
                .saturating_add(self.unit_size / 2)
                .checked_div(self.unit_size)
                .unwrap_or(0)
                .max(1)
        };
        (count(self.width), count(self.height))
    }

    /// Row-major unit index; vertical unit boundaries move up eight luma rows.
    #[must_use]
    pub fn unit_index(self, x: usize, y: usize) -> usize {
        let (cols, rows) = self.unit_counts();
        let row = y
            .saturating_add(8 >> u32::from(self.subsampling_y))
            .checked_div(self.unit_size)
            .unwrap_or(0)
            .min(rows - 1);
        let col = x.checked_div(self.unit_size).unwrap_or(0).min(cols - 1);
        row.saturating_mul(cols).saturating_add(col)
    }

    fn validate(self, before: &Plane, after: &Plane, units: &[RestorationUnit]) -> Result<()> {
        if !matches!(self.bit_depth, 8 | 10 | 12)
            || !matches!(self.unit_size, 32 | 64 | 128 | 256)
            || self.width == 0
            || self.height == 0
            || self.width > i32::MAX as usize - 8
            || self.height > i32::MAX as usize - 64
            || self.width > before.width()
            || self.width > after.width()
            || self.height > before.height()
            || self.height > after.height()
        {
            return Err(Error::InvalidData(
                "AV1 restoration plane geometry or bit depth",
            ));
        }
        let (cols, rows) = self.unit_counts();
        if cols.checked_mul(rows) != Some(units.len()) {
            return Err(Error::InvalidData("AV1 restoration unit count"));
        }
        for unit in units {
            unit.validate()?;
        }
        let max = (1u16 << self.bit_depth) - 1;
        if [before, after].iter().any(|plane| {
            (0..self.height)
                .any(|y| (0..self.width).any(|x| plane.get_clamped(coord(x), coord(y)) > max))
        }) {
            return Err(Error::InvalidData(
                "AV1 restoration sample exceeds bit depth",
            ));
        }
        Ok(())
    }
}

/// Restore a visible plane using the unit map and pre-CDEF boundary pixels.
///
/// Both inputs must already be upscaled. Output has the visible dimensions and
/// owns its budgeted allocation; it never modifies either source (§7.17).
///
/// # Errors
/// Rejects invalid geometry, samples, parameters, unit counts or exhausted budget.
pub fn restore_plane(
    before_cdef: &Plane,
    after_cdef: &Plane,
    config: PlaneConfig,
    units: &[RestorationUnit],
    budget: &mut Budget,
) -> Result<Plane> {
    config.validate(before_cdef, after_cdef, units)?;
    let mut output = Plane::new(budget, config.width, config.height)?;
    for y in 0..config.height {
        let source = Source::new(before_cdef, after_cdef, config, y);
        for x in 0..config.width {
            let unit = units
                .get(config.unit_index(x, y))
                .ok_or(Error::InvalidData("AV1 restoration unit index"))?;
            let value = match *unit {
                RestorationUnit::None => i64::from(after_cdef.get_clamped(coord(x), coord(y))),
                RestorationUnit::Wiener {
                    vertical,
                    horizontal,
                } => wiener(&source, coord(x), coord(y), vertical, horizontal),
                RestorationUnit::SelfGuided { set, xqd } => {
                    self_guided(&source, coord(x), coord(y), set, xqd)
                }
            };
            output.set(x, y, value.clamp(0, (1 << config.bit_depth) - 1) as u16);
        }
    }
    Ok(output)
}

struct Source<'a> {
    before: &'a Plane,
    after: &'a Plane,
    config: PlaneConfig,
    stripe_start: i32,
    stripe_end: i32,
}

impl<'a> Source<'a> {
    fn new(before: &'a Plane, after: &'a Plane, config: PlaneConfig, y: usize) -> Self {
        let sub_y = u32::from(config.subsampling_y);
        let stripe_height = 64 >> sub_y;
        let offset = 8 >> sub_y;
        let stripe = (y + offset) / stripe_height;
        let stripe_start = coord(stripe * stripe_height) - coord(offset);
        Self {
            before,
            after,
            config,
            stripe_start,
            stripe_end: stripe_start + coord(stripe_height) - 1,
        }
    }

    fn get(&self, x: i32, y: i32) -> i64 {
        let x = x.clamp(0, coord(self.config.width) - 1);
        let y = y.clamp(0, coord(self.config.height) - 1);
        let sample = if y < self.stripe_start {
            self.before.get_clamped(x, y.max(self.stripe_start - 2))
        } else if y > self.stripe_end {
            self.before.get_clamped(x, y.min(self.stripe_end + 2))
        } else {
            self.after.get_clamped(x, y)
        };
        i64::from(sample)
    }
}

fn coord(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn round2(value: i64, shift: u8) -> i64 {
    if shift == 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

fn taps([a, b, c]: [i16; 3]) -> [i64; 7] {
    let (a, b, c) = (i64::from(a), i64::from(b), i64::from(c));
    [a, b, c, 128 - 2 * (a + b + c), c, b, a]
}

fn wiener(source: &Source<'_>, x: i32, y: i32, vertical: [i16; 3], horizontal: [i16; 3]) -> i64 {
    let depth = source.config.bit_depth;
    let round_h = if depth == 12 { 5 } else { 3 };
    let offset = 1 << (depth + 7 - round_h - 1);
    let limit = (1 << (depth + 1 + 7 - round_h)) - 1;
    let horizontal = taps(horizontal);
    let sum = taps(vertical)
        .iter()
        .enumerate()
        .map(|(row, &weight)| {
            let h: i64 = horizontal
                .iter()
                .enumerate()
                .map(|(col, &tap)| tap * source.get(x + coord(col) - 3, y + coord(row) - 3))
                .sum();
            weight * round2(h, round_h).clamp(-offset, limit - offset)
        })
        .sum();
    round2(sum, 14 - round_h)
}

/// `Sgr_Params`, AV1 §7.17.3, arranged as radius/epsilon pairs for each pass.
const SGR_PARAMS: [[(i32, i64); 2]; 16] = [
    [(2, 12), (1, 4)],
    [(2, 15), (1, 6)],
    [(2, 18), (1, 8)],
    [(2, 21), (1, 9)],
    [(2, 24), (1, 10)],
    [(2, 29), (1, 11)],
    [(2, 36), (1, 12)],
    [(2, 45), (1, 13)],
    [(2, 56), (1, 14)],
    [(2, 68), (1, 15)],
    [(0, 0), (1, 5)],
    [(0, 0), (1, 8)],
    [(0, 0), (1, 11)],
    [(0, 0), (1, 14)],
    [(2, 30), (0, 0)],
    [(2, 75), (0, 0)],
];

/// Whether a self-guided parameter set codes a projection coefficient for a pass.
#[must_use]
pub(crate) fn sgr_uses_pass(set: u8, pass: usize) -> bool {
    SGR_PARAMS
        .get(usize::from(set))
        .and_then(|passes| passes.get(pass))
        .is_some_and(|(radius, _)| *radius != 0)
}

fn box_ab(source: &Source<'_>, x: i32, y: i32, radius: i32, epsilon: i64) -> (i64, i64) {
    let n = i64::from((2 * radius + 1).pow(2));
    let n2e = n * n * epsilon;
    let scale = ((1 << 20) + n2e / 2) / n2e;
    let (mut sum_sq, mut sum) = (0, 0);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sample = source.get(x + dx, y + dy);
            sum_sq += sample * sample;
            sum += sample;
        }
    }
    let a = round2(sum_sq, 2 * (source.config.bit_depth - 8));
    let d = round2(sum, source.config.bit_depth - 8);
    let variance = (a * n - d * d).max(0);
    let z = round2(variance * scale, 20);
    let a2 = if z >= 255 {
        256
    } else if z == 0 {
        1
    } else {
        ((z << 8) + z / 2) / (z + 1)
    };
    let reciprocal = ((1 << 12) + n / 2) / n;
    (a2, round2((256 - a2) * sum * reciprocal, 12))
}

fn guided_pass(source: &Source<'_>, x: i32, y: i32, pass: usize, radius: i32, epsilon: i64) -> i64 {
    let original = i64::from(source.after.get_clamped(x, y));
    if radius == 0 {
        return original << 4;
    }
    let (mut a, mut b) = (0, 0);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let weight = if pass == 0 {
                if (y + dy) & 1 == 0 {
                    0
                } else if dx == 0 {
                    6
                } else {
                    5
                }
            } else if dx == 0 || dy == 0 {
                4
            } else {
                3
            };
            if weight != 0 {
                let (aa, bb) = box_ab(source, x + dx, y + dy, radius, epsilon);
                a += weight * aa;
                b += weight * bb;
            }
        }
    }
    let shift = if pass == 0 && y & 1 != 0 { 4 } else { 5 };
    round2(a * original + b, 8 + shift - 4)
}

fn self_guided(source: &Source<'_>, x: i32, y: i32, set: u8, [w0, w1]: [i16; 2]) -> i64 {
    let params = SGR_PARAMS
        .get(usize::from(set))
        .copied()
        .unwrap_or([(0, 0); 2]);
    let [(r0, e0), (r1, e1)] = params;
    let (w0, w1) = (i64::from(w0), i64::from(w1));
    let original = i64::from(source.after.get_clamped(x, y)) << 4;
    let first = guided_pass(source, x, y, 0, r0, e0);
    let second = guided_pass(source, x, y, 1, r1, e1);
    round2(w0 * first + w1 * original + (128 - w0 - w1) * second, 11)
}
