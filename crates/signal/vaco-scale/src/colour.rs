//! Colour conversion: range, matrix, and the fixed-point forms of both.
//!
//! `vaco-color` owns the vocabulary and the `f64` matrices; this module owns the
//! decision of *which* matrix and the quantisation into integers. Nothing here
//! re-derives a coefficient that crate already knows.
//!
//! # The two shifts, and where they came from
//!
//! Both were recovered by probing the reference binary and are exact for 8-bit
//! conversions: every one of 65536 `(Y, V)` pairs and 60000 random `(R, G, B)`
//! triples reproduces byte for byte. The commands are in
//! `docs/signal/vaco-scale.md`.
//!
//! | Direction | Shift | Rounding constant |
//! |---|---|---|
//! | `Y'CbCr` to `R'G'B'` | 13 | `1 << 12` |
//! | `R'G'B'` to `Y'CbCr` | 15 | `(1 << 14) + (1 << 8)` |
//!
//! The second constant is `half + half/64`, i.e. the residue of a two-stage
//! `>> 9` then `>> 6`. It is worth about 1/128 of an output LSB and is
//! reproduced because D6 makes byte-identical output the contract, not because
//! it is principled.
//!
//! # The reference's own out-of-range behaviour is NOT reproduced
//!
//! For `Y'CbCr` to `R'G'B'`, a pre-clip value of 512 or more makes the reference emit
//! **0** rather than 255 — a table overrun, reachable from ordinary out-of-gamut
//! chroma (`Y = 225, U = 255` at BT.709 limited range is enough). We saturate
//! correctly. See [`crate::REFERENCE_CLIP_DIVERGENCE`] and the test that pins it.

use std::sync::Arc;

use vaco_color::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::options::{RenderingIntent, ScaleOptions};
use crate::spec::ImageSpec;

/// Fixed-point shift for a conversion whose output is `R'G'B'`.
pub const YUV_TO_RGB_SHIFT: u8 = 13;
/// Fixed-point shift for a conversion whose output is `Y'CbCr`.
pub const RGB_TO_YUV_SHIFT: u8 = 15;

/// Rounding constant for a shift, matching the reference per direction.
const fn rounding(shift: u8, yuv_out: bool) -> i64 {
    let half = 1i64 << (shift - 1);
    if yuv_out {
        // Measured: `half + half/64`. See the module docs.
        half + (half >> 6)
    } else {
        half
    }
}

/// A 3×3 affine transform on component code values, in fixed point.
///
/// `out[i] = clip((Σ_j m[i][j]·in[j] + bias[i]) >> shift, 0, max)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Affine {
    /// Rows are output channels 0..3, columns input channels 0..3.
    pub m: [[i32; 3]; 3],
    /// Per-output constant, already carrying the rounding term and both
    /// endpoints' offsets.
    pub bias: [i64; 3],
    /// Right shift applied to the accumulator.
    pub shift: u8,
    /// Upper clamp, i.e. `(1 << work_depth) - 1`.
    pub max: i32,
}

/// A high-precision colour-management stage.
///
/// Range expansion and the Y'CbCr endpoint matrices are evaluated directly in
/// `f64` here, rather than composing the existing fixed-point [`Affine`]
/// stages.  Transfer conversion and primary conversion are nonlinear, so
/// quantising between either operation changes the result measurably.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatTransform {
    src_space: Space,
    dst_space: Space,
    src_range: ColorRange,
    dst_range: ColorRange,
    src_matrix: MatrixCoefficients,
    dst_matrix: MatrixCoefficients,
    src_primaries: ColorPrimaries,
    dst_primaries: ColorPrimaries,
    src_transfer: TransferCharacteristic,
    dst_transfer: TransferCharacteristic,
    primary_matrix: [[f64; 3]; 3],
    lut: Option<Arc<Lut3D>>,
    depth: u8,
}

impl FloatTransform {
    /// Convert one coded pixel without intermediate quantisation.
    #[must_use]
    pub fn apply(&self, px: [i32; 3]) -> [i32; 3] {
        let source = normalise(px, self.src_space, self.src_range, self.depth);
        let nonlinear = match self.src_space {
            Space::Rgb => source,
            Space::Gray => [source[0]; 3],
            Space::YCbCr => self
                .src_matrix
                .ycbcr_to_rgb_with(self.src_primaries)
                .map_or(source, |m| mul3v(m, source)),
        };
        let mapped = if let Some(lut) = &self.lut {
            // HDR transfer functions are steep near black.  Lattice axes are
            // therefore coded R'G'B' values, not linear-light values: a 33³
            // grid resolves PQ shadow detail before its nonlinear decode.
            lut.sample(nonlinear)
        } else {
            let linear = nonlinear.map(|v| self.src_transfer.decode(v).unwrap_or(v));
            mul3v(self.primary_matrix, linear)
        };
        let encoded = mapped.map(|v| self.dst_transfer.encode(v).unwrap_or(v));
        let destination = match self.dst_space {
            Space::Rgb => encoded,
            Space::Gray => self
                .dst_matrix
                .rgb_to_ycbcr_with(self.dst_primaries)
                .map_or(encoded, |m| mul3v(m, encoded)),
            Space::YCbCr => self
                .dst_matrix
                .rgb_to_ycbcr_with(self.dst_primaries)
                .map_or(encoded, |m| mul3v(m, encoded)),
        };
        quantise(destination, self.dst_space, self.dst_range, self.depth)
    }
}

impl Affine {
    /// Apply to one pixel. The scalar reference every kernel is checked against.
    #[must_use]
    #[inline]
    pub fn apply(&self, px: [i32; 3]) -> [i32; 3] {
        let mut out = [0i32; 3];
        for i in 0..3 {
            let Some(row) = self.m.get(i) else { continue };
            let mut acc = self.bias.get(i).copied().unwrap_or(0);
            for j in 0..3 {
                let c = row.get(j).copied().unwrap_or(0);
                let v = px.get(j).copied().unwrap_or(0);
                acc += i64::from(c) * i64::from(v);
            }
            let v = acc >> self.shift;
            if let Some(slot) = out.get_mut(i) {
                *slot = v.clamp(0, i64::from(self.max)) as i32;
            }
        }
        out
    }
}

/// What the colour stage has to do between unpack and pack.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorStage {
    /// Nothing: the code values mean the same thing on both sides.
    None,
    /// A 3×3 affine on channels 0..2; channel 3 passes through.
    Affine(Affine),
    /// Nonlinear transfer/primaries conversion, evaluated in `f64`.
    Float(FloatTransform),
}

/// A bounded RGB lattice evaluated with tetrahedral interpolation.
///
/// Tone and gamut transforms are constructed once per plan, so this type keeps
/// their nonlinear work out of the per-pixel hot path while preserving the
/// neutral axis shared by every tetrahedron in the cube decomposition.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut3D {
    size: usize,
    values: Vec<[f64; 3]>,
}

impl Lut3D {
    /// Construct a bounded RGB lattice in red-fastest order.
    ///
    /// The constructor accepts two through sixty-five samples per edge: a
    /// two-point lattice is useful for externally supplied calibration cubes,
    /// while tone/gamut plans select the tighter 9..=65 range themselves.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidData`] when the grid shape or a sample is not
    /// finite, rather than allowing malformed caller data to produce NaNs in a
    /// conversion plan.
    pub fn from_values(size: usize, values: Vec<[f64; 3]>) -> Result<Self> {
        let expected = size.saturating_mul(size).saturating_mul(size);
        if !(2..=65).contains(&size)
            || values.len() != expected
            || values.iter().flatten().any(|value| !value.is_finite())
        {
            return Err(Error::InvalidData("invalid 3D LUT lattice"));
        }
        Ok(Self { size, values })
    }

    /// Evaluate the lattice in normalised destination-linear RGB.
    #[must_use]
    pub fn sample(&self, rgb: [f64; 3]) -> [f64; 3] {
        if self.size < 2 {
            return self.values.first().copied().unwrap_or([0.0; 3]);
        }
        let scale = (self.size - 1) as f64;
        let p = rgb.map(|v| v.clamp(0.0, 1.0) * scale);
        let base = p.map(|v| v.floor() as usize);
        let f = std::array::from_fn(|i| {
            p.get(i).copied().unwrap_or(0.0) - base.get(i).copied().unwrap_or(0) as f64
        });
        let c000 = self.at(base[0], base[1], base[2]);
        let c100 = self.at(base[0].saturating_add(1), base[1], base[2]);
        let c010 = self.at(base[0], base[1].saturating_add(1), base[2]);
        let c001 = self.at(base[0], base[1], base[2].saturating_add(1));
        let c110 = self.at(
            base[0].saturating_add(1),
            base[1].saturating_add(1),
            base[2],
        );
        let c101 = self.at(
            base[0].saturating_add(1),
            base[1],
            base[2].saturating_add(1),
        );
        let c011 = self.at(
            base[0],
            base[1].saturating_add(1),
            base[2].saturating_add(1),
        );
        let c111 = self.at(
            base[0].saturating_add(1),
            base[1].saturating_add(1),
            base[2].saturating_add(1),
        );
        tetrahedral(c000, c100, c010, c001, c110, c101, c011, c111, f)
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f64; 3] {
        let n = self.size.saturating_sub(1);
        let index = r
            .min(n)
            .saturating_add(g.min(n).saturating_mul(self.size))
            .saturating_add(b.min(n).saturating_mul(self.size).saturating_mul(self.size));
        self.values.get(index).copied().unwrap_or([0.0; 3])
    }
}

impl ColorStage {
    /// Whether this stage needs all three channels at one resolution.
    #[must_use]
    pub const fn needs_common_resolution(&self) -> bool {
        matches!(self, Self::Affine(_) | Self::Float(_))
    }
}

/// How a component set is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    /// R, G, B (or G, B, R stored as such — the channel indices are logical).
    Rgb,
    /// Y', Cb, Cr.
    YCbCr,
    /// Y' only.
    Gray,
}

/// The quantisation endpoints of one side of a conversion.
#[derive(Debug, Clone, Copy)]
struct Levels {
    /// Code value of black / the chroma neutral point.
    y_off: f64,
    c_off: f64,
    /// Code values spanned by one unit of the normalised signal.
    y_span: f64,
    c_span: f64,
}

fn levels(range: ColorRange, depth: u8, space: Space) -> Levels {
    let maxv = f64::from((1u32 << depth) - 1);
    let unit = f64::from(1u32 << (depth - 8));
    // R'G'B' quantises like luma on both axes: there is no chroma offset.
    let full = matches!(range, ColorRange::Full);
    match space {
        Space::Rgb => {
            if full {
                Levels {
                    y_off: 0.0,
                    c_off: 0.0,
                    y_span: maxv,
                    c_span: maxv,
                }
            } else {
                Levels {
                    y_off: 16.0 * unit,
                    c_off: 16.0 * unit,
                    y_span: 219.0 * unit,
                    c_span: 219.0 * unit,
                }
            }
        }
        Space::YCbCr | Space::Gray => {
            if full {
                Levels {
                    y_off: 0.0,
                    c_off: f64::from(1u32 << (depth - 1)),
                    y_span: maxv,
                    c_span: maxv,
                }
            } else {
                Levels {
                    y_off: 16.0 * unit,
                    c_off: 128.0 * unit,
                    y_span: 219.0 * unit,
                    c_span: 224.0 * unit,
                }
            }
        }
    }
}

/// Round a real coefficient into `shift` fractional bits.
fn q(v: f64, shift: u8) -> i32 {
    let scaled = v * f64::from(1i32 << shift);
    if !scaled.is_finite() {
        return 0;
    }
    scaled
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Build the colour stage for one conversion at `depth` bits of working
/// precision.
///
/// `None` is returned as [`ColorStage::None`] whenever both sides agree, which
/// is what makes `rgb24 -> bgr24` and `yuv420p -> nv12` cost nothing: channel
/// *order* is a layout fact, handled by [`crate::geometry`], never by a matrix.
/// Build the colour stage, reserving the bounded LUT storage from `budget`.
///
/// # Errors
///
/// Returns the allocation error when the selected LUT grid cannot fit the
/// caller's budget.
pub fn build(
    budget: &mut Budget,
    src: &ImageSpec,
    dst: &ImageSpec,
    opts: &ScaleOptions,
    depth: u8,
) -> Result<ColorStage> {
    let src_primaries = src.effective_primaries();
    let dst_primaries = dst.effective_primaries();
    let src_transfer = src.effective_transfer();
    let dst_transfer = dst.effective_transfer();
    let source_peak = src.effective_peak_nits();
    let destination_peak = dst.effective_peak_nits();
    let needs_lut = needs_lut(
        opts.intent,
        src_primaries,
        dst_primaries,
        src_transfer,
        dst_transfer,
        source_peak,
        destination_peak,
    );
    if src_primaries != dst_primaries || src_transfer != dst_transfer || needs_lut {
        let primary_matrix = primary_matrix(
            src_primaries,
            dst_primaries,
            !matches!(opts.intent, RenderingIntent::AbsoluteColorimetric),
        );
        let lut = if needs_lut {
            Some(Arc::new(build_lut(
                budget,
                opts.lut3d_size.clamp(9, 65) as usize,
                opts.intent,
                src_transfer,
                dst_transfer,
                source_peak,
                destination_peak,
                primary_matrix,
            )?))
        } else {
            None
        };
        return Ok(ColorStage::Float(FloatTransform {
            src_space: src.space(),
            dst_space: dst.space(),
            src_range: src.effective_range(),
            dst_range: dst.effective_range(),
            src_matrix: src.effective_matrix(),
            dst_matrix: dst.effective_matrix(),
            src_primaries,
            dst_primaries,
            src_transfer,
            dst_transfer,
            primary_matrix: if lut.is_some() {
                IDENTITY3
            } else {
                primary_matrix
            },
            lut,
            depth,
        }));
    }
    let (ss, ds) = (src.space(), dst.space());
    let (sr, dr) = (src.effective_range(), dst.effective_range());
    let (sm, dm) = (src.effective_matrix(), dst.effective_matrix());

    let same_space = matches!(
        (ss, ds),
        (Space::Rgb, Space::Rgb) | (Space::YCbCr | Space::Gray, Space::YCbCr | Space::Gray)
    );
    if same_space && sr == dr && (matches!(ss, Space::Rgb) || sm == dm) {
        return Ok(ColorStage::None);
    }

    let sl = levels(sr, depth, ss);
    let dl = levels(dr, depth, ds);
    let maxv = (1i32 << depth) - 1;

    // The transform in normalised space: input code -> normalised -> matrix ->
    // normalised -> output code. Composed in f64, quantised once.
    let fwd: [[f64; 3]; 3] = match (ss, ds) {
        (Space::Rgb, Space::Rgb) => IDENTITY3,
        (Space::YCbCr | Space::Gray, Space::Rgb) => match ycbcr_to_rgb(sm, src) {
            Some(m) => m,
            None => return Ok(ColorStage::None),
        },
        (Space::Rgb, Space::YCbCr | Space::Gray) => match rgb_to_ycbcr(dm, dst) {
            Some(m) => m,
            None => return Ok(ColorStage::None),
        },
        (Space::YCbCr | Space::Gray, Space::YCbCr | Space::Gray) => {
            if sm == dm {
                IDENTITY3
            } else {
                let (Some(a), Some(b)) = (ycbcr_to_rgb(sm, src), rgb_to_ycbcr(dm, dst)) else {
                    return Ok(ColorStage::None);
                };
                mul3(&b, &a)
            }
        }
    };

    let yuv_out = !matches!(ds, Space::Rgb);
    let shift = if yuv_out {
        RGB_TO_YUV_SHIFT
    } else {
        YUV_TO_RGB_SHIFT
    };

    // Column scale: input code -> normalised. Row scale: normalised -> output.
    let in_span = [sl.y_span, sl.c_span, sl.c_span];
    let in_off = [sl.y_off, sl.c_off, sl.c_off];
    let out_span = [dl.y_span, dl.c_span, dl.c_span];
    let out_off = [dl.y_off, dl.c_off, dl.c_off];
    // For R'G'B' every channel is a luma-like axis.
    let (in_span, in_off) = if matches!(ss, Space::Rgb) {
        ([sl.y_span; 3], [sl.y_off; 3])
    } else {
        (in_span, in_off)
    };
    let (out_span, out_off) = if matches!(ds, Space::Rgb) {
        ([dl.y_span; 3], [dl.y_off; 3])
    } else {
        (out_span, out_off)
    };

    let mut m = [[0i32; 3]; 3];
    let mut bias = [0i64; 3];
    for i in 0..3 {
        let os = out_span.get(i).copied().unwrap_or(1.0);
        let mut b = out_off.get(i).copied().unwrap_or(0.0) * f64::from(1i32 << shift);
        for j in 0..3 {
            let coef = fwd.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0.0) * os
                / in_span.get(j).copied().unwrap_or(1.0);
            let qc = q(coef, shift);
            if let Some(slot) = m.get_mut(i).and_then(|r| r.get_mut(j)) {
                *slot = qc;
            }
            b -= f64::from(qc) * in_off.get(j).copied().unwrap_or(0.0);
        }
        if let Some(slot) = bias.get_mut(i) {
            *slot = (b.round() as i64) + rounding(shift, yuv_out);
        }
    }

    Ok(ColorStage::Affine(Affine {
        m,
        bias,
        shift,
        max: maxv,
    }))
}

const IDENTITY3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const BRADFORD: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];
const INV_BRADFORD: [[f64; 3]; 3] = [
    [0.986_992_9, -0.147_054_3, 0.159_962_7],
    [0.432_305_3, 0.518_360_3, 0.049_291_2],
    [-0.008_528_7, 0.040_042_8, 0.968_486_7],
];

fn normalise(px: [i32; 3], space: Space, range: ColorRange, depth: u8) -> [f64; 3] {
    let bit_depth = u32::from(depth);
    let Some(luma) = range.luma_levels(bit_depth) else {
        return [0.0; 3];
    };
    let Some(chroma) = range.chroma_levels(bit_depth) else {
        return [0.0; 3];
    };
    let y = (f64::from(px[0]) - f64::from(luma.offset)) / f64::from(luma.scale);
    match space {
        Space::Rgb => [y, y_from(px[1], luma), y_from(px[2], luma)],
        Space::Gray => [y, 0.0, 0.0],
        Space::YCbCr => [
            y,
            (f64::from(px[1]) - f64::from(chroma.offset)) / f64::from(chroma.scale),
            (f64::from(px[2]) - f64::from(chroma.offset)) / f64::from(chroma.scale),
        ],
    }
}

fn y_from(value: i32, levels: vaco_color::Levels) -> f64 {
    (f64::from(value) - f64::from(levels.offset)) / f64::from(levels.scale)
}

fn quantise(value: [f64; 3], space: Space, range: ColorRange, depth: u8) -> [i32; 3] {
    let bit_depth = u32::from(depth);
    let Some(luma) = range.luma_levels(bit_depth) else {
        return [0; 3];
    };
    let Some(chroma) = range.chroma_levels(bit_depth) else {
        return [0; 3];
    };
    let y = quantise_component(value[0], luma);
    match space {
        Space::Rgb => [
            y,
            quantise_component(value[1], luma),
            quantise_component(value[2], luma),
        ],
        Space::Gray => [y, 0, 0],
        Space::YCbCr => [
            y,
            quantise_component(value[1], chroma),
            quantise_component(value[2], chroma),
        ],
    }
}

fn quantise_component(value: f64, levels: vaco_color::Levels) -> i32 {
    (f64::from(levels.offset) + f64::from(levels.scale) * value)
        .round()
        .clamp(f64::from(levels.min), f64::from(levels.max)) as i32
}

fn needs_lut(
    intent: RenderingIntent,
    src_primaries: ColorPrimaries,
    dst_primaries: ColorPrimaries,
    src_transfer: TransferCharacteristic,
    dst_transfer: TransferCharacteristic,
    source_peak: u32,
    destination_peak: u32,
) -> bool {
    let gamut_policy =
        !matches!(intent, RenderingIntent::RelativeColorimetric) && src_primaries != dst_primaries;
    let dynamic_range =
        (src_transfer.is_hdr() || dst_transfer.is_hdr()) && source_peak != destination_peak;
    gamut_policy || dynamic_range
}

fn build_lut(
    budget: &mut Budget,
    size: usize,
    intent: RenderingIntent,
    src_transfer: TransferCharacteristic,
    dst_transfer: TransferCharacteristic,
    source_peak: u32,
    destination_peak: u32,
    primary: [[f64; 3]; 3],
) -> Result<Lut3D> {
    let n = size.clamp(9, 65);
    let entries = n.saturating_mul(n).saturating_mul(n);
    let mut values = budget.alloc::<[f64; 3]>(entries)?;
    let denominator = n.saturating_sub(1).max(1) as f64;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let source = [
                    r as f64 / denominator,
                    g as f64 / denominator,
                    b as f64 / denominator,
                ];
                let index = r
                    .saturating_add(g.saturating_mul(n))
                    .saturating_add(b.saturating_mul(n).saturating_mul(n));
                if let Some(slot) = values.get_mut(index) {
                    *slot = map_colour(
                        mul3v(
                            primary,
                            source.map(|value| src_transfer.decode(value).unwrap_or(value)),
                        ),
                        intent,
                        src_transfer,
                        dst_transfer,
                        source_peak,
                        destination_peak,
                    );
                }
            }
        }
    }
    Ok(Lut3D { size: n, values })
}

fn map_colour(
    rgb: [f64; 3],
    intent: RenderingIntent,
    src_transfer: TransferCharacteristic,
    dst_transfer: TransferCharacteristic,
    source_peak: u32,
    destination_peak: u32,
) -> [f64; 3] {
    let src_reference = transfer_reference_peak(src_transfer, source_peak);
    let dst_reference = transfer_reference_peak(dst_transfer, destination_peak);
    let nits = rgb.map(|v| v * src_reference);
    let luminance = luma(nits);
    let mapped_luminance = if source_peak > destination_peak {
        bt2390_eetf(luminance, source_peak, destination_peak)
    } else {
        luminance
    };
    let mapped_nits = if luminance > 0.0 {
        nits.map(|v| v * mapped_luminance / luminance)
    } else {
        [0.0; 3]
    };
    gamut_map(mapped_nits.map(|v| v / dst_reference.max(1.0)), intent)
}

fn transfer_reference_peak(transfer: TransferCharacteristic, peak: u32) -> f64 {
    match transfer {
        TransferCharacteristic::Smpte2084 => 10_000.0,
        TransferCharacteristic::AribStdB67 => f64::from(peak),
        _ => 100.0,
    }
}

fn luma(rgb: [f64; 3]) -> f64 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}

/// BT.2390's PQ-domain Hermite EETF, evaluated on luminance in cd/m².
fn bt2390_eetf(nits: f64, source_peak: u32, destination_peak: u32) -> f64 {
    if source_peak <= destination_peak || nits <= 0.0 {
        return nits.max(0.0);
    }
    let source_white = pq_code(f64::from(source_peak));
    let target_white = pq_code(f64::from(destination_peak));
    if source_white <= 0.0 || target_white >= source_white {
        return nits.max(0.0);
    }
    let max_lum = target_white / source_white;
    let knee = (1.5 * max_lum - 0.5).clamp(0.0, 1.0);
    let input = (pq_code(nits.min(f64::from(source_peak))) / source_white).clamp(0.0, 1.0);
    let shaped = if input < knee || knee >= 1.0 {
        input
    } else {
        let t = ((input - knee) / (1.0 - knee)).clamp(0.0, 1.0);
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * knee
            + (t3 - 2.0 * t2 + t) * (1.0 - knee)
            + (-2.0 * t3 + 3.0 * t2) * max_lum
    };
    pq_nits((shaped * source_white).clamp(0.0, target_white))
}

fn pq_code(nits: f64) -> f64 {
    TransferCharacteristic::Smpte2084
        .encode((nits.max(0.0) / 10_000.0).min(1.0))
        .unwrap_or(0.0)
}

fn pq_nits(code: f64) -> f64 {
    TransferCharacteristic::Smpte2084
        .decode(code.clamp(0.0, 1.0))
        .unwrap_or(0.0)
        * 10_000.0
}

fn gamut_map(rgb: [f64; 3], intent: RenderingIntent) -> [f64; 3] {
    match intent {
        RenderingIntent::AbsoluteColorimetric | RenderingIntent::RelativeColorimetric => {
            rgb.map(|v| v.clamp(0.0, 1.0))
        }
        RenderingIntent::Saturation | RenderingIntent::Perceptual => {
            let neutral = luma(rgb).clamp(0.0, 1.0);
            let chroma = rgb.map(|v| v - neutral);
            let mut limit: f64 = 1.0;
            for component in chroma {
                let candidate = if component > 0.0 {
                    (1.0 - neutral) / component
                } else if component < 0.0 {
                    -neutral / component
                } else {
                    1.0
                };
                limit = limit.min(candidate);
            }
            let scale = match intent {
                RenderingIntent::Saturation => limit.min(1.0),
                RenderingIntent::Perceptual if limit < 1.0 => {
                    limit * (1.0 - (-1.0 / limit.max(f64::MIN_POSITIVE)).exp())
                }
                _ => 1.0,
            };
            chroma.map(|v| (neutral + v * scale).clamp(0.0, 1.0))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the eight cube corners and three fractional coordinates are the tetrahedral definition"
)]
fn tetrahedral(
    c000: [f64; 3],
    c100: [f64; 3],
    c010: [f64; 3],
    c001: [f64; 3],
    c110: [f64; 3],
    c101: [f64; 3],
    c011: [f64; 3],
    c111: [f64; 3],
    f: [f64; 3],
) -> [f64; 3] {
    let (a, b, c) = (f[0], f[1], f[2]);
    if a >= b {
        if b >= c {
            blend_tetra(c000, c100, c110, c111, a, b, c)
        } else if a >= c {
            blend_tetra(c000, c100, c101, c111, a, c, b)
        } else {
            blend_tetra(c000, c001, c101, c111, c, a, b)
        }
    } else if a >= c {
        blend_tetra(c000, c010, c110, c111, b, a, c)
    } else if b >= c {
        blend_tetra(c000, c010, c011, c111, b, c, a)
    } else {
        blend_tetra(c000, c001, c011, c111, c, b, a)
    }
}

fn blend_tetra(
    c0: [f64; 3],
    c1: [f64; 3],
    c2: [f64; 3],
    c3: [f64; 3],
    first: f64,
    second: f64,
    third: f64,
) -> [f64; 3] {
    std::array::from_fn(|i| {
        let v0 = c0.get(i).copied().unwrap_or(0.0);
        let v1 = c1.get(i).copied().unwrap_or(0.0);
        let v2 = c2.get(i).copied().unwrap_or(0.0);
        let v3 = c3.get(i).copied().unwrap_or(0.0);
        v0 + first * (v1 - v0) + second * (v2 - v1) + third * (v3 - v2)
    })
}

fn primary_matrix(src: ColorPrimaries, dst: ColorPrimaries, adapt_white: bool) -> [[f64; 3]; 3] {
    if src == dst {
        return IDENTITY3;
    }
    let Some(src_to_xyz) = src.rgb_to_xyz() else {
        return IDENTITY3;
    };
    let Some(xyz_to_dst) = dst.xyz_to_rgb() else {
        return IDENTITY3;
    };
    let adaptation = if adapt_white {
        bradford_adaptation(src, dst)
    } else {
        IDENTITY3
    };
    mul3(&xyz_to_dst, &mul3(&adaptation, &src_to_xyz))
}

fn bradford_adaptation(src: ColorPrimaries, dst: ColorPrimaries) -> [[f64; 3]; 3] {
    let (Some(source), Some(destination)) = (src.chromaticity(), dst.chromaticity()) else {
        return IDENTITY3;
    };
    let (Some(source_white), Some(destination_white)) =
        (white_xyz(source.white), white_xyz(destination.white))
    else {
        return IDENTITY3;
    };
    let source_cones = mul3v(BRADFORD, source_white);
    let destination_cones = mul3v(BRADFORD, destination_white);
    let ratio: [f64; 3] = std::array::from_fn(|i| {
        let den = source_cones.get(i).copied().unwrap_or(1.0);
        let num = destination_cones.get(i).copied().unwrap_or(den);
        if den == 0.0 { 1.0 } else { num / den }
    });
    let scale = [
        [ratio[0], 0.0, 0.0],
        [0.0, ratio[1], 0.0],
        [0.0, 0.0, ratio[2]],
    ];
    mul3(&INV_BRADFORD, &mul3(&scale, &BRADFORD))
}

fn white_xyz((x, y): (f64, f64)) -> Option<[f64; 3]> {
    (y != 0.0).then_some([x / y, 1.0, (1.0 - x - y) / y])
}

fn mul3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut s = 0.0;
            for k in 0..3 {
                s += a.get(i).and_then(|r| r.get(k)).copied().unwrap_or(0.0)
                    * b.get(k).and_then(|r| r.get(j)).copied().unwrap_or(0.0);
            }
            if let Some(slot) = out.get_mut(i).and_then(|r| r.get_mut(j)) {
                *slot = s;
            }
        }
    }
    out
}

fn mul3v(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| {
        m.get(i).map_or(0.0, |row| {
            row.iter()
                .zip(v)
                .map(|(coefficient, value)| coefficient * value)
                .sum()
        })
    })
}

fn ycbcr_to_rgb(mc: MatrixCoefficients, spec: &ImageSpec) -> Option<[[f64; 3]; 3]> {
    mc.ycbcr_to_rgb_with(spec.color.primaries)
}

fn rgb_to_ycbcr(mc: MatrixCoefficients, spec: &ImageSpec) -> Option<[[f64; 3]; 3]> {
    mc.rgb_to_ycbcr_with(spec.color.primaries)
}

/// Whether a matrix code point has a linear R'G'B' form this crate implements.
#[must_use]
pub fn matrix_is_supported(mc: MatrixCoefficients, primaries: vaco_color::ColorPrimaries) -> bool {
    mc == MatrixCoefficients::Unspecified || mc.ycbcr_to_rgb_with(primaries).is_some()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use crate::options::ScaleOptions;
    use crate::spec::ImageSpec;
    use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn spec(fmt: PixFmt, range: ColorRange, matrix: MatrixCoefficients) -> ImageSpec {
        ImageSpec {
            format: fmt,
            width: 16,
            height: 16,
            color: ColorInfo {
                range,
                matrix,
                ..ColorInfo::default()
            },
            ..ImageSpec::new(fmt, 16, 16)
        }
    }

    fn stage(src: &ImageSpec, dst: &ImageSpec) -> ColorStage {
        let mut budget = Budget::new(Limits::permissive());
        build(&mut budget, src, dst, &ScaleOptions::default(), 8).unwrap()
    }

    /// The exact integers recovered from the reference. If any of these move,
    /// every "Exact" grade in the fidelity table moves with them.
    #[test]
    fn yuv_to_rgb_coefficients_are_the_measured_ones() {
        let s = spec(
            PixFmt::Yuv444p,
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
        );
        let d = spec(PixFmt::Rgb24, ColorRange::Full, MatrixCoefficients::Bt709);
        let ColorStage::Affine(a) = stage(&s, &d) else {
            panic!("expected an affine stage");
        };
        assert_eq!(a.shift, 13);
        assert_eq!(a.m[0][0], 9539, "Cy");
        assert_eq!(a.m[0][2], 14686, "Crv");
        assert_eq!(a.m[1][1], -1747, "Cgu");
        assert_eq!(a.m[2][1], 17305, "Cbu");
        // bias folds the input offsets and the rounding term.
        assert_eq!(a.bias[0], -(9539 * 16 + 14686 * 128) + 4096);
    }

    #[test]
    fn rgb_to_yuv_coefficients_are_the_measured_ones() {
        let s = spec(PixFmt::Rgb24, ColorRange::Full, MatrixCoefficients::Bt709);
        let d = spec(
            PixFmt::Yuv444p,
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
        );
        let ColorStage::Affine(a) = stage(&s, &d) else {
            panic!("expected an affine stage");
        };
        assert_eq!(a.shift, 15);
        assert_eq!(a.m[0], [5983, 20127, 2032], "Y row");
        assert_eq!(a.m[1], [-3298, -11094, 14392], "Cb row");
        assert_eq!(a.m[2], [14392, -13073, -1320], "Cr row");
        assert_eq!(a.bias[0], (16 << 15) + (1 << 14) + (1 << 8));
        assert_eq!(a.bias[1], (128 << 15) + (1 << 14) + (1 << 8));
    }

    #[test]
    fn matching_endpoints_need_no_stage() {
        let s = spec(
            PixFmt::Yuv420p,
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
        );
        let d = spec(PixFmt::Nv12, ColorRange::Limited, MatrixCoefficients::Bt709);
        assert_eq!(stage(&s, &d), ColorStage::None);
        let s = spec(
            PixFmt::Rgb24,
            ColorRange::Full,
            MatrixCoefficients::Unspecified,
        );
        let d = spec(
            PixFmt::Bgr24,
            ColorRange::Full,
            MatrixCoefficients::Unspecified,
        );
        assert_eq!(stage(&s, &d), ColorStage::None);
    }

    #[test]
    fn limited_to_full_luma_is_the_reference_curve() {
        let s = spec(
            PixFmt::Yuv444p,
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
        );
        let d = spec(PixFmt::Rgb24, ColorRange::Full, MatrixCoefficients::Bt709);
        let ColorStage::Affine(a) = stage(&s, &d) else {
            panic!("expected an affine stage");
        };
        for y in 0..=255i32 {
            let got = a.apply([y, 128, 128]);
            let want = (f64::from(y - 16) * 255.0 / 219.0)
                .round()
                .clamp(0.0, 255.0) as i32;
            assert_eq!(got[0], want, "Y = {y}");
        }
    }

    #[test]
    fn tetrahedral_uses_the_enclosing_ordered_simplex() {
        // For r >= g >= b, the barycentric weights of the enclosing
        // tetrahedron are 1-r, r-g, g-b, b. These independent hand-worked
        // values distinguish this interpolation from trilinear blending.
        let lut = Lut3D {
            size: 2,
            values: vec![
                [0.0, 0.0, 0.0],
                [1.0, 2.0, 3.0],
                [2.0, 3.0, 5.0],
                [3.0, 5.0, 8.0],
                [4.0, 7.0, 11.0],
                [5.0, 11.0, 13.0],
                [6.0, 13.0, 17.0],
                [7.0, 17.0, 19.0],
            ],
        };
        let output = lut.sample([0.75, 0.5, 0.25]);
        assert_eq!(output, [2.75, 6.0, 7.5]);
    }

    #[test]
    fn bt2390_rolloff_is_monotonic_and_reaches_the_target_peak() {
        let source_peak = 1_000;
        let target_peak = 100;
        let dark = bt2390_eetf(10.0, source_peak, target_peak);
        let middle = bt2390_eetf(100.0, source_peak, target_peak);
        let bright = bt2390_eetf(f64::from(source_peak), source_peak, target_peak);
        assert!(0.0 <= dark && dark <= middle && middle <= bright);
        assert!((bright - f64::from(target_peak)).abs() < 0.001);
    }

    #[test]
    fn hdr_peak_change_builds_a_lut_while_sdr_metadata_does_not() {
        let pq = ImageSpec::new(PixFmt::Rgb24, 8, 8)
            .with_color(ColorInfo {
                transfer: TransferCharacteristic::Smpte2084,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
                ..ColorInfo::default()
            })
            .with_hdr_peaks(Some(1_000), None);
        let sdr = ImageSpec::new(PixFmt::Rgb24, 8, 8)
            .with_color(ColorInfo {
                transfer: TransferCharacteristic::Bt709,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
                ..ColorInfo::default()
            })
            .with_hdr_peaks(Some(100), None);
        let mut budget = Budget::new(Limits::permissive());
        let ColorStage::Float(stage) =
            build(&mut budget, &pq, &sdr, &ScaleOptions::default(), 8).unwrap()
        else {
            panic!("HDR-to-SDR needs a float LUT stage");
        };
        assert!(stage.lut.is_some());

        let left = ImageSpec::new(PixFmt::Rgb24, 8, 8).with_hdr_peaks(Some(80), None);
        let right = ImageSpec::new(PixFmt::Rgb24, 8, 8).with_hdr_peaks(Some(200), None);
        assert!(left.is_same_picture(&right));
    }
}
