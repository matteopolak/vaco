//! §6.4.24-26's coefficient token decode (`tokens`/`get_scan`'s `TxType`
//! selection lives in `crate::decode`; this module is `tokens`/`read_coef`
//! plus §9.3.2's `more_coefs`/`token` context derivation and the `pareto`
//! probability-expansion helper).

use vaco_codec_msac::Vp9BoolDecoder as Bd;

use crate::decode::FrameCtx;
use crate::header::EntropyContext;
use crate::tables;

/// §9.3.2's `pareto(node, prob)`.
#[allow(clippy::integer_division, reason = "spec-defined: x = (prob-1)/2, prob in 1..=255")]
fn pareto(node: usize, prob: u8) -> u8 {
    if node < 2 {
        return prob;
    }
    let x = (usize::from(prob).saturating_sub(1)) / 2;
    if prob & 1 != 0 {
        tables::PARETO_TABLE.get(x).and_then(|r| r.get(node - 2)).copied().unwrap_or(prob)
    } else {
        let a = tables::PARETO_TABLE.get(x).and_then(|r| r.get(node - 2)).copied().unwrap_or(prob);
        let b = tables::PARETO_TABLE.get(x + 1).and_then(|r| r.get(node - 2)).copied().unwrap_or(prob);
        u8::try_from((u16::from(a) + u16::from(b)) >> 1).unwrap_or(prob)
    }
}

/// The 3 stored probabilities for one `(txSz, plane>0, isInter=0, band,
/// ctx)` coefficient-probability row: `[more_coefs, node0, node1-and-beyond-seed]`.
fn coef_row(entropy: &EntropyContext, tx_sz: usize, plane0: usize, band: usize, ctx: usize) -> &[u8; 3] {
    entropy
        .coef_probs
        .get(tx_sz)
        .and_then(|a| a.get(plane0))
        .and_then(|a| a.first()) // is_inter always 0 for a key frame
        .and_then(|a| a.get(band))
        .and_then(|a| a.get(ctx))
        .unwrap_or(&[128, 128, 128])
}

/// §9.3.2's neighbour positions for the token-cache context, `nb[0]`/`nb[1]`.
#[allow(clippy::many_single_char_names, reason = "mirrors the spec's own i/j/pos/n/c names directly")]
fn neighbors(c: usize, pos: usize, n: usize, tx_type: vaco_codec_dsp_idct::vp9::TxType) -> (usize, usize) {
    use vaco_codec_dsp_idct::vp9::TxType::{AdstDct, DctAdst};
    if c == 0 {
        return (0, 0);
    }
    #[allow(clippy::integer_division, reason = "spec-defined: i = pos/n, j = pos%n, a raster decomposition")]
    let i = pos / n;
    let j = pos % n;
    if i > 0 && j > 0 {
        let a = (i - 1) * n + j;
        let a2 = i * n + j - 1;
        match tx_type {
            DctAdst => (a, a),
            AdstDct => (a2, a2),
            _ => (a, a2),
        }
    } else if i > 0 {
        let a = (i - 1) * n + j;
        (a, a)
    } else {
        let a = i * n + j - 1;
        (a, a)
    }
}

/// §6.4.24's `tokens()`. Returns `(Tokens, eobCount)` where `Tokens` is
/// `n0*n0` raster-order dequantizable coefficients and `eobCount` (`c` at
/// loop exit) is nonzero exactly when [`crate::decode`]'s `nonzero` is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_tokens(
    bd: &mut Bd<'_>,
    entropy: &EntropyContext,
    ctx: &FrameCtx,
    plane: usize,
    start_x: usize,
    start_y: usize,
    tx_sz: i32,
    scan: &[usize],
    tx_type: vaco_codec_dsp_idct::vp9::TxType,
    subsampling_x: bool,
    subsampling_y: bool,
    bit_depth: u32,
) -> (Vec<i32>, usize) {
    let tx_sz_u = usize::try_from(tx_sz).unwrap_or(0);
    let seg_eob = 16usize << (tx_sz_u << 1);
    let n = 4usize << tx_sz_u;
    let mut tokens = vec![0i32; seg_eob];
    let mut token_cache = vec![0usize; seg_eob];
    let plane0 = usize::from(plane > 0);

    let (sx, sy) = if plane > 0 { (subsampling_x, subsampling_y) } else { (false, false) };
    let max_x = (2 * ctx.mi_cols) >> u32::from(sx);
    let max_y = (2 * ctx.mi_rows) >> u32::from(sy);
    let x4 = start_x >> 2;
    let y4 = start_y >> 2;
    let numpts = 1usize << tx_sz_u;
    let mut above = false;
    let mut left = false;
    for i in 0..numpts {
        if x4 + i < max_x {
            above |= ctx.above_nz.get(plane).and_then(|r| r.get(x4 + i)).copied().unwrap_or(false);
        }
        if y4 + i < max_y {
            left |= ctx.left_nz.get(plane).and_then(|r| r.get((y4 + i) % 16)).copied().unwrap_or(false);
        }
    }
    let c0_ctx = usize::from(above) + usize::from(left);

    let mut check_eob = true;
    let mut c = 0usize;
    while c < seg_eob {
        let pos = scan.get(c).copied().unwrap_or(0);
        let band = if tx_sz == tables::TX_4X4 {
            tables::COEFBAND_4X4.get(c).copied().unwrap_or(5)
        } else {
            tables::COEFBAND_8X8PLUS.get(c).copied().unwrap_or(5)
        };
        let bctx = if c == 0 {
            c0_ctx
        } else {
            let (nb0, nb1) = neighbors(c, pos, n, tx_type);
            let a = token_cache.get(nb0).copied().unwrap_or(0);
            let b = token_cache.get(nb1).copied().unwrap_or(0);
            (1 + a + b) >> 1
        };
        let row = coef_row(entropy, tx_sz_u, plane0, band, bctx.min(5));

        if check_eob {
            let more_coefs = bd.read_bool(row.first().copied().unwrap_or(128));
            if !more_coefs {
                break;
            }
        }

        // Build the token tree's 10 per-node probabilities via `pareto`.
        let mut node_probs = [0u8; 10];
        for (node, slot) in node_probs.iter_mut().enumerate() {
            let idx = (1 + node).min(2);
            let base = row.get(idx).copied().unwrap_or(128);
            *slot = pareto(node, base);
        }
        let token = bd.read_tree(&tables::TOKEN_TREE, &node_probs);

        if let Some(slot) = token_cache.get_mut(pos) {
            *slot = tables::ENERGY_CLASS.get(usize::try_from(token).unwrap_or(0)).copied().unwrap_or(0);
        }

        if token == tables::token::ZERO_TOKEN {
            if let Some(slot) = tokens.get_mut(pos) {
                *slot = 0;
            }
            check_eob = false;
        } else {
            let coef = read_coef(bd, token, bit_depth);
            let sign_bit = bd.read_literal(1) != 0;
            if let Some(slot) = tokens.get_mut(pos) {
                *slot = if sign_bit { -coef } else { coef };
            }
            check_eob = true;
        }
        c += 1;
    }

    for i in c..seg_eob {
        if let Some(&pos) = scan.get(i)
            && let Some(slot) = tokens.get_mut(pos)
        {
            *slot = 0;
        }
    }
    (tokens, c)
}

/// §6.4.26's `read_coef`. `BitDepth > 8`'s high-bit extension for
/// `DCT_VAL_CATEGORY6` is implemented (profiles 2/3 are epic #32b's scope,
/// not tested here, but this keeps the syntax total rather than silently
/// wrong for a 10/12-bit key frame this crate otherwise decodes correctly).
fn read_coef(bd: &mut Bd<'_>, token: i32, bit_depth: u32) -> i32 {
    let idx = usize::try_from(token).unwrap_or(0);
    let &(cat, num_extra, base) = tables::EXTRA_BITS.get(idx).unwrap_or(&(0, 0, 0));
    let mut coef = base;
    if token == tables::token::DCT_VAL_CATEGORY6 {
        for e in 0..bit_depth.saturating_sub(8) {
            let high_bit = i32::from(bd.read_bool(255));
            coef += high_bit << (5 + bit_depth - e);
        }
    }
    let probs = tables::CAT_PROBS.get(cat).copied().unwrap_or(&[128]);
    for e in 0..num_extra {
        let p = probs.get(usize::try_from(e).unwrap_or(0)).copied().unwrap_or(128);
        let bit = i32::from(bd.read_bool(p));
        coef += bit << (num_extra - 1 - e);
    }
    coef
}
