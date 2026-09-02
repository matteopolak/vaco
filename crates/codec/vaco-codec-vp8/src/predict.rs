//! Intra prediction, RFC 6386 §12.
//!
//! Every function here is pure: it takes already-gathered neighbour pixels
//! (with RFC 6386's fixed 127/above, 129/left edge-fill values already
//! substituted by the caller — [`crate::decode`] owns the frame buffer and
//! the above-right special-casing §12.3 describes for the rightmost column
//! of 4x4 subblocks) and returns the predicted block. Keeping neighbour
//! gathering out of this module is what makes the ten 4x4 submodes testable
//! against the RFC's own worked formulas in isolation.
//!
//! `OFF_FRAME_ABOVE = 127`, `OFF_FRAME_LEFT = 129` (§12, opening paragraph).
//! **`DC_PRED`/`B_DC_PRED` do not follow that rule identically** — whole-block
//! (16x16/8x8) `DC_PRED` averages only the genuinely available side rather
//! than substituting 129/127, falling back to a flat 128 when neither side
//! exists; `B_DC_PRED` (4x4) has no such special case and always uses the
//! filled-in 127/129 values like every other mode. See [`predict_dc`] vs
//! [`b_dc`].

pub const OFF_FRAME_ABOVE: u8 = 127;
pub const OFF_FRAME_LEFT: u8 = 129;

fn avg2(x: u8, y: u8) -> u8 {
    ((u16::from(x) + u16::from(y) + 1) >> 1) as u8
}

fn avg3(x: u8, y: u8, z: u8) -> u8 {
    ((u16::from(x) + 2 * u16::from(y) + u16::from(z) + 2) >> 2) as u8
}

fn clamp255(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// `V_PRED`: every row copies `above`.
#[must_use]
pub fn predict_v<const N: usize>(above: &[u8; N]) -> [[u8; N]; N] {
    let mut out = [[0u8; N]; N];
    out.fill(*above);
    out
}

/// `H_PRED`: every column copies `left`.
#[must_use]
pub fn predict_h<const N: usize>(left: &[u8; N]) -> [[u8; N]; N] {
    let mut out = [[0u8; N]; N];
    for (r, row) in out.iter_mut().enumerate() {
        let Some(&v) = left.get(r) else { continue };
        row.fill(v);
    }
    out
}

/// `TM_PRED`: `clamp255(left[r] + above[c] - corner)`. Always uses the
/// filled-in 127/129 edge values when off-frame — unlike [`predict_dc`].
#[must_use]
pub fn predict_tm<const N: usize>(above: &[u8; N], left: &[u8; N], corner: u8) -> [[u8; N]; N] {
    let mut out = [[0u8; N]; N];
    for (r, row) in out.iter_mut().enumerate() {
        let l = left.get(r).copied().unwrap_or(0);
        for (c, px) in row.iter_mut().enumerate() {
            let a = above.get(c).copied().unwrap_or(0);
            *px = clamp255(i32::from(l) + i32::from(a) - i32::from(corner));
        }
    }
    out
}

/// `DC_PRED` (16x16 luma / 8x8 chroma only — see [`b_dc`] for the 4x4 rule).
/// `above`/`left` are `None` when that side is genuinely off-frame (not
/// filled with 127/129): only the available side(s) are averaged, and a
/// macroblock with neither available is filled with a flat 128, per RFC
/// 6386 §12.3's explicit exception.
#[must_use]
pub fn predict_dc<const N: usize>(above: Option<&[u8; N]>, left: Option<&[u8; N]>) -> [[u8; N]; N] {
    // Delegates to `vaco_codec_dsp_intrapred::dc_predict` (D-09): both
    // branches here reduce to that function's own average-with-rounding
    // formula for a power-of-two count (`N`, `2*N` are always 4/8/16/32),
    // where `(sum + count/2) / count` and a shift-based `(sum +
    // (1<<(shift-1))) >> shift` are the identical computation -- verified
    // bit-exact against this function's own pre-existing unit tests below
    // before landing, not merely assumed equivalent.
    let mut top_buf = [0u16; N];
    let mut left_buf = [0u16; N];
    let top: &[u16] = match above {
        Some(a) => {
            for (d, &s) in top_buf.iter_mut().zip(a.iter()) {
                *d = u16::from(s);
            }
            &top_buf
        }
        None => &[],
    };
    let left_slice: &[u16] = match left {
        Some(l) => {
            for (d, &s) in left_buf.iter_mut().zip(l.iter()) {
                *d = u16::from(s);
            }
            &left_buf
        }
        None => &[],
    };
    let dc = vaco_codec_dsp_intrapred::dc_predict(top, left_slice, N, 8);
    let dc_u8 = u8::try_from(dc).unwrap_or(u8::MAX);
    [[dc_u8; N]; N]
}

/// `B_DC_PRED`: the 4x4 luma subblock DC mode. Unlike [`predict_dc`], both
/// sides are always summed (using the caller's already-filled 127/129 edge
/// values), fixed shift of 3.
#[must_use]
pub fn b_dc(above: &[u8; 4], left: &[u8; 4]) -> [[u8; 4]; 4] {
    // Both sides always present here (the caller has already substituted
    // 127/129 for any off-frame edge), so this is exactly
    // `dc_predict`'s both-available branch at size 4 -- `(sum + 4) >> 3`
    // is that branch's own formula for count = 2*4 = 8.
    let top: [u16; 4] = core::array::from_fn(|i| u16::from(above.get(i).copied().unwrap_or(0)));
    let left_arr: [u16; 4] = core::array::from_fn(|i| u16::from(left.get(i).copied().unwrap_or(0)));
    let dc = vaco_codec_dsp_intrapred::dc_predict(&top, &left_arr, 4, 8);
    let dc_u8 = u8::try_from(dc).unwrap_or(u8::MAX);
    [[dc_u8; 4]; 4]
}

/// `above` here is the 8-pixel row (4 direct + 4 above-right); `corner` is
/// the pixel diagonally above-left. `E[0..9] = [left[3],left[2],left[1],
/// left[0], corner, above[0],above[1],above[2],above[3]]` (RFC 6386 §12.3).
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "keeps the same by-reference calling convention as every other prediction function in this module"
)]
fn edge_array(above: &[u8; 8], left: &[u8; 4], corner: u8) -> [u8; 9] {
    [
        left[3], left[2], left[1], left[0], corner, above[0], above[1], above[2], above[3],
    ]
}

#[allow(
    clippy::many_single_char_names,
    reason = "x/y/z mirror avg3's own parameter names directly"
)]
fn avg3p(a: &[u8], i: usize) -> u8 {
    let x = a.get(i.wrapping_sub(1)).copied().unwrap_or(0);
    let y = a.get(i).copied().unwrap_or(0);
    let z = a.get(i + 1).copied().unwrap_or(0);
    avg3(x, y, z)
}

fn avg2p(a: &[u8], i: usize) -> u8 {
    let x = a.get(i).copied().unwrap_or(0);
    let y = a.get(i + 1).copied().unwrap_or(0);
    avg2(x, y)
}

/// `B_VE_PRED`: every row is the 3-tap-smoothed `above` row.
#[must_use]
pub fn b_ve(above: &[u8; 8], corner: u8) -> [[u8; 4]; 4] {
    let ext = [corner, above[0], above[1], above[2], above[3], above[4]];
    let row = [
        avg3p(&ext, 1),
        avg3p(&ext, 2),
        avg3p(&ext, 3),
        avg3p(&ext, 4),
    ];
    [row, row, row, row]
}

/// `B_HE_PRED`: every column is the 3-tap-smoothed `left` column; the
/// bottom row duplicates `left[3]` since `left[4]` does not exist.
#[must_use]
pub fn b_he(left: &[u8; 4], corner: u8) -> [[u8; 4]; 4] {
    let ext = [corner, left[0], left[1], left[2], left[3]];
    let v0 = avg3p(&ext, 1);
    let v1 = avg3p(&ext, 2);
    let v2 = avg3p(&ext, 3);
    let v3 = avg3(left[2], left[3], left[3]);
    [[v0; 4], [v1; 4], [v2; 4], [v3; 4]]
}

/// `B_LD_PRED`: southwest diagonal from `above` (including above-right);
/// the final corner duplicates `above[7]` since `above[8]` does not exist.
#[must_use]
pub fn b_ld(above: &[u8; 8]) -> [[u8; 4]; 4] {
    let a6 = above.get(6).copied().unwrap_or(0);
    let a7 = above.get(7).copied().unwrap_or(0);
    let mut out = [[0u8; 4]; 4];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, px) in row.iter_mut().enumerate() {
            let k = r + c;
            *px = if k == 6 {
                avg3(a6, a7, a7)
            } else {
                avg3p(above, k + 1)
            };
        }
    }
    out
}

/// `B_RD_PRED`: southeast diagonal from `E` (left/corner/above, no above-right).
#[must_use]
pub fn b_rd(above: &[u8; 8], left: &[u8; 4], corner: u8) -> [[u8; 4]; 4] {
    let e = edge_array(above, left, corner);
    let mut out = [[0u8; 4]; 4];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, px) in row.iter_mut().enumerate() {
            // RFC 6386 §12.3: idx = c - r + 4 maps (r=3,c=0)->1 .. (r=0,c=3)->7,
            // indexing avg3p(E+idx) for idx in 1..=7.
            let d = i32::try_from(c).unwrap_or(0) - i32::try_from(r).unwrap_or(0) + 4;
            let idx = usize::try_from(d.clamp(1, 7)).unwrap_or(4);
            *px = avg3p(&e, idx);
        }
    }
    out
}

/// `B_VR_PRED`: vertical-right, mixed 3-tap/2-tap from `E`.
#[must_use]
pub fn b_vr(above: &[u8; 8], left: &[u8; 4], corner: u8) -> [[u8; 4]; 4] {
    let e = edge_array(above, left, corner);
    [
        [avg2p(&e, 4), avg2p(&e, 5), avg2p(&e, 6), avg2p(&e, 7)],
        [avg3p(&e, 4), avg3p(&e, 5), avg3p(&e, 6), avg3p(&e, 7)],
        [avg3p(&e, 3), avg2p(&e, 4), avg2p(&e, 5), avg2p(&e, 6)],
        [avg3p(&e, 2), avg3p(&e, 4), avg3p(&e, 5), avg3p(&e, 6)],
    ]
}

/// `B_VL_PRED`: vertical-left from `above` including above-right, with the
/// two irregular final entries the RFC calls out explicitly.
#[must_use]
pub fn b_vl(above: &[u8; 8]) -> [[u8; 4]; 4] {
    [
        [
            avg2p(above, 0),
            avg2p(above, 1),
            avg2p(above, 2),
            avg2p(above, 3),
        ],
        [
            avg3p(above, 1),
            avg3p(above, 2),
            avg3p(above, 3),
            avg3p(above, 4),
        ],
        [
            avg2p(above, 1),
            avg2p(above, 2),
            avg2p(above, 3),
            avg3p(above, 5),
        ],
        [
            avg3p(above, 2),
            avg3p(above, 3),
            avg3p(above, 4),
            avg3p(above, 6),
        ],
    ]
}

/// `B_HD_PRED`: horizontal-down from `E`.
#[must_use]
pub fn b_hd(above: &[u8; 8], left: &[u8; 4], corner: u8) -> [[u8; 4]; 4] {
    let e = edge_array(above, left, corner);
    [
        [avg2p(&e, 3), avg3p(&e, 4), avg3p(&e, 5), avg3p(&e, 6)],
        [avg2p(&e, 2), avg3p(&e, 3), avg2p(&e, 3), avg3p(&e, 4)],
        [avg2p(&e, 1), avg3p(&e, 2), avg2p(&e, 2), avg3p(&e, 3)],
        [avg2p(&e, 0), avg3p(&e, 1), avg2p(&e, 1), avg3p(&e, 2)],
    ]
}

/// `B_HU_PRED`: horizontal-up from `left` only; the bottom-right region has
/// no reconstructed neighbours on its diagonals and is filled with `left[3]`.
#[must_use]
pub fn b_hu(left: &[u8; 4]) -> [[u8; 4]; 4] {
    let l3 = left[3];
    [
        [
            avg2(left[0], left[1]),
            avg3(left[0], left[1], left[2]),
            avg2(left[1], left[2]),
            avg3(left[1], left[2], left[3]),
        ],
        [
            avg2(left[1], left[2]),
            avg3(left[1], left[2], left[3]),
            avg2(left[2], left[3]),
            avg3(left[2], left[3], left[3]),
        ],
        [
            avg2(left[2], left[3]),
            avg3(left[2], left[3], left[3]),
            l3,
            l3,
        ],
        [l3, l3, l3, l3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v_pred_copies_the_above_row_down() {
        let above = [10, 20, 30, 40];
        assert_eq!(predict_v(&above), [[10, 20, 30, 40]; 4]);
    }

    #[test]
    fn h_pred_copies_the_left_column_across() {
        let left = [10, 20, 30, 40];
        let out = predict_h(&left);
        for (l, row) in left.iter().zip(out.iter()) {
            assert_eq!(*row, [*l; 4]);
        }
    }

    #[test]
    fn tm_pred_matches_the_formula() {
        let above = [10, 20, 30, 40];
        let left = [1, 2, 3, 4];
        let out = predict_tm(&above, &left, 5);
        assert_eq!(out[0][0], clamp255(1 + 10 - 5));
        assert_eq!(out[3][3], clamp255(4 + 40 - 5));
    }

    #[test]
    fn dc_pred_averages_both_sides_when_available() {
        let above = [10u8; 8];
        let left = [30u8; 8];
        let out = predict_dc(Some(&above), Some(&left));
        assert_eq!(out[0][0], 20); // (80+240+8)>>4 = 20.5 -> 20
    }

    #[test]
    fn dc_pred_falls_back_to_128_at_the_top_left_corner() {
        let out = predict_dc::<16>(None, None);
        assert_eq!(out[0][0], 128);
    }

    #[test]
    fn dc_pred_uses_only_the_available_side() {
        let left = [40u8; 4];
        let out = predict_dc(None, Some(&left));
        assert_eq!(out[0][0], 40);
    }

    #[test]
    fn b_dc_always_uses_both_sides_even_when_they_are_fill_values() {
        let above = [OFF_FRAME_ABOVE; 4];
        let left = [OFF_FRAME_LEFT; 4];
        let out = b_dc(&above, &left);
        let expected = ((4 * 127 + 4 * 129 + 4) >> 3) as u8;
        assert_eq!(out[0][0], expected);
    }

    #[test]
    fn b_hu_fills_the_unreachable_corner_with_left3() {
        let left = [1, 2, 3, 9];
        let out = b_hu(&left);
        assert_eq!(out[3], [9, 9, 9, 9]);
        assert_eq!(out[2][2], 9);
        assert_eq!(out[2][3], 9);
    }

    proptest::proptest! {
        #[test]
        fn every_mode_produces_valid_bytes_for_arbitrary_input(
            above in proptest::array::uniform8(proptest::prelude::any::<u8>()),
            left in proptest::array::uniform4(proptest::prelude::any::<u8>()),
            corner in proptest::prelude::any::<u8>(),
        ) {
            let above4 = [above[0], above[1], above[2], above[3]];
            let _ = predict_v(&above4);
            let _ = predict_h(&left);
            let _ = predict_tm(&above4, &left, corner);
            let _ = predict_dc(Some(&above4), Some(&left));
            let _ = b_dc(&above4, &left);
            let _ = b_ve(&above, corner);
            let _ = b_he(&left, corner);
            let _ = b_ld(&above);
            let _ = b_rd(&above, &left, corner);
            let _ = b_vr(&above, &left, corner);
            let _ = b_vl(&above);
            let _ = b_hd(&above, &left, corner);
            let _ = b_hu(&left);
        }
    }
}
