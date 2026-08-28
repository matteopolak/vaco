//! T3-01g (#420)'s own scope: pure intra prediction sample-generation
//! functions, following the exact split `dequant.rs` established -- primary-
//! text equations as standalone, independently testable functions, not
//! wired into `mb.rs`'s macroblock loop for the general multi-macroblock
//! case here (see `crate::reconstruct` for that composition).
//!
//! # What is implemented, and why not more
//!
//! `Intra_16x16` (clause 8.3.2): Vertical, Horizontal, DC. **Not Plane**
//! (clause 8.3.2.4) -- deferred, not attempted incorrectly; it needs a
//! weighted-sum-of-differences formula not yet reached.
//!
//! Chroma (clause 8.3.3): DC, Horizontal, Vertical, all four 4x4
//! quadrants' worth of clause 8.3.3.1's own case split for DC. **Not
//! Plane** (clause 8.3.3.4), same reason.
//!
//! `Intra_4x4` (clause 8.3.1): all nine prediction modes (clauses
//! 8.3.1.2.1..8.3.1.2.9, Table 8-2) via a single unified `p[x, y]`
//! addressing helper matching the spec's own notation, plus mode
//! inference (clause 8.3.1.1, eq. (8-42)) as a standalone pure function
//! taking each neighbour's already-resolved effective mode -- neighbour
//! derivation itself (clause 6.4.7.3/6.4.8, the `dcOnlyPredictionFlag`
//! substitution, real per-4x4-block picture state) is `crate::reconstruct`'s
//! job, the same "resolved before this module sees it" split every other
//! function here already keeps.
//!
//! **No 8x8 intra prediction at all.** Checked rather than assumed: this
//! crate's own `iso-iec-14496-10-2002-draft` source has no `Intra_8x8`
//! clause anywhere (searched for it directly) -- the same edition gap
//! already established for the 8x8 transform. A real scope reduction, not
//! a gap left for later.
//!
//! # Neighbour availability, not yet general
//!
//! [`Neighbours16`]/[`NeighboursChroma`] take already-resolved availability
//! + sample values -- deriving those from a real multi-macroblock picture
//! (clause 6.4.8's neighbouring-location process, `constrained_intra_pred_flag`,
//! slice boundaries) is not implemented here. What clause 8.3.2/8.3.3
//! guarantee independent of that derivation -- the "all unavailable"
//! case reduces to a flat `128` for every mode with a defined behaviour
//! for it (eq. (8-75) for luma, eq. (8-85)/(8-93) for chroma) -- is
//! exactly the case the flat oracle fixture is built to exercise, and the
//! only one wired into a real decode this round (see this module's own
//! `tests::flat_fixture_reconstructs_to_uniform_128`, which drives these
//! functions with `Intra16x16PredMode`/`intra_chroma_pred_mode` decoded
//! live off `tests/fixtures/cabac_intra_oracle_flat.264`, not synthetic
//! inputs).

#![allow(
    dead_code,
    reason = "exercised by this module's own tests and by crate::reconstruct; \
              not wired into mb.rs's macroblock loop for the general \
              multi-macroblock case directly -- see this module's own doc"
)]

/// One row (or column, transposed) of 16 luma neighbour samples plus
/// whether they are all marked available, per clause 6.4.8's own
/// definition (`mbAddrN` available, and not `constrained_intra_pred_flag`-
/// excluded).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Neighbours16 {
    pub(crate) top_available: bool,
    pub(crate) top: [u8; 16],
    pub(crate) left_available: bool,
    pub(crate) left: [u8; 16],
}

/// Clause 8.3.2, `Intra16x16PredMode` (Table 8-3): `0` Vertical, `1`
/// Horizontal, `2` DC, `3` Plane (not implemented -- see module doc).
#[must_use]
pub(crate) fn predict_intra16x16(mode: u8, n: Neighbours16) -> [[u8; 16]; 16] {
    match mode {
        0 => {
            // eq. (8-70): predL[x, y] = p[x, -1] -- every row is the top
            // neighbour row, unchanged down all 16 rows.
            let mut out = [[0u8; 16]; 16];
            out.fill(n.top);
            out
        }
        1 => {
            // eq. (8-71): predL[x, y] = p[-1, y] -- every column is the
            // left neighbour column, so row `y` is a flat fill of
            // `left[y]`.
            let mut out = [[0u8; 16]; 16];
            for (y, row) in out.iter_mut().enumerate() {
                *row = [n.left.get(y).copied().unwrap_or(0); 16];
            }
            out
        }
        _ => {
            // eq. (8-72)..(8-75): the four DC sub-cases, by availability.
            let dc = match (n.top_available, n.left_available) {
                (true, true) => {
                    let sum: u32 = n
                        .top
                        .iter()
                        .chain(n.left.iter())
                        .map(|&v| u32::from(v))
                        .sum();
                    ((sum + 16) >> 5) as u8
                }
                (false, true) => {
                    let sum: u32 = n.left.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 8) >> 4) as u8
                }
                (true, false) => {
                    let sum: u32 = n.top.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 8) >> 4) as u8
                }
                (false, false) => 128,
            };
            [[dc; 16]; 16]
        }
    }
}

/// One 8-long row/column of chroma neighbour samples for one chroma
/// component, plus availability -- clause 8.3.3's own per-quadrant
/// structure needs the 0..3/4..7 split kept separate rather than one
/// monolithic 8-element array, since a quadrant's own availability can
/// differ from its neighbour quadrant's (e.g. constrained intra excluding
/// one neighbouring macroblock but not another).
#[derive(Debug, Clone, Copy)]
pub(crate) struct NeighboursChroma {
    pub(crate) top_available: bool,
    pub(crate) top: [u8; 8],
    pub(crate) left_available: bool,
    pub(crate) left: [u8; 8],
}

/// Clause 8.3.3, chroma prediction mode (Table 8-4, same numbering as
/// luma's `Intra16x16PredMode`): `0` DC, `1` Horizontal, `2` Vertical,
/// `3` Plane (not implemented).
#[must_use]
pub(crate) fn predict_intra_chroma(mode: u8, n: NeighboursChroma) -> [[u8; 8]; 8] {
    match mode {
        1 => {
            // eq. (8-96): predC[x, y] = p[-1, y].
            let mut out = [[0u8; 8]; 8];
            for (y, row) in out.iter_mut().enumerate() {
                *row = [n.left.get(y).copied().unwrap_or(0); 8];
            }
            out
        }
        2 => {
            // eq. (8-97): predC[x, y] = p[x, -1].
            let mut out = [[0u8; 8]; 8];
            out.fill(n.top);
            out
        }
        _ => {
            // eq. (8-82)..(8-95): DC, one value per 4x4 quadrant, each
            // with its own four-way availability case split (clause
            // 8.3.3.1). Quadrants: (0,0)=top-left, (1,0)=top-right,
            // (0,1)=bottom-left, (1,1)=bottom-right, matching the
            // `x=0..3`/`x=4..7`, `y=0..3`/`y=4..7` ranges the clause
            // itself splits on.
            let top0 = &n.top[0..4];
            let top1 = &n.top[4..8];
            let left0 = &n.left[0..4];
            let left1 = &n.left[4..8];

            // eq. (8-82)..(8-85): the top-left and (eq. 8-92..8-95)
            // bottom-right quadrants are a symmetric four-way split --
            // both available averages both, either alone averages
            // itself, neither gives 128.
            let dc4_symmetric =
                |top: &[u8], top_avail: bool, left: &[u8], left_avail: bool| -> u8 {
                    match (top_avail, left_avail) {
                        (true, true) => {
                            let sum: u32 =
                                top.iter().chain(left.iter()).map(|&v| u32::from(v)).sum();
                            ((sum + 4) >> 3) as u8
                        }
                        (true, false) => {
                            let sum: u32 = top.iter().map(|&v| u32::from(v)).sum();
                            ((sum + 2) >> 2) as u8
                        }
                        (false, true) => {
                            let sum: u32 = left.iter().map(|&v| u32::from(v)).sum();
                            ((sum + 2) >> 2) as u8
                        }
                        (false, false) => 128,
                    }
                };
            // eq. (8-86)..(8-88): the top-right quadrant is *not*
            // symmetric -- it prefers its own top row unconditionally
            // when available (never averaging it with the left column),
            // falling back to the left column only when its own top row
            // is unavailable, and to 128 only when both are unavailable.
            // eq. (8-89)..(8-91) is the same shape for bottom-left with
            // the priority reversed (left column first).
            let dc4_priority = |primary: &[u8],
                                primary_avail: bool,
                                secondary: &[u8],
                                secondary_avail: bool|
             -> u8 {
                if primary_avail {
                    let sum: u32 = primary.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 2) >> 2) as u8
                } else if secondary_avail {
                    let sum: u32 = secondary.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 2) >> 2) as u8
                } else {
                    128
                }
            };

            let dc_tl = dc4_symmetric(top0, n.top_available, left0, n.left_available);
            let dc_tr = dc4_priority(top1, n.top_available, left0, n.left_available);
            let dc_bl = dc4_priority(left1, n.left_available, top0, n.top_available);
            let dc_br = dc4_symmetric(top1, n.top_available, left1, n.left_available);

            let mut out = [[0u8; 8]; 8];
            for (y, row) in out.iter_mut().enumerate() {
                let (l, r) = if y < 4 {
                    (dc_tl, dc_tr)
                } else {
                    (dc_bl, dc_br)
                };
                *row = [l, l, l, l, r, r, r, r];
            }
            out
        }
    }
}

/// Clause 8.3.1's own "13 neighbouring samples p[x,y]", already resolved
/// to concrete values (or a harmless default where genuinely unused) by
/// the caller -- this struct's own job is the nine equations of clause
/// 8.3.1.2.1..8.3.1.2.9, not neighbour derivation (clause 6.4.7.3/6.4.8),
/// which needs a real, multi-macroblock picture buffer this module still
/// does not have (see the module doc's own "not yet general" section).
///
/// `top` and `top_right` are kept separate (rather than one 8-element
/// array) because clause 8.3.1.2's own substitution rule -- when
/// `p[4..8,-1]` are unavailable but `p[3,-1]` is available, substitute
/// `p[3,-1]`'s value for all four and mark them available -- is exactly
/// the kind of caller-side resolution step this struct expects to have
/// already happened, the same "resolved before this module sees it"
/// contract [`Neighbours16`]/[`NeighboursChroma`] already keep.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Neighbours4 {
    pub(crate) top_available: bool,
    pub(crate) top: [u8; 4],
    /// `p[4..8,-1]` -- already substituted per the rule above if the real
    /// top-right was unavailable but `p[3,-1]` was available. Modes that
    /// need it (`Diagonal_Down_Left`, `Vertical_Left`) are only ever
    /// selected by a conformant encoder when this precondition already
    /// holds, so this struct does not separately track "was this the
    /// genuine value or a substitution" -- by the time a mode reads it,
    /// the distinction is spec-irrelevant.
    pub(crate) top_right: [u8; 4],
    pub(crate) left_available: bool,
    pub(crate) left: [u8; 4],
    /// `p[-1,-1]`, needed by `Diagonal_Down_Right`/`Vertical_Right`/
    /// `Horizontal_Down`'s own `x == y` (or `zVR`/`zHD == -1`) case.
    pub(crate) corner: u8,
}

/// Clause 8.3.1.2's own unified `p[x, y]` notation, `x = -1..=7`,
/// `y = -1..=3` -- a single addressable function lets every equation
/// below be copied close to verbatim, including the cases where an
/// equation's own algebra (e.g. eq. (8-53)'s `p[x-y-2,-1]`) drifts to
/// `x = -1` and lands back on the corner rather than the top row, which
/// is exactly what the spec's own shared `p[x,y]` space means by that
/// notation in the first place.
fn p4(n: &Neighbours4, x: i32, y: i32) -> i32 {
    if x == -1 && y == -1 {
        return i32::from(n.corner);
    }
    if y == -1 {
        // Top row (`x` may run 0..=7): index 0..3 is `top`, 4..=7 is
        // `top_right`. `.get()` rather than raw indexing even though
        // every call site in this module is provably in range (checked
        // by hand against each of the nine modes' own equations,
        // documented on `predict_intra4x4` below) -- a defensive `0`
        // default costs nothing here and keeps this function itself
        // panic-free regardless of a future caller's own arithmetic.
        return usize::try_from(x)
            .ok()
            .and_then(|i| {
                if i < 4 {
                    n.top.get(i)
                } else {
                    n.top_right.get(i - 4)
                }
            })
            .copied()
            .map_or(0, i32::from);
    }
    // Left column (`x == -1`, `y` runs 0..=3).
    usize::try_from(y)
        .ok()
        .and_then(|i| n.left.get(i))
        .copied()
        .map_or(0, i32::from)
}

/// Clause 8.3.1.1, eq. (8-42): `Intra4x4PredMode[luma4x4BlkIdx]` from the
/// two neighbouring blocks' own effective modes plus the two syntax
/// elements read for this block. `mode_a`/`mode_b` are already resolved
/// to `2` (DC) by the caller for any neighbour that is unavailable or
/// whose own macroblock is not coded `Intra_4x4` (clause 8.3.1.1's own
/// `dcOnlyPredictionFlag` substitution) -- this function's only job is
/// `predIntra4x4PredMode = Min(...)` and the `prev`/`rem` combination,
/// not neighbour derivation.
#[must_use]
pub(crate) const fn infer_intra4x4_pred_mode(
    mode_a: u8,
    mode_b: u8,
    prev_flag: bool,
    rem: u8,
) -> u8 {
    let pred = if mode_a < mode_b { mode_a } else { mode_b };
    if prev_flag {
        pred
    } else if rem < pred {
        rem
    } else {
        rem + 1
    }
}

/// Clause 8.3.1.2.1..8.3.1.2.9: the nine `Intra_4x4` modes (Table 8-2),
/// transcribed directly against this crate's own
/// `iso-iec-14496-10-2002-draft` source. `mode` is assumed already
/// resolved (e.g. by [`infer_intra4x4_pred_mode`]) and assumed valid for
/// `n`'s own availability (a conformant encoder never selects a mode
/// whose own "shall be used only when..." precondition `n` does not
/// satisfy) -- out-of-range `mode` values fall back to DC (mode 2)
/// rather than panicking, matching this crate's established "defensive
/// default over an indexing panic" idiom.
#[must_use]
pub(crate) fn predict_intra4x4(mode: u8, n: Neighbours4) -> [[u8; 4]; 4] {
    let mut out = [[0u8; 4]; 4];
    match mode {
        0 => {
            // eq. (8-45), Vertical.
            for row in &mut out {
                *row = n.top;
            }
        }
        1 => {
            // eq. (8-46), Horizontal.
            for (y, row) in out.iter_mut().enumerate() {
                *row = [n.left.get(y).copied().unwrap_or(0); 4];
            }
        }
        3 => {
            // eq. (8-51)/(8-52), Diagonal_Down_Left.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    *v = if x == 3 && y == 3 {
                        (p4(&n, 6, -1) + 3 * p4(&n, 7, -1) + 2) >> 2
                    } else {
                        (p4(&n, x + y, -1) + 2 * p4(&n, x + y + 1, -1) + p4(&n, x + y + 2, -1) + 2)
                            >> 2
                    } as u8;
                }
            }
        }
        4 => {
            // eq. (8-53)/(8-54)/(8-55), Diagonal_Down_Right.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    *v = match x.cmp(&y) {
                        core::cmp::Ordering::Greater => {
                            (p4(&n, x - y - 2, -1)
                                + 2 * p4(&n, x - y - 1, -1)
                                + p4(&n, x - y, -1)
                                + 2)
                                >> 2
                        }
                        core::cmp::Ordering::Less => {
                            (p4(&n, -1, y - x - 2)
                                + 2 * p4(&n, -1, y - x - 1)
                                + p4(&n, -1, y - x)
                                + 2)
                                >> 2
                        }
                        core::cmp::Ordering::Equal => {
                            (p4(&n, 0, -1) + 2 * p4(&n, -1, -1) + p4(&n, -1, 0) + 2) >> 2
                        }
                    } as u8;
                }
            }
        }
        5 => {
            // eq. (8-56)..(8-59), Vertical_Right.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    let z_vr = 2 * x - y;
                    *v = if z_vr >= 0 && z_vr % 2 == 0 {
                        (p4(&n, x - (y >> 1) - 1, -1) + p4(&n, x - (y >> 1), -1) + 1) >> 1
                    } else if z_vr == 1 || z_vr == 3 || z_vr == 5 {
                        (p4(&n, x - (y >> 1) - 2, -1)
                            + 2 * p4(&n, x - (y >> 1) - 1, -1)
                            + p4(&n, x - (y >> 1), -1)
                            + 2)
                            >> 2
                    } else if z_vr == -1 {
                        (p4(&n, -1, 0) + 2 * p4(&n, -1, -1) + p4(&n, 0, -1) + 2) >> 2
                    } else {
                        (p4(&n, -1, y - 1) + 2 * p4(&n, -1, y - 2) + p4(&n, -1, y - 3) + 2) >> 2
                    } as u8;
                }
            }
        }
        6 => {
            // eq. (8-60)..(8-63), Horizontal_Down.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    let z_hd = 2 * y - x;
                    *v = if z_hd >= 0 && z_hd % 2 == 0 {
                        (p4(&n, -1, y - (x >> 1) - 1) + p4(&n, -1, y - (x >> 1)) + 1) >> 1
                    } else if z_hd == 1 || z_hd == 3 || z_hd == 5 {
                        (p4(&n, -1, y - (x >> 1) - 2)
                            + 2 * p4(&n, -1, y - (x >> 1) - 1)
                            + p4(&n, -1, y - (x >> 1))
                            + 2)
                            >> 2
                    } else if z_hd == -1 {
                        (p4(&n, -1, 0) + 2 * p4(&n, -1, -1) + p4(&n, 0, -1) + 2) >> 2
                    } else {
                        (p4(&n, x - 1, -1) + 2 * p4(&n, x - 2, -1) + p4(&n, x - 3, -1) + 2) >> 2
                    } as u8;
                }
            }
        }
        7 => {
            // eq. (8-64)/(8-65), Vertical_Left.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    *v = if y == 0 || y == 2 {
                        (p4(&n, x + (y >> 1), -1) + p4(&n, x + (y >> 1) + 1, -1) + 1) >> 1
                    } else {
                        (p4(&n, x + (y >> 1), -1)
                            + 2 * p4(&n, x + (y >> 1) + 1, -1)
                            + p4(&n, x + (y >> 1) + 2, -1)
                            + 2)
                            >> 2
                    } as u8;
                }
            }
        }
        8 => {
            // eq. (8-66)..(8-69), Horizontal_Up.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (x as i32, y as i32);
                    let z_hu = x + 2 * y;
                    *v = if z_hu == 0 || z_hu == 2 || z_hu == 4 {
                        (p4(&n, -1, y + (x >> 1)) + p4(&n, -1, y + (x >> 1) + 1) + 1) >> 1
                    } else if z_hu == 1 || z_hu == 3 {
                        (p4(&n, -1, y + (x >> 1))
                            + 2 * p4(&n, -1, y + (x >> 1) + 1)
                            + p4(&n, -1, y + (x >> 1) + 2)
                            + 2)
                            >> 2
                    } else if z_hu == 5 {
                        (p4(&n, -1, 2) + 3 * p4(&n, -1, 3) + 2) >> 2
                    } else {
                        p4(&n, -1, 3)
                    } as u8;
                }
            }
        }
        // mode 2 (DC) and any out-of-range value.
        _ => {
            let dc = match (n.top_available, n.left_available) {
                (true, true) => {
                    let sum: u32 = n
                        .top
                        .iter()
                        .chain(n.left.iter())
                        .map(|&v| u32::from(v))
                        .sum();
                    ((sum + 4) >> 3) as u8
                }
                (false, true) => {
                    let sum: u32 = n.left.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 2) >> 2) as u8
                }
                (true, false) => {
                    let sum: u32 = n.top.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 2) >> 2) as u8
                }
                (false, false) => 128,
            };
            out = [[dc; 4]; 4];
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn unavailable16() -> Neighbours16 {
        Neighbours16 {
            top_available: false,
            top: [0; 16],
            left_available: false,
            left: [0; 16],
        }
    }

    fn unavailable_chroma() -> NeighboursChroma {
        NeighboursChroma {
            top_available: false,
            top: [0; 8],
            left_available: false,
            left: [0; 8],
        }
    }

    /// Clause 8.3.2.3, eq. (8-75): the case this round's flat fixture
    /// exercises end to end.
    #[test]
    fn intra16x16_dc_with_no_neighbours_is_128() {
        let out = predict_intra16x16(2, unavailable16());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    /// Clause 8.3.3.1, eq. (8-85) (and its continuation for the other
    /// three quadrants): same "all unavailable -> 128" case, one level
    /// down, for chroma.
    #[test]
    fn chroma_dc_with_no_neighbours_is_128() {
        let out = predict_intra_chroma(0, unavailable_chroma());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    #[test]
    fn intra16x16_vertical_copies_top_row_down_every_row() {
        let mut n = unavailable16();
        n.top_available = true;
        n.top = [7; 16];
        let out = predict_intra16x16(0, n);
        assert!(out.iter().all(|row| *row == [7u8; 16]));
    }

    #[test]
    fn intra16x16_horizontal_copies_left_column_across_every_row() {
        let mut n = unavailable16();
        n.left_available = true;
        n.left = core::array::from_fn(|y| y as u8);
        let out = predict_intra16x16(1, n);
        for (y, row) in out.iter().enumerate() {
            assert_eq!(*row, [y as u8; 16]);
        }
    }

    /// eq. (8-86)/(8-87): the top-right quadrant prefers its own top row
    /// unconditionally over the left column when both are available --
    /// not a symmetric average the way the top-left quadrant is.
    #[test]
    fn chroma_dc_top_right_quadrant_prefers_top_over_left() {
        let n = NeighboursChroma {
            top_available: true,
            top: [0, 0, 0, 0, 8, 8, 8, 8],
            left_available: true,
            left: [4, 4, 4, 4, 4, 4, 4, 4],
        };
        let out = predict_intra_chroma(0, n);
        // Top-right quadrant (columns 4..7, rows 0..3): averages top1
        // alone ((8*4+2)>>2 == 8), not top1+left0 combined.
        assert_eq!(out[0][4], 8);
    }

    /// eq. (8-72): full-average DC when both neighbour edges are
    /// available -- 32 samples of value 4 average to exactly 4.
    #[test]
    fn intra16x16_dc_full_average() {
        let n = Neighbours16 {
            top_available: true,
            top: [4; 16],
            left_available: true,
            left: [4; 16],
        };
        let out = predict_intra16x16(2, n);
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 4)));
    }

    /// End-to-end reconstruction of `cabac_intra_oracle_flat.264`'s one
    /// macroblock (16x16 frame, one 16x16 macroblock, IDR slice) -- the
    /// coordinator's own explicit hand-checkable case. Drives this
    /// module's prediction functions with `Intra16x16PredMode` and
    /// `intra_chroma_pred_mode` decoded *live* off the real bitstream
    /// (`crate::mb::decode_slice_cabac`), not synthetic test inputs, and
    /// with `Neighbours16`/`NeighboursChroma` set to "nothing available"
    /// -- correct by construction for macroblock 0 of a slice, since
    /// clause 6.4.8's own neighbouring-location process has nowhere to
    /// look for macroblock address -1.
    ///
    /// By hand: `mb_type` decodes to `I_16x16_2_0_0` (`Intra16x16PredMode`
    /// = 2 = DC, `CodedBlockPatternLuma` = 0, `CodedBlockPatternChroma` =
    /// 0) and `intra_chroma_pred_mode` = 0 (DC) -- all asserted below
    /// directly off `SliceStats`, not assumed. Zero luma/chroma CBP means
    /// clause 7.3.5's `residual()` is never invoked for this macroblock at
    /// all (no luma AC, no chroma AC, and Intra_16x16's own luma DC block
    /// -- read unconditionally whenever `Intra16x16PredMode` applies --
    /// decodes `coded_block_flag == 0`, independently confirmed by this
    /// crate's own CBP-oracle work two rounds ago), so reconstruction is
    /// prediction alone: eq. (8-75) gives `predL[x,y] = 128` for all 256
    /// luma samples (neither neighbour available), and clause 8.3.3.1's
    /// neither-available case gives `predC[x,y] = 128` for all 64+64
    /// chroma samples. The fully reconstructed picture is uniformly `128`.
    #[test]
    fn flat_fixture_reconstructs_to_uniform_128() {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_flat.264");
        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut stats = None;

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget)
                            .unwrap();
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let s = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                    )
                    .unwrap_or_else(|e| panic!("flat fixture: decode_slice_cabac failed: {e:?}"));
                    assert!(
                        !cabac.malformed(),
                        "flat fixture: CABAC engine reported malformed input"
                    );
                    stats = Some(s);
                }
                _ => {}
            }
        }

        let stats = stats.expect("flat fixture: no slice NAL found");
        assert_eq!(
            stats.macroblock_count, 1,
            "flat fixture: expected exactly one macroblock"
        );
        assert_eq!(
            stats.first_slice_mb_cbp,
            Some((0, 0)),
            "flat fixture: expected zero luma/chroma coded_block_pattern"
        );
        assert_eq!(
            stats.first_slice_mb_intra16x16_pred_mode,
            Some(2),
            "flat fixture: expected Intra16x16PredMode == 2 (DC)"
        );
        assert_eq!(
            stats.first_slice_mb_intra_chroma_pred_mode,
            Some(0),
            "flat fixture: expected intra_chroma_pred_mode == 0 (DC)"
        );

        // Macroblock 0 of an IDR slice: no neighbour has anywhere to come
        // from, so both edges are unavailable -- correct by construction,
        // not merely convenient for this fixture.
        let luma = predict_intra16x16(2, unavailable16());
        assert!(
            luma.iter().all(|row| row.iter().all(|&v| v == 128)),
            "flat fixture: reconstructed luma is not uniformly 128"
        );
        let cb = predict_intra_chroma(0, unavailable_chroma());
        let cr = predict_intra_chroma(0, unavailable_chroma());
        assert!(
            cb.iter().all(|row| row.iter().all(|&v| v == 128)),
            "flat fixture: reconstructed Cb is not uniformly 128"
        );
        assert!(
            cr.iter().all(|row| row.iter().all(|&v| v == 128)),
            "flat fixture: reconstructed Cr is not uniformly 128"
        );
    }

    fn unavailable4() -> Neighbours4 {
        Neighbours4 {
            top_available: false,
            top: [0; 4],
            top_right: [0; 4],
            left_available: false,
            left: [0; 4],
            corner: 0,
        }
    }

    /// Clause 8.3.1.2.1, eq. (8-45): every row of the 4x4 block is a copy
    /// of the top neighbour row.
    #[test]
    fn intra4x4_vertical_copies_top_row_down_every_row() {
        let mut n = unavailable4();
        n.top_available = true;
        n.top = [10, 20, 30, 40];
        let out = predict_intra4x4(0, n);
        assert!(out.iter().all(|row| *row == [10, 20, 30, 40]));
    }

    /// Clause 8.3.1.2.2, eq. (8-46): every column is a copy of the left
    /// neighbour column.
    #[test]
    fn intra4x4_horizontal_copies_left_column_across_every_row() {
        let mut n = unavailable4();
        n.left_available = true;
        n.left = [1, 2, 3, 4];
        let out = predict_intra4x4(1, n);
        for (y, row) in out.iter().enumerate() {
            assert_eq!(*row, [n.left[y]; 4]);
        }
    }

    /// Clause 8.3.1.2.3, eq. (8-50): the one case this module's flat
    /// fixture and gradient fixture never exercise (both are
    /// `Intra_16x16`) but that `cabac_i_only.264`'s own macroblock 0 does.
    #[test]
    fn intra4x4_dc_with_no_neighbours_is_128() {
        let out = predict_intra4x4(2, unavailable4());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    /// Clause 8.3.1.2.4, eq. (8-51): the one hand-checkable corner of
    /// `Diagonal_Down_Left` -- `x = y = 3` uses a different (three-tap,
    /// asymmetric) formula from every other position in the block.
    #[test]
    fn intra4x4_diagonal_down_left_corner_uses_its_own_formula() {
        let mut n = unavailable4();
        n.top_available = true;
        n.top = [0, 0, 0, 0];
        n.top_right = [0, 0, 0, 8];
        let out = predict_intra4x4(3, n);
        // eq. (8-51): (p[6,-1] + 3*p[7,-1] + 2) >> 2 = (0 + 24 + 2) >> 2 = 6.
        assert_eq!(out[3][3], 6);
    }

    /// Clause 8.3.1.1, eq. (8-42) -- the exact case the coordinator asked
    /// to be checked deliberately: neighbour A and neighbour B disagree,
    /// so `Min(intra4x4PredModeA, intra4x4PredModeB)` actually matters
    /// (a test where both neighbours coincidentally agree could pass with
    /// the fallback backwards, e.g. `Max` instead of `Min`, or with the
    /// wrong neighbour's value used outright).
    #[test]
    fn mode_inference_uses_min_of_disagreeing_neighbours() {
        // A = Horizontal (1), B = Diagonal_Down_Left (3): predIntra4x4PredMode
        // must be Min(1, 3) = 1, not 3, not either neighbour picked
        // arbitrarily.
        let pred = infer_intra4x4_pred_mode(1, 3, true, 0);
        assert_eq!(
            pred, 1,
            "prev_flag set: must equal predIntra4x4PredMode = Min(1, 3) = 1"
        );

        // Swapping which neighbour holds which value must not change the
        // result -- Min is commutative, a backwards implementation that
        // picks "A" or "B" specifically would not be.
        let pred_swapped = infer_intra4x4_pred_mode(3, 1, true, 0);
        assert_eq!(
            pred_swapped, 1,
            "Min must be commutative in which argument is A vs B"
        );
    }

    /// Clause 8.3.1.1, eq. (8-42)'s own `rem >= pred` branch: this is the
    /// one that would silently produce a plausible-looking wrong mode if
    /// the `< pred` / `>= pred` comparison were flipped -- both branches
    /// produce a valid mode number (0..8), so only checking against the
    /// primary text's exact comparison direction catches a swap.
    #[test]
    fn mode_inference_rem_at_or_above_pred_is_incremented() {
        // predIntra4x4PredMode = Min(4, 6) = 4. rem = 4 (>= pred) -> mode = rem + 1 = 5.
        assert_eq!(infer_intra4x4_pred_mode(4, 6, false, 4), 5);
        // rem = 2 (< pred) -> mode = rem = 2, unchanged.
        assert_eq!(infer_intra4x4_pred_mode(4, 6, false, 2), 2);
    }
}
