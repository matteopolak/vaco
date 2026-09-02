//! Post-transform block reconstruction: clearing a coefficient block, and
//! writing a reconstructed (or residual-plus-prediction) block back into a
//! strided pixel plane with saturation.
//!
//! [`add_pixels_clamped`] is every block-based video standard's
//! reconstruction equation in one place: H.264 §8.5's general decoding
//! process for a macroblock, HEVC §8.6.5's picture reconstruction process
//! and MPEG-2's equivalent all define the same
//! `recSample = Clip1(pred + residual)` step, saturating to the sample's
//! bit depth (`[0, 255]` for 8-bit, the common case here). It is
//! format-dictated arithmetic, not an authorial choice — every conforming
//! decoder computes exactly this — which is why it belongs in the shared
//! crate rather than being written once per codec (D19).
//!
//! `clippy::indexing_slicing` is denied workspace-wide, so every access
//! here goes through iterator adapters or `.get()`/`.get_mut()`.

/// Zero every element of a coefficient block. A block-based decoder clears
/// its working coefficient buffer before an entropy-decode pass fills in
/// only the non-zero positions it actually signals.
pub fn clear_block(block: &mut [i16]) {
    for v in block {
        *v = 0;
    }
}

/// Fill a `w x h` region of a strided `u8` plane with a constant value —
/// the fast path for a fully-predicted block with no residual at all (a
/// skipped macroblock, or a DC-only block whose residual is uniform).
///
/// Same truncate-rather-than-panic contract as
/// [`crate::pixblockdsp::get_pixels`]: writes stop early if `dst` is
/// shorter than the region needs.
pub fn fill_block(dst: &mut [u8], stride: usize, w: usize, h: usize, value: u8) {
    for row in 0..h {
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(stride)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        dst_row.fill(value);
    }
}

/// Write a `w x h` block of already-final sample values (an `i16` block
/// whose values already represent the complete reconstructed sample, not a
/// residual) into a strided `u8` plane, clamping to `[0, 255]`.
///
/// Used where there is no separate prediction to add — a lossless
/// transform-bypass path, or a block whose "prediction" is folded into the
/// same buffer already.
pub fn put_pixels_clamped(src: &[i16], dst: &mut [u8], stride: usize, w: usize, h: usize) {
    for row in 0..h {
        let Some(src_row) = src.get(row.saturating_mul(w)..).and_then(|r| r.get(..w)) else {
            return;
        };
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(stride)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        for (d, &s) in dst_row.iter_mut().zip(src_row) {
            *d = s.clamp(0, 255) as u8;
        }
    }
}

/// `dst[y, x] = Clip1(dst[y, x] + residual[y * w + x])` over a `w x h`
/// block — add an inverse-transformed residual onto an existing prediction
/// already sitting in `dst`, saturating to `[0, 255]`. See the module doc
/// for why this one function is every format's reconstruction step.
///
/// Same truncate-rather-than-panic contract as the rest of this module.
pub fn add_pixels_clamped(residual: &[i16], dst: &mut [u8], stride: usize, w: usize, h: usize) {
    for row in 0..h {
        let Some(res_row) = residual
            .get(row.saturating_mul(w)..)
            .and_then(|r| r.get(..w))
        else {
            return;
        };
        let Some(dst_row) = dst
            .get_mut(row.saturating_mul(stride)..)
            .and_then(|r| r.get_mut(..w))
        else {
            return;
        };
        for (d, &r) in dst_row.iter_mut().zip(res_row) {
            let sum = i32::from(*d) + i32::from(r);
            *d = sum.clamp(0, 255) as u8;
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
    fn clear_block_zeroes_everything() {
        let mut block = [1i16, -2, 3, -4];
        clear_block(&mut block);
        assert_eq!(block, [0, 0, 0, 0]);
    }

    #[test]
    fn fill_block_matches_hand_computed() {
        let mut dst = [9u8; 8]; // 4-wide stride, 2 rows
        fill_block(&mut dst, 4, 2, 2, 5);
        assert_eq!(dst, [5, 5, 9, 9, 5, 5, 9, 9]);
    }

    #[test]
    fn put_pixels_clamped_saturates_both_directions() {
        let src = [-10i16, 300, 128, 0];
        let mut dst = [0u8; 4];
        put_pixels_clamped(&src, &mut dst, 2, 2, 2);
        assert_eq!(dst, [0, 255, 128, 0]);
    }

    #[test]
    fn add_pixels_clamped_matches_hand_computed_no_saturation() {
        let mut dst = [100u8, 100, 100, 100];
        let residual = [10i16, -10, 0, 5];
        add_pixels_clamped(&residual, &mut dst, 2, 2, 2);
        assert_eq!(dst, [110, 90, 100, 105]);
    }

    #[test]
    fn add_pixels_clamped_saturates_high_and_low() {
        let mut dst = [250u8, 5];
        let residual = [100i16, -100];
        add_pixels_clamped(&residual, &mut dst, 2, 2, 1);
        assert_eq!(dst, [255, 0]);
    }

    #[test]
    fn add_pixels_clamped_is_the_reconstruction_identity_at_zero_residual() {
        // Clip1(pred + 0) == pred for every legal 8-bit pred value.
        for pred in 0u8..=255 {
            let mut dst = [pred];
            add_pixels_clamped(&[0], &mut dst, 1, 1, 1);
            assert_eq!(dst[0], pred);
        }
    }

    proptest::proptest! {
        #[test]
        fn add_pixels_clamped_never_panics(
            dst_init in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            residual in proptest::collection::vec(proptest::num::i16::ANY, 0..64),
            stride in 0usize..16,
            w in 0usize..16,
            h in 0usize..16,
        ) {
            let mut dst = dst_init;
            add_pixels_clamped(&residual, &mut dst, stride, w, h);
        }

        #[test]
        fn fill_block_never_panics(
            dst_init in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            stride in 0usize..16,
            w in 0usize..16,
            h in 0usize..16,
            value in proptest::num::u8::ANY,
        ) {
            let mut dst = dst_init;
            fill_block(&mut dst, stride, w, h, value);
        }
    }
}
