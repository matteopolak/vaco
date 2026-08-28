//! Edge emulation: border replication for a motion vector that reaches past
//! the visible picture.
//!
//! A separable FIR interpolation filter reads several samples on each side of
//! its output position — up to three on either side for a six-tap filter.
//! Near a picture edge, an unrestricted motion vector routinely asks for
//! samples outside `0..width`/`0..height`. Every block-based codec's own
//! reference behaviour (ITU-T H.264 §8.4.2.2's "unavailable sample" rule, and
//! the equivalent clause in every later codec) is the same: clamp the
//! requested coordinate to the nearest in-picture sample rather than treating
//! it as an error.
//!
//! This is a **setup** step, not the hot loop the FIR taps run in, so it is
//! plain scalar code: one clamped copy per destination pixel, called once per
//! block rather than once per tap.

/// Fill `dst` (a `dst_w × dst_h` block, row-major, stride `dst_w`) from `src`
/// (a `src_w × src_h` plane, row-major, stride `src_stride`), clamping the
/// requested origin `(x0, y0)` — which may be negative, or reach past the
/// source in either axis — to the nearest source sample per pixel.
///
/// `src` must hold at least `src_h * src_stride` samples and `src_stride`
/// must be at least `src_w`; a source too short to index safely is treated as
/// `1×1` (every destination pixel reads `src[0]`) rather than panicking or
/// reading out of bounds, since this is a library entry point over
/// caller-controlled dimensions.
pub fn extend_edges(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    src_stride: usize,
    x0: i64,
    y0: i64,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    let (src_w, src_h, src_stride) = if src_stride < src_w || src.len() < src_h * src_stride {
        (1, 1, 1)
    } else {
        (src_w, src_h, src_stride)
    };
    if dst_w == 0 || dst_h == 0 {
        return;
    }

    let clamp_axis = |v: i64, len: usize| -> usize {
        let max = i64::try_from(len.saturating_sub(1)).unwrap_or(0);
        v.clamp(0, max).unsigned_abs() as usize
    };

    for (row_idx, row) in dst.chunks_exact_mut(dst_w).take(dst_h).enumerate() {
        let Some(row_idx_i64) = i64::try_from(row_idx).ok() else {
            continue;
        };
        let sy = clamp_axis(y0.saturating_add(row_idx_i64), src_h);
        let Some(src_row) = src.get(sy.saturating_mul(src_stride)..) else {
            continue;
        };
        for (col_idx, out) in row.iter_mut().enumerate() {
            let Some(col_idx_i64) = i64::try_from(col_idx).ok() else {
                continue;
            };
            let sx = clamp_axis(x0.saturating_add(col_idx_i64), src_w);
            *out = src_row.get(sx).copied().unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interior_block_is_a_plain_copy() {
        #[rustfmt::skip]
        let src: [u8; 16] = [
            0, 1, 2, 3,
            4, 5, 6, 7,
            8, 9, 10, 11,
            12, 13, 14, 15,
        ];
        let mut dst = [0u8; 4];
        extend_edges(&src, 4, 4, 4, 1, 1, &mut dst, 2, 2);
        assert_eq!(dst, [5, 6, 9, 10]);
    }

    #[test]
    fn negative_origin_clamps_to_the_top_left_sample() {
        #[rustfmt::skip]
        let src: [u8; 4] = [
            9, 8,
            7, 6,
        ];
        let mut dst = [0u8; 9];
        extend_edges(&src, 2, 2, 2, -1, -1, &mut dst, 3, 3);
        // Every out-of-range read clamps to the nearest edge sample, so the
        // top-left 2x2 of the destination is all `9` (the corner), the top
        // row's third column clamps `x` to 1 while `y` is still clamped, etc.
        assert_eq!(dst, [9, 9, 8, 9, 9, 8, 7, 7, 6]);
    }

    #[test]
    fn origin_past_the_far_edge_clamps_to_the_bottom_right_sample() {
        #[rustfmt::skip]
        let src: [u8; 4] = [
            1, 2,
            3, 4,
        ];
        let mut dst = [0u8; 4];
        extend_edges(&src, 2, 2, 2, 5, 5, &mut dst, 2, 2);
        assert_eq!(dst, [4, 4, 4, 4]);
    }

    #[test]
    fn zero_sized_destination_touches_nothing() {
        let src = [1u8, 2, 3, 4];
        let mut dst: [u8; 0] = [];
        extend_edges(&src, 2, 2, 2, 0, 0, &mut dst, 0, 0);
        assert!(dst.is_empty());
    }
}
