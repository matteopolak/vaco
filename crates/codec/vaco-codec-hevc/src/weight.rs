//! Explicit weighted sample prediction, ITU-T H.265 §8.5.3.3.4.3 — resolving
//! a slice's own `pred_weight_table()` (`vaco_parse_hevc::slice::
//! PredWeightTable`, already parsed by `vaco-parse-hevc` — see
//! `decoder.rs`'s own module doc for why nothing here duplicates that parse)
//! into a flat per-`ref_idx` table of `LumaWeightL0`/`ChromaWeightL0` and
//! `luma_offset_l0`/`ChromaOffsetL0`, resolved once per slice rather than
//! re-derived per pixel.
//!
//! # Scope
//!
//! `RefPicList0` (uni-predictive, P-slice weighting) only. Bi-predictive
//! weighting is §8.5.3.3.4.3's other half, unreachable from this crate's own
//! B-slice refusal (`decoder.rs::decode_packet`). §7.4.7.3's
//! `high_precision_offsets_enabled_flag` widening is unreachable too — it
//! lives entirely in the PPS/SPS range extension, which
//! `decoder::check_scope` already refuses outright — so `WpOffsetBdShiftY`/
//! `WpOffsetBdShiftC` are always `0` and `WpOffsetHalfRangeC` is always `128`
//! here (`BitDepth == 8` throughout this crate's scope, also
//! `check_scope`-enforced).
//!
//! # Specification
//!
//! ITU-T H.265 (08/2021) §8.5.3.3.4.3, cross-checked against HM 18.0's
//! `TComWeightPrediction::getWpScaling` (Tier A, BSD-3-Clause).

use vaco_parse_hevc::slice::PredWeightTable;

use crate::mc::Weight;

/// `shift1 = Max(2, 14 - BitDepth)` — the specification's own clause-local
/// name, reused here because `log2Wd` is defined in terms of it. Always `6`
/// in this crate's 8-bit-only scope; computed from `bit_depth` anyway so the
/// code reads the same as the clause it implements rather than hard-coding
/// the one value it is ever called with.
fn shift1(bit_depth: u32) -> i32 {
    (14 - i32::try_from(bit_depth).unwrap_or(8)).max(2)
}

/// One `RefPicList0` entry's own resolved luma and chroma (`[Cb, Cr]`)
/// weights.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RefWeights {
    pub luma: Weight,
    pub chroma: [Weight; 2],
}

/// Resolves every `RefPicList0` entry's own weight/offset from a slice's
/// `pred_weight_table()`, for [`crate::ctu::build_cu_prediction`] to index
/// by `ref_idx` directly.
///
/// `num_refs` is `RefPicList0.len()`; an index the table has no entry for
/// (fewer parsed entries than active references — only possible from a
/// malformed header, since a conformant one names exactly
/// `num_ref_idx_l0_active_minus1 + 1`) falls back to the neutral,
/// unweighted values, the same values §7.3.6.3 itself assigns whenever
/// `luma_weight_l0_flag[i]`/the chroma equivalent is `0`.
pub(crate) fn resolve_l0(table: &PredWeightTable, num_refs: usize, bit_depth_luma: u32, bit_depth_chroma: u32) -> Vec<RefWeights> {
    let s1_luma = shift1(bit_depth_luma);
    let s1_chroma = shift1(bit_depth_chroma);

    let luma_log2_denom = i32::try_from(table.luma_log2_weight_denom).unwrap_or(0);
    let luma_log2_wd = luma_log2_denom + s1_luma;
    let luma_denom_pow = 1i32 << luma_log2_denom.clamp(0, 30);

    // §7.4.7.3: ChromaLog2WeightDenom = luma_log2_weight_denom +
    // delta_chroma_log2_weight_denom. Clamped to a sane non-negative range
    // before use as a shift amount: a conformant stream never drives this
    // negative, but nothing upstream of this function refuses one that does,
    // and a negative shift amount panics rather than misdecoding quietly —
    // clamping here is a fuzz-safety floor, not a new approximation of
    // conformant behaviour.
    let chroma_log2_denom = (luma_log2_denom + table.delta_chroma_log2_weight_denom).clamp(0, 30);
    let chroma_log2_wd = chroma_log2_denom + s1_chroma;
    let chroma_denom_pow = 1i32 << chroma_log2_denom;
    let half_range_c = 1i32 << bit_depth_chroma.saturating_sub(1).min(30);

    (0..num_refs)
        .map(|i| {
            let (dw, o) = table.luma[0].get(i).copied().flatten().unwrap_or((0, 0));
            let luma = Weight { log2_wd: luma_log2_wd, w: luma_denom_pow.saturating_add(dw), o };

            let chroma_entry = table.chroma[0].get(i).copied().flatten();
            let chroma = [0usize, 1usize].map(|c| {
                let (dwc, doc) = chroma_entry.and_then(|pair| pair.get(c).copied()).unwrap_or((0, 0));
                let w = chroma_denom_pow.saturating_add(dwc);
                // §8.5.3.3.4.3's `ChromaOffsetL0[i][j]`:
                //   Clip3(-halfRange, halfRange - 1,
                //         (halfRange + delta) - ((halfRange * w) >> ChromaLog2WeightDenom))
                let predicted = half_range_c.saturating_mul(w) >> chroma_log2_denom;
                let o = (half_range_c.saturating_add(doc) - predicted).clamp(-half_range_c, half_range_c - 1);
                Weight { log2_wd: chroma_log2_wd, w, o }
            });
            RefWeights { luma, chroma }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code over fixed scenarios")]
mod tests {
    use super::*;

    fn table_with(luma_denom: u32, entries: Vec<Option<(i32, i32)>>) -> PredWeightTable {
        PredWeightTable {
            luma_log2_weight_denom: luma_denom,
            delta_chroma_log2_weight_denom: 0,
            luma: [entries, Vec::new()],
            chroma: [Vec::new(), Vec::new()],
        }
    }

    #[test]
    fn an_absent_flag_resolves_to_the_neutral_weight() {
        // luma_weight_l0_flag[0] == 0: LumaWeightL0[0] == 1 << denom, offset == 0.
        let table = table_with(3, vec![None]);
        let resolved = resolve_l0(&table, 1, 8, 8);
        assert_eq!(resolved[0].luma.w, 1 << 3);
        assert_eq!(resolved[0].luma.o, 0);
        assert_eq!(resolved[0].luma.log2_wd, 3 + 6);
    }

    #[test]
    fn a_neutral_weight_collapses_to_the_default_shift_and_offset() {
        // With w == 1 << denom and o == 0, apply_weight must match
        // predict_block's own folded (pred + 32) >> 6 default arithmetic,
        // at every denom this crate can parse (0..=7).
        for denom in 0..=7u32 {
            let table = table_with(denom, vec![None]);
            let resolved = resolve_l0(&table, 1, 8, 8);
            for pred in [-500, -1, 0, 1, 4032, 4095, 30000] {
                let got = crate::mc::apply_weight(pred, resolved[0].luma, 8);
                let want = ((pred + 32) >> 6).clamp(0, 255);
                assert_eq!(got, want, "denom={denom} pred={pred}");
            }
        }
    }

    #[test]
    fn a_real_weight_matches_a_hand_derivation() {
        // luma_log2_weight_denom = 4, delta_luma_weight_l0[0] = -1 (w = 15),
        // luma_offset_l0[0] = -3 — the exact values named in
        // AGENT-CONSTRAINTS.md's H.264 sibling-bug warning, carried over as
        // a concrete non-neutral fixture for this crate's own table.
        let table = table_with(4, vec![Some((-1, -3))]);
        let resolved = resolve_l0(&table, 1, 8, 8);
        let w = resolved[0].luma;
        assert_eq!(w.w, 15);
        assert_eq!(w.o, -3);
        assert_eq!(w.log2_wd, 4 + 6);
        // predSampleLX = 200: (200*15 + 2^9) >> 10 - 3 = (3000+512)>>10 - 3 = 3 - 3 = 0.
        assert_eq!(crate::mc::apply_weight(200, w, 8), 0);
    }

    #[test]
    fn out_of_range_indices_fall_back_to_neutral() {
        let table = table_with(2, vec![Some((5, 10))]);
        // num_refs = 2 but the table only names one entry.
        let resolved = resolve_l0(&table, 2, 8, 8);
        assert_eq!(resolved[1].luma.w, 1 << 2);
        assert_eq!(resolved[1].luma.o, 0);
    }
}
