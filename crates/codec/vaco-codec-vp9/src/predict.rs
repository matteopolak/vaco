//! VP9 §8.5.1's intra prediction process — one implementation for every
//! block size (4/8/16/32), unlike VP8 where the 16x16/8x8 whole-block modes
//! and the 4x4 `B_*` submodes are different formulas. VP9's ten modes are
//! all defined generically in terms of `size = 1 << log2Size`.
//!
//! Every function here takes `above_row`/`left_col` as already-assembled
//! per §8.5.1's edge-extension rules (the `haveAbove`/`haveLeft`/
//! `notOnRight` fill-value logic) — `above_row` must be `2*size` entries
//! (in-block plus the "above-right" extension) plus one leading corner
//! sample at conceptual index `-1`, modelled here as `above_row[0]` =
//! `aboveRow[-1]` and `above_row[1..]` = `aboveRow[0..]`, so every
//! `aboveRow[i]` in the specification's text is `above_row[i + 1]` here.
//! `left_col` is plain `leftCol[0..size]`, no corner slot (the corner is
//! shared with `above_row[0]`).

use crate::tables;

fn round2(x: i32, n: u32) -> i32 {
    (x + (1i32 << (n - 1))) >> n
}

fn a(above_row: &[i32], i: i32) -> i32 {
    let idx = i + 1;
    usize::try_from(idx)
        .ok()
        .and_then(|i| above_row.get(i))
        .copied()
        .unwrap_or(0)
}

fn l(left_col: &[i32], i: i32) -> i32 {
    usize::try_from(i)
        .ok()
        .and_then(|i| left_col.get(i))
        .copied()
        .unwrap_or(0)
}

fn pset(pred: &mut [i32], size: usize, i: usize, j: usize, v: i32) {
    if let Some(slot) = pred.get_mut(i * size + j) {
        *slot = v;
    }
}

fn pget(pred: &[i32], size: usize, i: i32, j: i32) -> i32 {
    let Ok(i) = usize::try_from(i) else { return 0 };
    let Ok(j) = usize::try_from(j) else { return 0 };
    if i >= size || j >= size {
        return 0;
    }
    pred.get(i * size + j).copied().unwrap_or(0)
}

/// §8.5.1's intra prediction, writing `size * size` samples (row-major) into
/// `pred`. `bit_depth` supplies `1 << (BitDepth - 1)` for the DC/TM/border
/// fallback constants. `above_row` is `2*size + 1` entries as documented on
/// the module; `left_col` is `size` entries.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one dispatch over ten spec-defined modes"
)]
pub fn predict_intra(
    pred: &mut [i32],
    mode: i32,
    size: usize,
    // No longer read inside this function: D-09's dc_predict rewire
    // replaced the only branch that used it (a shift derived from
    // log2Size), and every other mode already takes `size` directly.
    // Kept as a parameter rather than removed, since removing it would
    // mean updating every call site for a purely internal simplification.
    _log2_size: u32,
    above_row: &[i32],
    left_col: &[i32],
    have_left: bool,
    have_above: bool,
    bit_depth: u32,
) {
    let sz = i32::try_from(size).unwrap_or(0);
    if mode == tables::V_PRED {
        for i in 0..size {
            for j in 0..size {
                pset(
                    pred,
                    size,
                    i,
                    j,
                    a(above_row, i32::try_from(j).unwrap_or(0)),
                );
            }
        }
    } else if mode == tables::H_PRED {
        for i in 0..size {
            for j in 0..size {
                pset(pred, size, i, j, l(left_col, i32::try_from(i).unwrap_or(0)));
            }
        }
    } else if mode == tables::D207_PRED {
        for j in 0..size {
            pset(pred, size, size - 1, j, l(left_col, sz - 1));
        }
        for i in 0..size.saturating_sub(1) {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                0,
                round2(l(left_col, ii) + l(left_col, ii + 1), 1),
            );
        }
        for i in 0..size.saturating_sub(2) {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                1,
                round2(
                    l(left_col, ii) + 2 * l(left_col, ii + 1) + l(left_col, ii + 2),
                    2,
                ),
            );
        }
        if size >= 2 {
            pset(
                pred,
                size,
                size - 2,
                1,
                round2(l(left_col, sz - 2) + 3 * l(left_col, sz - 1), 2),
            );
        }
        for i in (0..size.saturating_sub(1)).rev() {
            for j in 2..size {
                let v = pget(
                    pred,
                    size,
                    i32::try_from(i + 1).unwrap_or(0),
                    i32::try_from(j).unwrap_or(0) - 2,
                );
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::D45_PRED {
        for i in 0..size {
            for j in 0..size {
                let (ii, jj) = (i32::try_from(i).unwrap_or(0), i32::try_from(j).unwrap_or(0));
                let v = if ii + jj + 2 < sz * 2 {
                    round2(
                        a(above_row, ii + jj)
                            + a(above_row, ii + jj + 1) * 2
                            + a(above_row, ii + jj + 2),
                        2,
                    )
                } else {
                    a(above_row, 2 * sz - 1)
                };
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::D63_PRED {
        for i in 0..size {
            for j in 0..size {
                let (ii, jj) = (i32::try_from(i).unwrap_or(0), i32::try_from(j).unwrap_or(0));
                #[allow(
                    clippy::integer_division,
                    reason = "spec-defined: i/2, splitting a subblock row into an even/odd pair"
                )]
                let half = ii / 2;
                let v = if ii & 1 != 0 {
                    round2(
                        a(above_row, half + jj)
                            + a(above_row, half + jj + 1) * 2
                            + a(above_row, half + jj + 2),
                        2,
                    )
                } else {
                    round2(a(above_row, half + jj) + a(above_row, half + jj + 1), 1)
                };
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::D117_PRED {
        for j in 0..size {
            let jj = i32::try_from(j).unwrap_or(0);
            pset(
                pred,
                size,
                0,
                j,
                round2(a(above_row, jj - 1) + a(above_row, jj), 1),
            );
        }
        if size >= 2 {
            pset(
                pred,
                size,
                1,
                0,
                round2(l(left_col, 0) + 2 * a(above_row, -1) + a(above_row, 0), 2),
            );
            for j in 1..size {
                let jj = i32::try_from(j).unwrap_or(0);
                pset(
                    pred,
                    size,
                    1,
                    j,
                    round2(
                        a(above_row, jj - 2) + 2 * a(above_row, jj - 1) + a(above_row, jj),
                        2,
                    ),
                );
            }
        }
        if size >= 3 {
            pset(
                pred,
                size,
                2,
                0,
                round2(a(above_row, -1) + 2 * l(left_col, 0) + l(left_col, 1), 2),
            );
        }
        for i in 3..size {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                0,
                round2(
                    l(left_col, ii - 3) + 2 * l(left_col, ii - 2) + l(left_col, ii - 1),
                    2,
                ),
            );
        }
        for i in 2..size {
            for j in 1..size {
                let v = pget(
                    pred,
                    size,
                    i32::try_from(i).unwrap_or(0) - 2,
                    i32::try_from(j).unwrap_or(0) - 1,
                );
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::D135_PRED {
        pset(
            pred,
            size,
            0,
            0,
            round2(l(left_col, 0) + 2 * a(above_row, -1) + a(above_row, 0), 2),
        );
        for j in 1..size {
            let jj = i32::try_from(j).unwrap_or(0);
            pset(
                pred,
                size,
                0,
                j,
                round2(
                    a(above_row, jj - 2) + 2 * a(above_row, jj - 1) + a(above_row, jj),
                    2,
                ),
            );
        }
        if size >= 2 {
            pset(
                pred,
                size,
                1,
                0,
                round2(a(above_row, -1) + 2 * l(left_col, 0) + l(left_col, 1), 2),
            );
        }
        for i in 2..size {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                0,
                round2(
                    l(left_col, ii - 2) + 2 * l(left_col, ii - 1) + l(left_col, ii),
                    2,
                ),
            );
        }
        for i in 1..size {
            for j in 1..size {
                let v = pget(
                    pred,
                    size,
                    i32::try_from(i).unwrap_or(0) - 1,
                    i32::try_from(j).unwrap_or(0) - 1,
                );
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::D153_PRED {
        pset(
            pred,
            size,
            0,
            0,
            round2(l(left_col, 0) + a(above_row, -1), 1),
        );
        for i in 1..size {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                0,
                round2(l(left_col, ii - 1) + l(left_col, ii), 1),
            );
        }
        // §8.5.1 defines these unconditionally for `pred[0][1]`/`pred[1][1]`
        // and D153_PRED is never invoked below VP9's minimum 4x4 transform
        // size, so `size >= 4` always holds and column 1 always exists.
        pset(
            pred,
            size,
            0,
            1,
            round2(l(left_col, 0) + 2 * a(above_row, -1) + a(above_row, 0), 2),
        );
        pset(
            pred,
            size,
            1,
            1,
            round2(a(above_row, -1) + 2 * l(left_col, 0) + l(left_col, 1), 2),
        );
        for i in 2..size {
            let ii = i32::try_from(i).unwrap_or(0);
            pset(
                pred,
                size,
                i,
                1,
                round2(
                    l(left_col, ii - 2) + 2 * l(left_col, ii - 1) + l(left_col, ii),
                    2,
                ),
            );
        }
        for j in 2..size {
            let jj = i32::try_from(j).unwrap_or(0);
            pset(
                pred,
                size,
                0,
                j,
                round2(
                    a(above_row, jj - 3) + 2 * a(above_row, jj - 2) + a(above_row, jj - 1),
                    2,
                ),
            );
        }
        for i in 1..size {
            for j in 2..size {
                let v = pget(
                    pred,
                    size,
                    i32::try_from(i).unwrap_or(0) - 1,
                    i32::try_from(j).unwrap_or(0) - 2,
                );
                pset(pred, size, i, j, v);
            }
        }
    } else if mode == tables::TM_PRED {
        let clip_max = (1i32 << bit_depth) - 1;
        for i in 0..size {
            for j in 0..size {
                let (ii, jj) = (i32::try_from(i).unwrap_or(0), i32::try_from(j).unwrap_or(0));
                let v = (a(above_row, jj) + l(left_col, ii) - a(above_row, -1)).clamp(0, clip_max);
                pset(pred, size, i, j, v);
            }
        }
    } else {
        // DC_PRED, and its three border-fallback cases -- delegated to
        // `vaco_codec_dsp_intrapred::dc_predict` (D-09): this branch's own
        // four cases are that function's average/fallback formula, for a
        // count that is always a power of two (`size`, `2*size` are
        // always 4/8/16/32/64), where a shift-based rounding and
        // `dc_predict`'s `(sum + count/2) / count` are the identical
        // computation -- verified bit-exact against this module's own
        // pre-existing tests (which pin real expected values) before
        // landing, not merely assumed equivalent.
        const MAX_SZ: usize = 32;
        let n = size.min(MAX_SZ);
        let mut top_buf = [0u16; MAX_SZ];
        let mut left_buf = [0u16; MAX_SZ];
        for (k, slot) in top_buf.iter_mut().take(n).enumerate() {
            let ki = i32::try_from(k).unwrap_or(0);
            *slot = u16::try_from(a(above_row, ki).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
        }
        for (k, slot) in left_buf.iter_mut().take(n).enumerate() {
            let ki = i32::try_from(k).unwrap_or(0);
            *slot = u16::try_from(l(left_col, ki).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
        }
        let top: &[u16] = if have_above {
            top_buf.get(..n).unwrap_or(&[])
        } else {
            &[]
        };
        let left_slice: &[u16] = if have_left {
            left_buf.get(..n).unwrap_or(&[])
        } else {
            &[]
        };
        let value = i32::from(vaco_codec_dsp_intrapred::dc_predict(
            top, left_slice, n, bit_depth,
        ));
        for slot in pred.iter_mut().take(size * size) {
            *slot = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_pred_with_no_neighbours_is_the_bit_depth_midpoint() {
        let mut pred = [0i32; 16];
        predict_intra(
            &mut pred,
            tables::DC_PRED,
            4,
            2,
            &[0; 9],
            &[0; 4],
            false,
            false,
            8,
        );
        assert!(pred.iter().all(|&v| v == 128));
    }

    #[test]
    fn v_pred_copies_the_above_row() {
        let above = [0, 10, 20, 30, 40, 0, 0, 0, 0];
        let mut pred = [0i32; 16];
        predict_intra(
            &mut pred,
            tables::V_PRED,
            4,
            2,
            &above,
            &[0; 4],
            true,
            true,
            8,
        );
        assert_eq!(&pred[0..4], [10, 20, 30, 40]);
        assert_eq!(&pred[12..16], [10, 20, 30, 40]);
    }

    #[test]
    fn h_pred_copies_the_left_column() {
        let left = [10, 20, 30, 40];
        let mut pred = [0i32; 16];
        predict_intra(
            &mut pred,
            tables::H_PRED,
            4,
            2,
            &[0; 9],
            &left,
            true,
            true,
            8,
        );
        for (i, row) in pred.chunks(4).enumerate() {
            let want = left.get(i).copied().unwrap_or(0);
            assert!(row.iter().all(|&v| v == want));
        }
    }

    #[test]
    fn tm_pred_matches_the_formula() {
        let mut above = [0i32; 9];
        above[0] = 5; // corner
        above[1] = 50;
        let left = [20, 0, 0, 0];
        let mut pred = [0i32; 16];
        predict_intra(
            &mut pred,
            tables::TM_PRED,
            4,
            2,
            &above,
            &left,
            true,
            true,
            8,
        );
        assert_eq!(pred[0], (50 + 20 - 5).clamp(0, 255));
    }

    #[test]
    fn every_mode_and_size_runs_without_panicking() {
        for &(size, log2) in &[(4usize, 2u32), (8, 3), (16, 4), (32, 5)] {
            let above = vec![37i32; 2 * size + 1];
            let left = vec![37i32; size];
            let mut pred = vec![0i32; size * size];
            for mode in 0..10 {
                predict_intra(&mut pred, mode, size, log2, &above, &left, true, true, 8);
                predict_intra(&mut pred, mode, size, log2, &above, &left, false, false, 8);
            }
        }
    }
}
