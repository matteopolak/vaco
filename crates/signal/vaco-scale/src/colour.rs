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

use vaco_color::{ColorRange, MatrixCoefficients};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorStage {
    /// Nothing: the code values mean the same thing on both sides.
    None,
    /// A 3×3 affine on channels 0..2; channel 3 passes through.
    Affine(Affine),
}

impl ColorStage {
    /// Whether this stage needs all three channels at one resolution.
    #[must_use]
    pub const fn needs_common_resolution(&self) -> bool {
        matches!(self, Self::Affine(_))
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
#[must_use]
pub fn build(src: &ImageSpec, dst: &ImageSpec, depth: u8) -> ColorStage {
    let (ss, ds) = (src.space(), dst.space());
    let (sr, dr) = (src.effective_range(), dst.effective_range());
    let (sm, dm) = (src.effective_matrix(), dst.effective_matrix());

    let same_space = matches!(
        (ss, ds),
        (Space::Rgb, Space::Rgb) | (Space::YCbCr | Space::Gray, Space::YCbCr | Space::Gray)
    );
    if same_space && sr == dr && (matches!(ss, Space::Rgb) || sm == dm) {
        return ColorStage::None;
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
            None => return ColorStage::None,
        },
        (Space::Rgb, Space::YCbCr | Space::Gray) => match rgb_to_ycbcr(dm, dst) {
            Some(m) => m,
            None => return ColorStage::None,
        },
        (Space::YCbCr | Space::Gray, Space::YCbCr | Space::Gray) => {
            if sm == dm {
                IDENTITY3
            } else {
                let (Some(a), Some(b)) = (ycbcr_to_rgb(sm, src), rgb_to_ycbcr(dm, dst)) else {
                    return ColorStage::None;
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

    ColorStage::Affine(Affine {
        m,
        bias,
        shift,
        max: maxv,
    })
}

const IDENTITY3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

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
    use crate::spec::ImageSpec;
    use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
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
        }
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
        let ColorStage::Affine(a) = build(&s, &d, 8) else {
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
        let ColorStage::Affine(a) = build(&s, &d, 8) else {
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
        assert_eq!(build(&s, &d, 8), ColorStage::None);
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
        assert_eq!(build(&s, &d, 8), ColorStage::None);
    }

    #[test]
    fn limited_to_full_luma_is_the_reference_curve() {
        let s = spec(
            PixFmt::Yuv444p,
            ColorRange::Limited,
            MatrixCoefficients::Bt709,
        );
        let d = spec(PixFmt::Rgb24, ColorRange::Full, MatrixCoefficients::Bt709);
        let ColorStage::Affine(a) = build(&s, &d, 8) else {
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
}
