//! Widening pixel-block extraction: reading an `w x h` region out of a
//! strided `u8` plane into a contiguous, wider buffer.
//!
//! Both functions here are the encoder-side counterpart of
//! [`crate::blockdsp`]'s reconstruction: before a forward transform can run,
//! an encoder needs the source block (or the source-minus-prediction
//! residual) as a contiguous `i16` array, exactly the shape every
//! block-based encoder's residual-formation step needs regardless of which
//! transform follows it.
//!
//! `clippy::indexing_slicing` is denied workspace-wide, so every access
//! here goes through iterator adapters or `.get()`/`.get_mut()`.

/// Copy a `w x h` block from a strided `u8` plane into a contiguous `i16`
/// buffer, widening each sample.
///
/// `src` is read `h` rows of `w` samples each, `stride` samples apart;
/// `dst` is written row-major with no padding (`dst[y * w + x]`). Reads and
/// writes stop early if either buffer is shorter than the block needs,
/// rather than panicking — an out-of-range `stride`/`w`/`h` combination is a
/// caller bug this function reports by leaving the untouched part of `dst`
/// at whatever it already held, not by aborting.
pub fn get_pixels(dst: &mut [i16], src: &[u8], stride: usize, w: usize, h: usize) {
    for row in 0..h {
        let Some(src_row) = src
            .get(row.saturating_mul(stride)..)
            .and_then(|r| r.get(..w))
        else {
            return;
        };
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(w)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        for (d, &s) in dst_row.iter_mut().zip(src_row) {
            *d = i16::from(s);
        }
    }
}

/// `dst[y * w + x] = src1[y, x] - src2[y, x]` over a `w x h` block, each
/// source read from its own strided plane — the residual an encoder forms
/// between a source block and its prediction, before the forward
/// transform.
///
/// Same truncate-rather-than-panic contract as [`get_pixels`].
pub fn diff_pixels(
    dst: &mut [i16],
    src1: &[u8],
    stride1: usize,
    src2: &[u8],
    stride2: usize,
    w: usize,
    h: usize,
) {
    for row in 0..h {
        let Some(row1) = src1
            .get(row.saturating_mul(stride1)..)
            .and_then(|r| r.get(..w))
        else {
            return;
        };
        let Some(row2) = src2
            .get(row.saturating_mul(stride2)..)
            .and_then(|r| r.get(..w))
        else {
            return;
        };
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(w)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        for ((d, &a), &b) in dst_row.iter_mut().zip(row1).zip(row2) {
            *d = i16::from(a) - i16::from(b);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test setup building small fixed-size fixtures; an out-of-range index here is itself a test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn get_pixels_widens_a_strided_block() {
        // 4-wide stride, 2x2 block starting at the origin.
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut dst = [0i16; 4];
        get_pixels(&mut dst, &src, 4, 2, 2);
        // row0 = src[0..2] = [1,2]; row1 = src[4..6] = [5,6].
        assert_eq!(dst, [1, 2, 5, 6]);
    }

    #[test]
    fn get_pixels_full_range_matches_hand_computed() {
        let src: [u8; 16] = core::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let mut dst = [0i16; 16];
        get_pixels(&mut dst, &src, 4, 4, 4);
        let expect: [i16; 16] = core::array::from_fn(|i| i16::try_from(i).unwrap_or(0));
        assert_eq!(dst, expect);
    }

    #[test]
    fn diff_pixels_matches_hand_computed() {
        let src1 = [10u8, 20, 30, 40];
        let src2 = [1u8, 2, 3, 4];
        let mut dst = [0i16; 4];
        diff_pixels(&mut dst, &src1, 2, &src2, 2, 2, 2);
        assert_eq!(dst, [9, 18, 27, 36]);
    }

    #[test]
    fn diff_pixels_is_negative_when_second_source_is_larger() {
        let src1 = [1u8, 1];
        let src2 = [10u8, 20];
        let mut dst = [0i16; 2];
        diff_pixels(&mut dst, &src1, 2, &src2, 2, 2, 1);
        assert_eq!(dst, [-9, -19]);
    }

    #[test]
    fn undersized_src_stops_without_panicking() {
        let src = [1u8, 2, 3]; // not enough for a 2x2 block at stride 2
        let mut dst = [9i16; 4];
        get_pixels(&mut dst, &src, 2, 2, 2);
        // First row copies fine, second row is short and is skipped.
        assert_eq!(&dst[..2], &[1, 2]);
    }

    proptest::proptest! {
        #[test]
        fn get_pixels_never_panics(
            src in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
            stride in 0usize..32,
            w in 0usize..32,
            h in 0usize..32,
        ) {
            let mut dst = vec![0i16; w.saturating_mul(h)];
            get_pixels(&mut dst, &src, stride, w, h);
        }

        #[test]
        fn diff_pixels_never_panics(
            src1 in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
            src2 in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
            stride1 in 0usize..32,
            stride2 in 0usize..32,
            w in 0usize..32,
            h in 0usize..32,
        ) {
            let mut dst = vec![0i16; w.saturating_mul(h)];
            diff_pixels(&mut dst, &src1, stride1, &src2, stride2, w, h);
        }
    }
}
