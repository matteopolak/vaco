//! Per-tile CDF context: §8.2.2's "make a `Tile*Cdf` copy of every default
//! array" step, scoped to the syntax elements this crate's intra-only
//! decode path reads.
//!
//! # Why this is smaller than the specification's own list
//!
//! §8.2.2 names roughly ninety `Tile*Cdf` arrays; the vast majority belong
//! to inter prediction, palette mode and loop restoration, none of which
//! this crate decodes (see the crate root doc for the full list of what is
//! out of scope). [`TileCdf`] holds only the arrays an intra/intra-only
//! frame's `intra_frame_mode_info()`/`residual()` walk can actually reach.
//!
//! Every frame this crate decodes has `primary_ref_frame ==
//! PRIMARY_REF_NONE` (`frame_header`'s module doc explains why: it is
//! forced for every intra frame regardless of order), so [`TileCdf::new`]
//! is the *only* CDF initialization path this crate needs — `load_cdfs()`
//! from a saved frame context never applies.
//!
//! `Vaco-Spec-Ref: aom-av1-spec §8.2.2 (symbol decoder initialization), §9.4 (default CDF tables)`.

use crate::tables::default_cdf as d;

/// One CDF array plus its trailing adaptation counter, exactly the shape
/// §8.2.6's `read_symbol` expects: `N` real thresholds (the last fixed at
/// `1 << 15`) followed by the count. Kept as a thin wrapper only so
/// [`TileCdf`]'s fields read as named arrays rather than bare `Vec<u16>`s.
pub type Cdf<const N: usize> = [u16; N];

fn to_vec<const N: usize>(rows: &[[u16; N]]) -> Vec<Cdf<N>> {
    rows.to_vec()
}

/// Index a `COEFF_CDF_Q_CTXS`-sized (always 4, always non-empty) array by
/// `q` without ever indexing out of bounds — `q` comes from [`qctx`], which
/// only ever returns `0..=3`, so the fallback to `arr.first()` is dead code
/// on every real call, kept only so this function cannot panic if that
/// ever stops being true.
#[allow(
    clippy::unwrap_used,
    reason = "arr is a fixed non-empty array (COEFF_CDF_Q_CTXS = 4 at every call site), so `first()` is always `Some`"
)]
fn pick<T: Copy, const M: usize>(arr: [T; M], q: usize) -> T {
    arr.get(q).or_else(|| arr.first()).copied().unwrap()
}

/// The full per-tile CDF state, initialized fresh at the start of every
/// tile (`init_symbol`, §8.2.2) since this crate never loads a saved
/// context.
#[derive(Debug, Clone)]
pub struct TileCdf {
    pub intra_frame_y_mode: [[Cdf<14>; 5]; 5],
    pub uv_mode_cfl_not_allowed: [Cdf<14>; 13],
    pub uv_mode_cfl_allowed: [Cdf<15>; 13],
    pub angle_delta: [Cdf<8>; 8],
    pub partition_w8: [Cdf<5>; 4],
    pub partition_w16: [Cdf<11>; 4],
    pub partition_w32: [Cdf<11>; 4],
    pub partition_w64: [Cdf<11>; 4],
    pub partition_w128: [Cdf<9>; 4],
    pub tx_8x8: [Cdf<3>; 3],
    pub tx_16x16: [Cdf<4>; 3],
    pub tx_32x32: [Cdf<4>; 3],
    pub tx_64x64: [Cdf<4>; 3],
    pub skip: [Cdf<3>; 3],
    pub segment_id: [Cdf<9>; 3],
    pub delta_q: Cdf<5>,
    pub delta_lf: Cdf<5>,
    pub delta_lf_multi: [Cdf<5>; 4],
    pub intra_tx_type_set1: [[Cdf<8>; 13]; 2],
    pub intra_tx_type_set2: [[Cdf<6>; 13]; 3],
    pub cfl_sign: Cdf<9>,
    pub cfl_alpha: [Cdf<17>; 6],
    pub intrabc: Cdf<3>,
    pub filter_intra_mode: Cdf<6>,
    pub filter_intra: Vec<Cdf<3>>,
    /// `PaletteYModeCdf[bsizeCtx][ctx]`, §8.3.2 — this crate never applies
    /// a palette prediction, but a real encoder can set
    /// `allow_screen_content_tools` regardless of whether any given block
    /// actually uses one, and `has_palette_y`/`has_palette_uv` are read
    /// unconditionally whenever the syntax makes them present. `ctx` is
    /// always `0` here (a block ever setting `has_palette_y` returns
    /// `Error::Unsupported` before any `PaletteSizes` entry could become
    /// nonzero, so no neighbour ever contributes a nonzero context).
    pub palette_y_mode: [[Cdf<3>; 3]; 7],
    pub palette_uv_mode: [Cdf<3>; 2],
    // Coefficient decoding, §9.4's `idx`-selected (base_q_idx bucketed)
    // tables — see `qctx`.
    pub txb_skip: Vec<[Cdf<3>; 13]>,
    pub eob_pt_16: [[Cdf<6>; 2]; 2],
    pub eob_pt_32: [[Cdf<7>; 2]; 2],
    pub eob_pt_64: [[Cdf<8>; 2]; 2],
    pub eob_pt_128: [[Cdf<9>; 2]; 2],
    pub eob_pt_256: [[Cdf<10>; 2]; 2],
    pub eob_pt_512: [Cdf<11>; 2],
    pub eob_pt_1024: [Cdf<12>; 2],
    pub eob_extra: Vec<[[Cdf<3>; 9]; 2]>,
    pub dc_sign: [[Cdf<3>; 3]; 2],
    pub coeff_base_eob: Vec<[[Cdf<4>; 4]; 2]>,
    pub coeff_base: Vec<[[Cdf<5>; 42]; 2]>,
    pub coeff_br: Vec<[[Cdf<5>; 21]; 2]>,
}

/// `idx` in §8.2.2's `init_coeff_cdfs()`: which of the four quantizer-
/// bucketed default coefficient CDF sets a frame's `base_q_idx` selects.
#[must_use]
pub const fn qctx(base_q_idx: u8) -> usize {
    if base_q_idx <= 20 {
        0
    } else if base_q_idx <= 60 {
        1
    } else if base_q_idx <= 120 {
        2
    } else {
        3
    }
}

impl TileCdf {
    /// `init_non_coeff_cdfs()` + `init_coeff_cdfs()`, §8.2.2, for this
    /// crate's own reduced syntax-element set.
    #[must_use]
    pub fn new(base_q_idx: u8) -> Self {
        let q = qctx(base_q_idx);
        Self {
            intra_frame_y_mode: d::DEFAULT_INTRA_FRAME_Y_MODE_CDF,
            uv_mode_cfl_not_allowed: d::DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF,
            uv_mode_cfl_allowed: d::DEFAULT_UV_MODE_CFL_ALLOWED_CDF,
            angle_delta: d::DEFAULT_ANGLE_DELTA_CDF,
            partition_w8: d::DEFAULT_PARTITION_W8_CDF,
            partition_w16: d::DEFAULT_PARTITION_W16_CDF,
            partition_w32: d::DEFAULT_PARTITION_W32_CDF,
            partition_w64: d::DEFAULT_PARTITION_W64_CDF,
            partition_w128: d::DEFAULT_PARTITION_W128_CDF,
            tx_8x8: d::DEFAULT_TX_8X8_CDF,
            tx_16x16: d::DEFAULT_TX_16X16_CDF,
            tx_32x32: d::DEFAULT_TX_32X32_CDF,
            tx_64x64: d::DEFAULT_TX_64X64_CDF,
            skip: d::DEFAULT_SKIP_CDF,
            segment_id: d::DEFAULT_SEGMENT_ID_CDF,
            delta_q: d::DEFAULT_DELTA_Q_CDF,
            delta_lf: d::DEFAULT_DELTA_LF_CDF,
            delta_lf_multi: [d::DEFAULT_DELTA_LF_CDF; 4],
            intra_tx_type_set1: d::DEFAULT_INTRA_TX_TYPE_SET1_CDF,
            intra_tx_type_set2: d::DEFAULT_INTRA_TX_TYPE_SET2_CDF,
            cfl_sign: d::DEFAULT_CFL_SIGN_CDF,
            cfl_alpha: d::DEFAULT_CFL_ALPHA_CDF,
            intrabc: d::DEFAULT_INTRABC_CDF,
            filter_intra_mode: d::DEFAULT_FILTER_INTRA_MODE_CDF,
            filter_intra: to_vec(&d::DEFAULT_FILTER_INTRA_CDF),
            palette_y_mode: d::DEFAULT_PALETTE_Y_MODE_CDF,
            palette_uv_mode: d::DEFAULT_PALETTE_UV_MODE_CDF,
            txb_skip: pick(d::DEFAULT_TXB_SKIP_CDF, q).to_vec(),
            eob_pt_16: pick(d::DEFAULT_EOB_PT_16_CDF, q),
            eob_pt_32: pick(d::DEFAULT_EOB_PT_32_CDF, q),
            eob_pt_64: pick(d::DEFAULT_EOB_PT_64_CDF, q),
            eob_pt_128: pick(d::DEFAULT_EOB_PT_128_CDF, q),
            eob_pt_256: pick(d::DEFAULT_EOB_PT_256_CDF, q),
            eob_pt_512: pick(d::DEFAULT_EOB_PT_512_CDF, q),
            eob_pt_1024: pick(d::DEFAULT_EOB_PT_1024_CDF, q),
            eob_extra: pick(d::DEFAULT_EOB_EXTRA_CDF, q).to_vec(),
            dc_sign: pick(d::DEFAULT_DC_SIGN_CDF, q),
            coeff_base_eob: pick(d::DEFAULT_COEFF_BASE_EOB_CDF, q).to_vec(),
            coeff_base: pick(d::DEFAULT_COEFF_BASE_CDF, q).to_vec(),
            coeff_br: pick(d::DEFAULT_COEFF_BR_CDF, q).to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qctx_matches_the_four_documented_bands() {
        assert_eq!(qctx(0), 0);
        assert_eq!(qctx(20), 0);
        assert_eq!(qctx(21), 1);
        assert_eq!(qctx(60), 1);
        assert_eq!(qctx(61), 2);
        assert_eq!(qctx(120), 2);
        assert_eq!(qctx(121), 3);
        assert_eq!(qctx(255), 3);
    }

    #[test]
    fn every_qctx_bucket_constructs_without_panicking() {
        for q in [0u8, 20, 21, 60, 61, 120, 121, 255] {
            let _ = TileCdf::new(q);
        }
    }
}
