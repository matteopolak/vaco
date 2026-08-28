//! DCT coefficient (residual token) decoding, RFC 6386 §13.
//!
//! Neighbour "has coefficients" bookkeeping (the initial context at
//! `first_coeff`, and the special rule that Y2's above/left predictors skip
//! past B_PRED/SPLITMV macroblocks that have no Y2 block) is [`crate::decode`]'s
//! job, since it spans macroblocks; this module decodes exactly one block
//! given the context value the caller already resolved.

use vaco_codec_msac::Vp8BoolDecoder as Bd;

use crate::tables::{
    CATEGORY_BASE, COEFF_BANDS, COEFF_TREE, PCAT1, PCAT2, PCAT3, PCAT4, PCAT5, PCAT6, ZIGZAG,
    token,
};

/// One decoded 4x4 block: raster-order coefficients and whether any were
/// non-zero (the flag the neighbour-context bookkeeping needs).
#[derive(Debug, Clone, Copy)]
pub struct BlockCoeffs {
    pub coeffs: [i32; 16],
    pub has_coeffs: bool,
    /// Highest scan position touched (0 if the block was entirely empty),
    /// so the caller can skip the inverse transform's DC-only fast path
    /// decision without rescanning.
    pub last_nonzero_scan: usize,
}

fn category_extra(bd: &mut Bd<'_>, cat_index: usize) -> i32 {
    let probs: &[u8] = match cat_index {
        0 => &PCAT1,
        1 => &PCAT2,
        2 => &PCAT3,
        3 => &PCAT4,
        4 => &PCAT5,
        _ => &PCAT6,
    };
    let mut extra = 0i32;
    for &p in probs {
        extra = (extra << 1) | i32::from(bd.read_bool(p));
    }
    CATEGORY_BASE.get(cat_index).copied().unwrap_or(0) + extra
}

/// Decode one 4x4 block. `probs` is `coeff_probs[plane]` (the `[8][3][11]`
/// slice for this block's plane type — Y-after-Y2, Y2, UV or Y-without-Y2,
/// RFC 6386 §13.3). `first_coeff` is 1 for a Y block that has a separate Y2
/// DC block, 0 otherwise. `initial_ctx` is 0..2, the count of the left/above
/// neighbour blocks (same plane) that had at least one non-zero coefficient.
#[must_use]
pub fn decode_block(
    bd: &mut Bd<'_>,
    probs: &[[[u8; 11]; 3]; 8],
    first_coeff: usize,
    initial_ctx: usize,
) -> BlockCoeffs {
    let mut coeffs = [0i32; 16];
    let mut has_coeffs = false;
    let mut last_nonzero_scan = 0usize;
    let mut ctx = initial_ctx.min(2);
    let mut prev_was_zero = false;
    let mut i = first_coeff;

    while i < 16 {
        let band = COEFF_BANDS.get(i).copied().unwrap_or(7);
        let row = probs
            .get(band)
            .and_then(|b| b.get(ctx))
            .copied()
            .unwrap_or([128; 11]);
        let start = if prev_was_zero { 2 } else { 0 };
        let tok = bd.read_tree_at(&COEFF_TREE, start, &row);

        if tok == token::DCT_EOB {
            break;
        }

        let abs_value = if tok <= token::DCT_4 {
            tok
        } else {
            let cat_index = (tok - token::DCT_CAT1) as usize;
            category_extra(bd, cat_index)
        };

        let value = if abs_value != 0 {
            has_coeffs = true;
            last_nonzero_scan = i;
            if bd.read_bool(128) {
                -abs_value
            } else {
                abs_value
            }
        } else {
            0
        };

        if let Some(slot) = ZIGZAG.get(i).and_then(|&raster| coeffs.get_mut(raster)) {
            *slot = value;
        }

        ctx = match abs_value {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        prev_was_zero = abs_value == 0;
        i += 1;
    }

    BlockCoeffs {
        coeffs,
        has_coeffs,
        last_nonzero_scan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{DEFAULT_COEFF_PROBS, PLANE_Y_NO_Y2};

    #[test]
    fn an_empty_partition_decodes_an_all_zero_block_via_overrun_zeros() {
        let mut bd = Bd::new(&[]);
        let probs = &DEFAULT_COEFF_PROBS[PLANE_Y_NO_Y2];
        let block = decode_block(&mut bd, probs, 0, 0);
        assert!(!block.has_coeffs);
        assert_eq!(block.coeffs, [0; 16]);
    }

    proptest::proptest! {
        #[test]
        fn decoding_never_panics_on_arbitrary_input(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64)) {
            let mut bd = Bd::new(&data);
            let probs = &DEFAULT_COEFF_PROBS[PLANE_Y_NO_Y2];
            let _ = decode_block(&mut bd, probs, 0, 0);
        }
    }
}
