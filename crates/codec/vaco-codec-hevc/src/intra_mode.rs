//! Intra prediction mode derivation: the most-probable-mode (MPM) list
//! (§8.4.2), the chroma derived-mode rule (Table 8-2/8-3), the mode-to-angle
//! table (Table 8-5) and the mode-to-scan-order rule (§6.5.1 / Table 6-x).
//!
//! Cross-checked against the HM reference decoder's
//! `TComDataCU::getIntraDirPredictor` (MPM) and `TDecSbac::parseIntraDirChroma`
//! / `TComDataCU::getAllowedChromaDir` (chroma), and `TComPrediction::xPredIntraAng`
//! (angle table) — BSD-3-Clause, Tier A, see `cabac_ctx`'s module doc.

/// `INTRA_PLANAR`, mode 0.
pub(crate) const PLANAR_IDX: u8 = 0;
/// `INTRA_DC`, mode 1.
pub(crate) const DC_IDX: u8 = 1;
/// `INTRA_ANGULAR26` (pure vertical), mode 26.
pub(crate) const VER_IDX: u8 = 26;
/// `INTRA_ANGULAR10` (pure horizontal), mode 10.
pub(crate) const HOR_IDX: u8 = 10;
/// `INTRA_DM_CHROMA`, the chroma "use the luma mode" sentinel — not a real
/// prediction mode, only ever seen as `intra_chroma_pred_mode`'s decoded
/// value before substitution.
pub(crate) const DM_CHROMA_IDX: u8 = 4;

/// Table 8-5's angle table, indexed by `abs(intraPredAngleMode)` (0..=8) —
/// `HM`'s `angTable`.
pub(crate) const ANG_TABLE: [i32; 9] = [0, 2, 5, 9, 13, 17, 21, 26, 32];
/// The matching inverse-angle table for negative angles' leftward
/// extension — `HM`'s `invAngTable`, `(256 * 32) / angle`.
pub(crate) const INV_ANG_TABLE: [i32; 9] = [0, 4096, 1638, 910, 630, 482, 390, 315, 256];

/// §8.4.2's most-probable-mode list, given the (already DC-substituted, per
/// the caller) left and above neighbours' luma modes.
///
/// `left`/`above` should already have had `DC_IDX` substituted for an
/// unavailable, non-intra, or (chroma-mode-storage-only) `DM_CHROMA_IDX`
/// neighbour — this function only implements the three-case combination
/// rule, not neighbour resolution.
#[must_use]
pub(crate) fn mpm_list(left: u8, above: u8) -> [u8; 3] {
    if left == above {
        if left > DC_IDX {
            [
                left,
                ((u32::from(left) + 29) % 32) as u8 + 2,
                ((u32::from(left) + 31) % 32) as u8 + 2,
            ]
        } else {
            [PLANAR_IDX, DC_IDX, VER_IDX]
        }
    } else {
        let third = if left != PLANAR_IDX && above != PLANAR_IDX {
            PLANAR_IDX
        } else if u32::from(left) + u32::from(above) < 2 {
            VER_IDX
        } else {
            DC_IDX
        };
        [left, above, third]
    }
}

/// Resolve `rem_intra_luma_pred_mode` (a plain 5-bit value, 0..=31) into the
/// final mode, given the *unsorted* MPM list — §8.4.2's "insert in ascending
/// order, then bump every mode at or past each MPM" derivation.
#[must_use]
pub(crate) fn resolve_rem_mode(rem: u8, mpm: [u8; 3]) -> u8 {
    let mut sorted = mpm;
    sorted.sort_unstable();
    let mut mode = rem;
    for m in sorted {
        if mode >= m {
            mode += 1;
        }
    }
    mode
}

/// Table 8-2/8-3: the chroma intra prediction mode, given the decoded
/// `intra_chroma_pred_mode` syntax value (0..=3 for an explicit candidate,
/// [`DM_CHROMA_IDX`] for "derived from luma") and the corresponding luma
/// PU's mode.
#[must_use]
pub(crate) fn chroma_mode(syntax_value: u8, luma_mode: u8) -> u8 {
    if syntax_value == DM_CHROMA_IDX {
        return luma_mode;
    }
    let candidates = [PLANAR_IDX, VER_IDX, HOR_IDX, DC_IDX];
    let picked = candidates
        .get(usize::from(syntax_value))
        .copied()
        .unwrap_or(DC_IDX);
    if picked == luma_mode { 34 } else { picked }
}

/// §7.4.9.11's mode-to-scan-order rule for one intra transform block's
/// residual coding. Mode-dependent scanning applies at `log2TrafoSize == 2`
/// for both components, and additionally at `log2TrafoSize == 3` for luma
/// only — an 8x8 *chroma* block (reached from a 16x16 luma CU at 4:2:0) is
/// always diagonal. Every other size is diagonal too.
#[must_use]
pub(crate) fn scan_order_for_mode(
    mode: u8,
    log2_size: u32,
    is_chroma: bool,
) -> crate::scan::ScanOrder {
    let mode_dependent = log2_size == 2 || (log2_size == 3 && !is_chroma);
    if !mode_dependent {
        return crate::scan::ScanOrder::Diag;
    }
    let mode = i32::from(mode);
    if (6..=14).contains(&mode) {
        crate::scan::ScanOrder::Vert
    } else if (22..=30).contains(&mode) {
        crate::scan::ScanOrder::Horiz
    } else {
        crate::scan::ScanOrder::Diag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_angular_neighbours_produce_the_documented_three() {
        let mpm = mpm_list(26, 26);
        // (26+29)%32+2 = 25, (26-1)%32+2 = 27 — HM's own `getIntraDirPredictor`.
        assert_eq!(mpm, [26, 25, 27]);
    }

    #[test]
    fn equal_non_angular_neighbours_are_planar_dc_ver() {
        assert_eq!(mpm_list(DC_IDX, DC_IDX), [PLANAR_IDX, DC_IDX, VER_IDX]);
        assert_eq!(
            mpm_list(PLANAR_IDX, PLANAR_IDX),
            [PLANAR_IDX, DC_IDX, VER_IDX]
        );
    }

    #[test]
    fn distinct_neighbours_neither_planar_get_planar_as_third() {
        assert_eq!(mpm_list(5, 9), [5, 9, PLANAR_IDX]);
    }

    #[test]
    fn distinct_neighbours_with_a_planar_use_ver_or_dc() {
        assert_eq!(mpm_list(PLANAR_IDX, DC_IDX), [PLANAR_IDX, DC_IDX, VER_IDX]);
        assert_eq!(mpm_list(PLANAR_IDX, 5), [PLANAR_IDX, 5, DC_IDX]);
    }

    #[test]
    fn resolve_rem_mode_skips_every_mpm_in_ascending_order() {
        // MPMs 1, 5, 26 (already distinct): rem 0 -> 0 (below all); rem 1 ->
        // bumped past 1 -> 2; rem 4 (would land on 5 after first bump) -> 6.
        let mpm = [1u8, 5, 26];
        assert_eq!(resolve_rem_mode(0, mpm), 0);
        assert_eq!(resolve_rem_mode(1, mpm), 2);
        assert_eq!(resolve_rem_mode(4, mpm), 6);
    }

    #[test]
    fn chroma_mode_substitutes_34_on_collision() {
        assert_eq!(chroma_mode(1, VER_IDX), 34); // candidate VER collides with luma VER
        assert_eq!(chroma_mode(1, DC_IDX), VER_IDX); // no collision
        assert_eq!(chroma_mode(DM_CHROMA_IDX, 17), 17);
    }
}
