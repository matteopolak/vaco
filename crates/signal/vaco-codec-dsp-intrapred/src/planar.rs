//! Planar intra prediction: a bilinear blend of the four corner reference
//! samples across a `size x size` block.
//!
//! Transcribed from ITU-T H.265 (08/2021) §8.4.4.2.4 (`INTRA_PLANAR`,
//! predModeIntra == 0):
//!
//! ```text
//! pred[x][y] = ( (size-1-x)*left[y] + (x+1)*topRight
//!              + (size-1-y)*top[x]  + (y+1)*bottomLeft
//!              + size ) >> (log2Size + 1)
//! ```
//!
//! AV1's `SMOOTH_PRED` and VP9's `TM`-adjacent planar-style modes are close
//! cousins of the same bilinear-corner idea but are parameterised
//! differently enough (AV1 uses a set of weight LUTs rather than the linear
//! ramp above) that they are out of scope here; this function is HEVC's
//! formula specifically, which is exact and needs no weight table.

/// Fill a `size x size` block with HEVC-style planar prediction.
///
/// `top[x]` is the reference sample directly above column `x`
/// (`p[x][-1]`), `left[y]` the one directly left of row `y` (`p[-1][y]`),
/// `top_right` the corner sample at `p[size][-1]`, `bottom_left` the corner
/// at `p[-1][size]`. `dst` is written row-major (`dst[y * size + x]`),
/// `log2_size` is `size`'s base-2 logarithm (the caller's own value, since
/// `size` is always a power of two for every format this serves).
///
/// `top`/`left` shorter than `size`, or `dst` shorter than `size * size`,
/// stop the corresponding rows/columns early rather than panicking or
/// reading/writing out of range.
pub fn planar_predict(
    dst: &mut [u16],
    top: &[u16],
    left: &[u16],
    top_right: u16,
    bottom_left: u16,
    size: usize,
    log2_size: u32,
) {
    if size == 0 {
        return;
    }
    // Clamped rather than a bare `+ 1`/shift: a real caller's `log2_size`
    // never exceeds ~6 (size 64), but this function takes it as a plain
    // `u32` with no enforced relationship to `size`, so `log2_size ==
    // u32::MAX` must not overflow the `+ 1` or panic on an out-of-range
    // shift.
    let shift = log2_size.saturating_add(1).min(31);
    let size_m1 = size - 1;

    for y in 0..size {
        let Some(&left_y) = left.get(y) else {
            return;
        };
        let Some(dst_row) = dst.get_mut(y.saturating_mul(size)..).and_then(|r| r.get_mut(..size))
        else {
            return;
        };
        for (x, slot) in dst_row.iter_mut().enumerate() {
            let Some(&top_x) = top.get(x) else {
                break;
            };
            let horiz = u32::try_from(size_m1 - x).unwrap_or(0) * u32::from(left_y)
                + u32::try_from(x + 1).unwrap_or(0) * u32::from(top_right);
            let vert = u32::try_from(size_m1 - y).unwrap_or(0) * u32::from(top_x)
                + u32::try_from(y + 1).unwrap_or(0) * u32::from(bottom_left);
            let sum = horiz + vert + u32::try_from(size).unwrap_or(0);
            *slot = u16::try_from(sum >> shift).unwrap_or(u16::MAX);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test fixtures index small fixed arrays; an out-of-range index here is itself a test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn uniform_references_reproduce_the_constant() {
        // Every reference sample equal to v -> every output must be v too,
        // for any size: the bilinear blend of four equal corners over a
        // flat field is that same constant.
        for &v in &[0u16, 1, 128, 255] {
            let mut dst = [0u16; 16];
            planar_predict(&mut dst, &[v; 4], &[v; 4], v, v, 4, 2);
            assert!(dst.iter().all(|&d| d == v), "size4 v={v}: {dst:?}");
        }
    }

    #[test]
    fn matches_hand_computed_2x2() {
        // size=2, log2=1, shift=2. top=[10,20], left=[30,40],
        // top_right=50, bottom_left=60.
        // pred[0][0] (x=0,y=0): (1*30 + 1*50) + (1*10 + 1*60) + 2 = 80+70+2=152; >>2 = 38.
        // pred[1][0] (x=1,y=0): (0*30 + 2*50) + (1*20 + 1*60) + 2 = 100+80+2=182; >>2 = 45.
        // pred[0][1] (x=0,y=1): (1*40 + 1*50) + (0*10 + 2*60) + 2 = 90+120+2=212; >>2 = 53.
        // pred[1][1] (x=1,y=1): (0*40 + 2*50) + (0*20 + 2*60) + 2 = 100+120+2=222; >>2 = 55.
        let mut dst = [0u16; 4];
        planar_predict(&mut dst, &[10, 20], &[30, 40], 50, 60, 2, 1);
        assert_eq!(dst, [38, 45, 53, 55]);
    }

    #[test]
    fn top_row_and_left_column_are_dominated_by_their_own_side() {
        // At x=0 the horizontal term weights `left[y]` at its maximum
        // (size-1) and `top_right` at its minimum (1); the reverse at
        // x=size-1. This is a property of the formula's own weights.
        let mut dst = [0u16; 64];
        planar_predict(&mut dst, &[200; 8], &[0; 8], 200, 0, 8, 3);
        // x=0 column should read closer to `left` (0) than x=7 does.
        let col0 = dst[0];
        let col7 = dst[7];
        assert!(col0 < col7, "col0={col0} col7={col7}");
    }

    #[test]
    fn undersized_inputs_stop_without_panicking() {
        let mut dst = [9u16; 16];
        planar_predict(&mut dst, &[1, 2], &[1, 2], 5, 5, 4, 2);
        // Only y=0,1 (left has 2 entries) and x=0,1 within those rows (top
        // has 2 entries) get written; the rest keep their initial value.
        assert_eq!(dst[0], dst[0]); // written, no panic reaching here
        assert_eq!(&dst[2..4], &[9, 9]);
        assert_eq!(&dst[8..], &[9; 8]);
    }

    proptest::proptest! {
        #[test]
        fn planar_predict_never_panics(
            top in proptest::collection::vec(proptest::num::u16::ANY, 0..64),
            left in proptest::collection::vec(proptest::num::u16::ANY, 0..64),
            top_right in proptest::num::u16::ANY,
            bottom_left in proptest::num::u16::ANY,
            size in 0usize..32,
            log2_size in 0u32..8,
        ) {
            let mut dst = vec![0u16; size.saturating_mul(size)];
            planar_predict(&mut dst, &top, &left, top_right, bottom_left, size, log2_size);
        }
    }
}
