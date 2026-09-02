//! Motion vector decoding (§7.6.3) and prediction forming (§7.6.4), for
//! frame pictures only. Field pictures, dual-prime and 16x8 MC are out of
//! scope.

use vaco_bitstream::BitReader;

use crate::picture::RefPicture;
use crate::tables;
use crate::vlc;

/// The longest `motion_code` is 11 bits (Table B.10).
const MAX_MOTION_CODE_LEN: u8 = 11;

/// `PMV[r][s][t]` for one direction `s`: two motion vectors (`r = 0, 1`,
/// only `r = 0` used outside field-based prediction), each with a
/// horizontal and vertical component. A macroblock decode keeps one of
/// these per direction (forward/backward), since forward and backward
/// predictors reset independently.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MotionPredictor {
    pub pmv: [[i32; 2]; 2],
}

impl MotionPredictor {
    pub(crate) const fn reset(&mut self) {
        self.pmv = [[0, 0], [0, 0]];
    }
}

/// Decode one `motion_code[r][s][t]` + optional `motion_residual` pair
/// (§7.6.3.1), given the (possibly already-adjusted) predictor value, and
/// return the reconstructed `vector'[r][s][t]`.
fn decode_component(r: &mut BitReader<'_>, f_code: u8, prediction: i32) -> i32 {
    let Some(&(_, code)) = vlc::decode(
        r,
        tables::MOTION_CODE,
        |row| (row.0, 0),
        MAX_MOTION_CODE_LEN,
    ) else {
        return prediction;
    };
    let r_size = f_code.saturating_sub(1);
    let f = 1i32 << r_size.min(31);
    let high = (16 * f) - 1;
    let low = -16 * f;
    let range = 32 * f;

    let delta = if f == 1 || code == 0 {
        i32::from(code)
    } else {
        let residual = i32::try_from(r.get(u32::from(r_size))).unwrap_or(0);
        let mag = (i32::from(code).abs() - 1) * f + residual + 1;
        if code < 0 { -mag } else { mag }
    };

    let mut vector = prediction + delta;
    if vector < low {
        vector += range;
    }
    if vector > high {
        vector -= range;
    }
    vector
}

/// Decode one `motion_vector(r, s)` (§6.2.5.2.1): both components, updating
/// `pred.pmv[r_idx]` in place and returning `vector'[r][s][0..2]`.
///
/// `field_and_frame_picture` is §7.6.3.1's one special case: `mv_format ==
/// "field"` **and** `picture_structure == "Frame picture"` (i.e. exactly
/// this crate's field-based-prediction-within-a-frame-picture case) halves
/// the vertical predictor before use and doubles it back before storing —
/// everywhere else the predictor is used and stored as-is.
pub(crate) fn decode_vector(
    r: &mut BitReader<'_>,
    pred: &mut MotionPredictor,
    r_idx: usize,
    f_code: [u8; 2],
    field_and_frame_picture: bool,
) -> [i32; 2] {
    let mut out = [0i32; 2];
    for t in 0..2usize {
        let raw_pmv = pred
            .pmv
            .get(r_idx)
            .and_then(|p| p.get(t))
            .copied()
            .unwrap_or(0);
        let special = field_and_frame_picture && t == 1;
        let prediction = if special {
            raw_pmv.div_euclid(2)
        } else {
            raw_pmv
        };
        let fc = f_code.get(t).copied().unwrap_or(1);
        let vector = decode_component(r, fc, prediction);
        if let Some(slot) = out.get_mut(t) {
            *slot = vector;
        }
        let stored = if special { vector * 2 } else { vector };
        if let Some(slot) = pred.pmv.get_mut(r_idx).and_then(|p| p.get_mut(t)) {
            *slot = stored;
        }
    }
    out
}

/// §7.6.4/§7.6.7.1 use the `//` operator, defined by §4.1 as "integer
/// division with rounding to the nearest integer, half-integer values
/// rounded away from zero" — **not** truncating division. For the
/// non-negative sums a pixel average always is, "away from zero" means
/// "round half up", so `(a + b + 1) / 2` (not `(a + b) / 2`) is the
/// literal reading: `avg2(0, 1)` is `1`, not `0`.
fn avg2(a: u8, b: u8) -> u8 {
    // Ceiling division is exactly "round half up" for a divisor of 2 —
    // there is no other kind of tie to break — so this avoids the
    // hand-written `(a + b + 1) / 2` division `clippy::integer_division`
    // would otherwise (rightly, in general) flag.
    (u16::from(a) + u16::from(b)).div_ceil(2) as u8
}

#[allow(
    clippy::integer_division,
    reason = "the +2-then-truncate form is round-to-nearest-ties-away-from-zero for a divisor of 4 (unlike div_ceil, which would round every non-exact remainder up, not just the half-way one) — this is the literal `//` operator from H.262 §4.1/§7.6.4, not an approximation"
)]
fn avg4(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((u16::from(a) + u16::from(b) + u16::from(c) + u16::from(d) + 2) / 4) as u8
}

/// §7.6.4: form one plane's `size_w x size_h` prediction block at
/// `(dst_x, dst_y_unit)` in `out` (row-major, `out_stride` wide), reading
/// from `refp`'s plane `plane_idx` offset by the half-pel motion vector
/// `(mv_x, mv_y)` (already scaled for chroma if applicable, §7.6.3.7).
///
/// `row_scale`/`row_parity` generalise frame vs. field addressing on the
/// **reference** picture: a frame-based prediction reads reference row `y`
/// directly (`row_scale = 1, row_parity = 0`); a field-based prediction
/// addresses one field's rows only, so reference row `y` really means
/// frame row `y * 2 + row_parity` (`row_scale = 2`) — including for the
/// vertical half-pel neighbour, which is the *next row of the same field*,
/// i.e. two frame rows away, not one. This is the one piece of machinery
/// `crate::macroblock` reuses for both luma and (half-resolution) chroma
/// field prediction, since both are "some plane, some field or frame".
#[allow(
    clippy::too_many_arguments,
    reason = "the forming-predictions equation (7.6.4) genuinely has this many independent inputs; a struct would not make any call site clearer, only add one more type to look up"
)]
pub(crate) fn form_prediction(
    refp: &RefPicture,
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
                (false, false) => refp.sample(plane_idx, sx, sy),
                (false, true) => avg2(
                    refp.sample(plane_idx, sx, sy),
                    refp.sample(plane_idx, sx, sy_next),
                ),
                (true, false) => avg2(
                    refp.sample(plane_idx, sx, sy),
                    refp.sample(plane_idx, sx + 1, sy),
                ),
                (true, true) => avg4(
                    refp.sample(plane_idx, sx, sy),
                    refp.sample(plane_idx, sx + 1, sy),
                    refp.sample(plane_idx, sx, sy_next),
                    refp.sample(plane_idx, sx + 1, sy_next),
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

/// §7.6.7.1: average two predictions (used for B-picture bidirectional
/// prediction).
pub(crate) fn average_predictions(a: &[u8], b: &[u8], out: &mut [u8]) {
    for ((&av, &bv), o) in a.iter().zip(b.iter()).zip(out.iter_mut()) {
        *o = avg2(av, bv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_resets_to_zero() {
        let mut p = MotionPredictor {
            pmv: [[5, -3], [1, 1]],
        };
        p.reset();
        assert_eq!(p.pmv, [[0, 0], [0, 0]]);
    }

    #[test]
    fn avg2_rounds_half_away_from_zero_per_the_spec_operator() {
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
}
