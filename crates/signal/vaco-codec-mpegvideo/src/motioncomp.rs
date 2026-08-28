//! Half-pel motion compensation (D-22c): forming one prediction block from a
//! reference picture at a half-pel motion vector, and combining forward and
//! backward predictions for a B-picture.
//!
//! Every member of this family that predicts at half-pel precision (H.261,
//! H.263 baseline, MPEG-1/2, and MPEG-4 Part 2's non-quarter-pel modes) forms
//! a prediction the same way: bilinear averaging of the one, two or four
//! nearest integer-sample neighbours, rounded to the nearest integer with
//! ties broken away from zero. Quarter-pel (MPEG-4 Part 2's own extension)
//! and 6-tap luma interpolation (the unrelated H.264/HEVC family) are out of
//! scope for this module — see [`crate`]'s module docs.
//!
//! Extracted and generalised from `vaco-codec-mpeg12`'s `motion.rs`
//! (`form_prediction`/`average_predictions`), which is why this module's own
//! rounding is checked against `vaco-codec-dsp-mc`'s independently-authored
//! `taps::BILINEAR` — a second implementation of the same rounding rule that
//! can be wrong differently, per this project's own "an oracle you wrote
//! shares your misreading" lesson.

/// A source of reference samples, decoupling this module's addressing maths
/// from any one family's picture representation. `sample` must never panic:
/// an out-of-range `(x, y)` is expected (an unrestricted motion vector
/// routinely points outside the picture) and should be answered by clamping
/// to the plane's own border, exactly as every member of this family's own
/// "unavailable sample" rule requires.
pub trait Sampler {
    /// One sample of plane `plane` at `(x, y)`, clamped to that plane's own
    /// border for any coordinate outside it.
    fn sample(&self, plane: usize, x: i32, y: i32) -> u8;
}

/// §4.1's `//` operator applied to a two-value average: "integer division
/// with rounding to the nearest integer, half-integer values rounded away
/// from zero." For the non-negative sums a pixel average always is, "away
/// from zero" means "round half up": `avg2(0, 1) == 1`, not `0`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "div_ceil of two u8 widened to u16 cannot exceed u8::MAX"
)]
pub fn avg2(a: u8, b: u8) -> u8 {
    // Ceiling division is exactly "round half up" for a divisor of 2 — the
    // only kind of tie a two-value average can produce.
    (u16::from(a) + u16::from(b)).div_ceil(2) as u8
}

/// The same rounding rule applied to a four-value (diagonal half-pel)
/// average.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the +2-then-truncate form is round-to-nearest-ties-away-from-zero for a divisor of 4 (unlike div_ceil, which rounds every non-exact remainder up, not just the half-way one) — this is the literal `//` operator applied to a sum of four non-negative values, not an approximation"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "the four-value rounded average of u8 samples cannot exceed u8::MAX"
)]
pub fn avg4(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((u16::from(a) + u16::from(b) + u16::from(c) + u16::from(d) + 2) / 4) as u8
}

/// Form one `size_w x size_h` prediction block at `(dst_x, dst_y)` — actually
/// written into `out` (row-major, `out_stride` wide) — reading from `src`'s
/// plane `plane_idx` offset by the half-pel motion vector `(mv_x, mv_y)`
/// (already scaled for chroma, if applicable — that scaling is a family's
/// own concern, not this function's).
///
/// `row_scale`/`row_parity` generalise frame vs. field addressing on the
/// **reference**: a frame-based prediction reads reference row `y` directly
/// (`row_scale = 1, row_parity = 0`); a field-based prediction addresses one
/// field's rows only, so reference row `y` really means frame row `y * 2 +
/// row_parity` (`row_scale = 2`) — including for the vertical half-pel
/// neighbour, which is the *next row of the same field*, two frame rows
/// away, not one.
#[allow(
    clippy::too_many_arguments,
    reason = "the forming-predictions equation genuinely has this many independent inputs (source position, motion vector, field addressing, block geometry, destination); a struct would not make any call site clearer, only add one more type to look up"
)]
pub fn form_prediction<S: Sampler>(
    src: &S,
    plane_idx: usize,
    src_x: i32,
    src_y: i32,
    mv_x: i32,
    mv_y: i32,
    row_scale: i32,
    row_parity: i32,
    size_w: usize,
    size_h: usize,
    out: &mut [u8],
    out_stride: usize,
) {
    let int_x = mv_x.div_euclid(2);
    let int_y = mv_y.div_euclid(2);
    let half_x = mv_x.rem_euclid(2) != 0;
    let half_y = mv_y.rem_euclid(2) != 0;

    for y in 0..size_h {
        for x in 0..size_w {
            let sx = src_x + i32::try_from(x).unwrap_or(0) + int_x;
            let field_y = src_y + i32::try_from(y).unwrap_or(0) + int_y;
            let sy = field_y * row_scale + row_parity;
            let sy_next = (field_y + 1) * row_scale + row_parity;
            let value = match (half_x, half_y) {
                (false, false) => src.sample(plane_idx, sx, sy),
                (false, true) => avg2(src.sample(plane_idx, sx, sy), src.sample(plane_idx, sx, sy_next)),
                (true, false) => avg2(src.sample(plane_idx, sx, sy), src.sample(plane_idx, sx + 1, sy)),
                (true, true) => avg4(
                    src.sample(plane_idx, sx, sy),
                    src.sample(plane_idx, sx + 1, sy),
                    src.sample(plane_idx, sx, sy_next),
                    src.sample(plane_idx, sx + 1, sy_next),
                ),
            };
            if let Some(row) = out.get_mut(y * out_stride..y * out_stride + out_stride)
                && let Some(slot) = row.get_mut(x)
            {
                *slot = value;
            }
        }
    }
}

/// Combine two already-formed predictions (forward/backward) for B-picture
/// bidirectional prediction: a plain rounded average, same rule as
/// [`avg2`]. `a`/`b`/`out` are zipped to the shortest of the three.
pub fn average_predictions(a: &[u8], b: &[u8], out: &mut [u8]) {
    for ((&av, &bv), o) in a.iter().zip(b.iter()).zip(out.iter_mut()) {
        *o = avg2(av, bv);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code")]
    use super::{Sampler, avg2, avg4, average_predictions, form_prediction};

    #[test]
    fn avg2_rounds_half_away_from_zero() {
        assert_eq!(avg2(0, 1), 1);
        assert_eq!(avg2(2, 3), 3);
        assert_eq!(avg2(255, 255), 255);
        assert_eq!(avg2(0, 0), 0);
    }

    #[test]
    fn avg4_rounds_half_up_too() {
        assert_eq!(avg4(0, 0, 0, 1), 0);
        assert_eq!(avg4(0, 0, 1, 1), 1);
    }

    /// Cross-check against `vaco-codec-dsp-mc`'s independently-authored
    /// bilinear tap set — a second implementation of the identical rounding
    /// rule (round-half-up over a sum, `(sum + round_bias) >> shift`), not a
    /// second transcription of this module's own arithmetic. Per this
    /// project's "an oracle you wrote shares your misreading" lesson, this
    /// is only worth something because the two crates were written for
    /// different purposes (a SIMD-dispatched two-pass FIR engine here vs.
    /// this module's plain scalar averaging) and never shared a line.
    #[test]
    fn avg2_agrees_with_dsp_mc_bilinear_tap_set() {
        use vaco_codec_dsp_mc::fir::taps::BILINEAR;
        for a in [0u8, 1, 2, 127, 128, 254, 255] {
            for b in [0u8, 1, 2, 127, 128, 254, 255] {
                let sum = i32::from(BILINEAR.coeffs[0]) * i32::from(a)
                    + i32::from(BILINEAR.coeffs[1]) * i32::from(b);
                let bias = i32::from(BILINEAR.round_bias());
                let expect = ((sum + bias) >> BILINEAR.shift).clamp(0, 255) as u8;
                assert_eq!(avg2(a, b), expect, "a={a} b={b}");
            }
        }
    }

    /// A trivial in-memory picture: one plane, flat stride, clamped border —
    /// the smallest thing that satisfies [`Sampler`].
    struct FlatPlane {
        w: i32,
        h: i32,
        data: Vec<u8>,
    }

    impl Sampler for FlatPlane {
        fn sample(&self, _plane: usize, x: i32, y: i32) -> u8 {
            let xx = x.clamp(0, self.w - 1);
            let yy = y.clamp(0, self.h - 1);
            self.data
                .get((yy * self.w + xx) as usize)
                .copied()
                .unwrap_or(0)
        }
    }

    #[test]
    fn integer_motion_vector_copies_samples_directly() {
        let src = FlatPlane {
            w: 4,
            h: 4,
            data: (0..16).collect(),
        };
        let mut out = [0u8; 4];
        // mv = (2, 2) half-pel units == (1, 1) integer samples.
        form_prediction(&src, 0, 0, 0, 2, 2, 1, 0, 2, 2, &mut out, 2);
        // Reading a 2x2 block starting at (1,1): 5,6,9,10.
        assert_eq!(out, [5, 6, 9, 10]);
    }

    #[test]
    fn half_pel_horizontal_averages_two_neighbours() {
        let src = FlatPlane {
            w: 4,
            h: 1,
            data: vec![0, 10, 20, 30],
        };
        let mut out = [0u8; 1];
        // mv_x = 1 half-pel unit (0.5 sample) at src_x = 0: avg(0, 10) = 5.
        form_prediction(&src, 0, 0, 0, 1, 0, 1, 0, 1, 1, &mut out, 1);
        assert_eq!(out, [5]);
    }

    #[test]
    fn half_pel_diagonal_averages_four_neighbours() {
        let src = FlatPlane {
            w: 2,
            h: 2,
            data: vec![0, 10, 20, 30],
        };
        let mut out = [0u8; 1];
        // mv = (1, 1) half-pel: diagonal half-pel at (0,0) averages all four
        // corners: (0+10+20+30+2)/4 = 15.
        form_prediction(&src, 0, 0, 0, 1, 1, 1, 0, 1, 1, &mut out, 1);
        assert_eq!(out, [15]);
    }

    #[test]
    fn field_addressing_reads_every_other_reference_row() {
        // 4 frame rows: row_scale=2 addresses only the odd (parity=1) rows.
        let src = FlatPlane {
            w: 1,
            h: 4,
            data: vec![0, 100, 0, 200],
        };
        let mut out = [0u8; 2];
        // Two field rows (0, 1) at parity 1: frame rows 1 and 3.
        form_prediction(&src, 0, 0, 0, 0, 0, 2, 1, 1, 2, &mut out, 1);
        assert_eq!(out, [100, 200]);
    }

    #[test]
    fn average_predictions_matches_avg2_elementwise() {
        let a = [0u8, 10, 255];
        let b = [1u8, 11, 255];
        let mut out = [0u8; 3];
        average_predictions(&a, &b, &mut out);
        assert_eq!(out, [avg2(0, 1), avg2(10, 11), avg2(255, 255)]);
    }
}
