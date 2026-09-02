//! DCT coefficient (residual token) decoding, RFC 6386 §13.
//!
//! Neighbour "has coefficients" bookkeeping (the initial context at
//! `first_coeff`, and the special rule that Y2's above/left predictors skip
//! past B_PRED/SPLITMV macroblocks that have no Y2 block) is [`crate::decode`]'s
//! job, since it spans macroblocks; this module decodes exactly one block
//! given the context value the caller already resolved.

use vaco_codec_msac::Vp8BoolDecoder as Bd;
use vaco_codec_msac::tree::write_tree_at;

use crate::encode::BoolWriter;
use crate::tables::{
    CATEGORY_BASE, COEFF_BANDS, COEFF_TREE, PCAT1, PCAT2, PCAT3, PCAT4, PCAT5, PCAT6, ZIGZAG, token,
};

/// One decoded 4x4 block: raster-order coefficients and whether any were
/// non-zero (the flag the neighbour-context bookkeeping needs).
#[derive(Debug, Clone, Copy, Default)]
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

fn token_for_abs(abs_value: i32) -> (i32, usize) {
    match abs_value {
        0 => (token::DCT_0, 0),
        1 => (token::DCT_1, 0),
        2 => (token::DCT_2, 0),
        3 => (token::DCT_3, 0),
        4 => (token::DCT_4, 0),
        v if v <= 6 => (token::DCT_CAT1, 0),
        v if v <= 10 => (token::DCT_CAT2, 1),
        v if v <= 18 => (token::DCT_CAT3, 2),
        v if v <= 34 => (token::DCT_CAT4, 3),
        v if v <= 66 => (token::DCT_CAT5, 4),
        _ => (token::DCT_CAT6, 5),
    }
}

/// The last zigzag scan index (exclusive) with a nonzero coefficient, or
/// `first_coeff` if the block is entirely empty from `first_coeff` on —
/// the position [`encode_block`] writes `DCT_EOB` at.
#[must_use]
fn find_eob(coeffs_raster: &[i32; 16], first_coeff: usize) -> usize {
    for i in (first_coeff..16).rev() {
        let raster = ZIGZAG.get(i).copied().unwrap_or(0);
        if coeffs_raster.get(raster).copied().unwrap_or(0) != 0 {
            return i + 1;
        }
    }
    first_coeff
}

/// The encode-side inverse of [`decode_block`]: write one 4x4 block's
/// already-quantised, raster-order coefficients as RFC 6386 §13 tokens.
/// Same parameter shape as [`decode_block`] (`probs`/`first_coeff`/
/// `initial_ctx`), so a caller's neighbour-context bookkeeping is identical
/// in both directions. Returns whether any coefficient was non-zero, for
/// that same bookkeeping.
#[must_use]
pub fn encode_block(
    bw: &mut BoolWriter,
    probs: &[[[u8; 11]; 3]; 8],
    coeffs_raster: &[i32; 16],
    first_coeff: usize,
    initial_ctx: usize,
) -> bool {
    let eob = find_eob(coeffs_raster, first_coeff);
    let mut ctx = initial_ctx.min(2);
    let mut prev_was_zero = false;
    let mut has_coeffs = false;
    let mut i = first_coeff;

    while i < 16 {
        let band = COEFF_BANDS.get(i).copied().unwrap_or(7);
        let row = probs
            .get(band)
            .and_then(|b| b.get(ctx))
            .copied()
            .unwrap_or([128; 11]);
        let start = if prev_was_zero { 2 } else { 0 };

        if i == eob {
            write_tree_at(&COEFF_TREE, start, token::DCT_EOB, |node, bit| {
                let p = row.get(node).copied().unwrap_or(128);
                bw.write_bool(p, bit);
            });
            return has_coeffs;
        }

        let raster = ZIGZAG.get(i).copied().unwrap_or(0);
        let val = coeffs_raster.get(raster).copied().unwrap_or(0);
        let abs_value = val.abs();
        let (tok, cat_idx) = token_for_abs(abs_value);

        write_tree_at(&COEFF_TREE, start, tok, |node, bit| {
            let p = row.get(node).copied().unwrap_or(128);
            bw.write_bool(p, bit);
        });

        if tok >= token::DCT_CAT1 {
            let cat_probs: &[u8] = match cat_idx {
                0 => &PCAT1,
                1 => &PCAT2,
                2 => &PCAT3,
                3 => &PCAT4,
                4 => &PCAT5,
                _ => &PCAT6,
            };
            let base = CATEGORY_BASE.get(cat_idx).copied().unwrap_or(0);
            let extra = (abs_value - base).max(0);
            for (bit_i, &p) in cat_probs.iter().enumerate() {
                let shift = cat_probs.len() - 1 - bit_i;
                let bit = (extra >> shift) & 1 != 0;
                bw.write_bool(p, bit);
            }
        }

        if abs_value != 0 {
            has_coeffs = true;
            bw.write_bool(128, val < 0);
        }

        ctx = match abs_value {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        prev_was_zero = abs_value == 0;
        i += 1;
    }

    has_coeffs
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

    #[test]
    fn encode_then_decode_round_trips_an_all_zero_block() {
        let probs = &DEFAULT_COEFF_PROBS[PLANE_Y_NO_Y2];
        let mut bw = BoolWriter::new();
        let has = encode_block(&mut bw, probs, &[0; 16], 0, 0);
        assert!(!has);
        let bytes = bw.finish();
        let mut bd = Bd::new(&bytes);
        let block = decode_block(&mut bd, probs, 0, 0);
        assert!(!block.has_coeffs);
        assert_eq!(block.coeffs, [0; 16]);
    }

    #[test]
    fn encode_then_decode_round_trips_a_mixed_block() {
        let probs = &DEFAULT_COEFF_PROBS[PLANE_Y_NO_Y2];
        // Raster order; zigzag scan visits these in a different order, and
        // some categories (cat1..cat6) are deliberately exercised.
        let coeffs: [i32; 16] = [3, 0, -1, 0, 7, -12, 0, 40, 0, 0, -100, 0, 0, 0, 0, 2];
        let mut bw = BoolWriter::new();
        let has = encode_block(&mut bw, probs, &coeffs, 0, 0);
        assert!(has);
        let bytes = bw.finish();
        let mut bd = Bd::new(&bytes);
        let block = decode_block(&mut bd, probs, 0, 0);
        assert_eq!(block.coeffs, coeffs);
        assert!(block.has_coeffs);
    }

    #[test]
    fn encode_then_decode_round_trips_with_first_coeff_one() {
        // has_y2 case: position 0 (raster DC) is never written or read.
        let probs = &DEFAULT_COEFF_PROBS[crate::tables::PLANE_Y_AFTER_Y2];
        let mut coeffs = [0i32; 16];
        if let Some(c) = coeffs.get_mut(1) {
            *c = 5;
        }
        if let Some(c) = coeffs.get_mut(4) {
            *c = -3;
        }
        let mut bw = BoolWriter::new();
        let has = encode_block(&mut bw, probs, &coeffs, 1, 0);
        assert!(has);
        let bytes = bw.finish();
        let mut bd = Bd::new(&bytes);
        let block = decode_block(&mut bd, probs, 1, 0);
        assert_eq!(block.coeffs, coeffs);
    }

    proptest::proptest! {
        #[test]
        fn encode_then_decode_round_trips_arbitrary_coefficients(
            raw in proptest::collection::vec(-200i32..=200, 16),
        ) {
            let probs = &DEFAULT_COEFF_PROBS[PLANE_Y_NO_Y2];
            let mut coeffs = [0i32; 16];
            coeffs.copy_from_slice(&raw);
            let mut bw = BoolWriter::new();
            let _ = encode_block(&mut bw, probs, &coeffs, 0, 0);
            let bytes = bw.finish();
            let mut bd = Bd::new(&bytes);
            let block = decode_block(&mut bd, probs, 0, 0);
            assert_eq!(block.coeffs, coeffs);
        }
    }
}
