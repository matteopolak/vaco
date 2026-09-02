//! Pure intra prediction sample-generation
//! functions, following the exact split `dequant.rs` established -- primary-
//! text equations as standalone, independently testable functions, not
//! wired into `mb.rs`'s macroblock loop for the general multi-macroblock
//! case here (see `crate::reconstruct` for that composition).
//!
//! # What is implemented, and why not more
//!
//! `Intra_16x16` (clause 8.3.2): Vertical, Horizontal, DC, and Plane
//! (clause 8.3.2.4, transcribed and cross-checked directly against a
//! primary text -- `provenance/sources.toml`'s
//! `iso-iec-14496-10-2002-draft`, fetched and `pdftotext`-extracted for
//! this -- rather than recalled, since a wrong constant here would be
//! silent, not a compile error).
//!
//! Chroma (clause 8.3.3): DC, Horizontal, Vertical, all four 4x4
//! quadrants' worth of clause 8.3.3.1's own case split for DC, and Plane
//! (clause 8.3.3.4, same source, same construction as luma's at chroma's
//! own 8x8 size and constants).
//!
//! Both Plane modes need the corner sample `p[-1,-1]` -- eq. (8-80)/
//! (8-81)'s and (8-102)/(8-103)'s own `x' == 7`/`y' == 3` boundary term
//! reaches back to it -- so [`Neighbours16`]/[`NeighboursChroma`] each
//! carry one, unlike every other mode here, which never reads it.
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
//! [`Neighbours16`]/[`NeighboursChroma`] take already-resolved availability +
//! sample values -- deriving those from a real multi-macroblock picture
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
    /// `p[-1, -1]`, needed only by Plane (eq. (8-80)/(8-81): the `x' == 7`/
    /// `y' == 7` term of `H`/`V` lands exactly here) -- every other mode
    /// this struct serves never reads it, so a caller that never selects
    /// Plane may leave this `0` without consequence (it is provably
    /// unread in that case, not merely unlikely to be).
    pub(crate) corner: u8,
}

/// Clause 8.3.2, `Intra16x16PredMode` (Table 8-3): `0` Vertical, `1`
/// Horizontal, `2` DC, `3` Plane (eq. (8-76)..(8-81)).
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "h/v/a/b/c/x/y mirror clause 8.3.2.4's own equation variable names (8-76)..(8-81)"
)]
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
        3 => {
            // eq. (8-76)..(8-81): a single plane fit through the top and
            // left neighbour rows -- `H`/`V` weighted sums of differences
            // across the top row/left column (whose own `x' == 7`/
            // `y' == 7` term reaches back to the corner sample `p[-1,-1]`,
            // eq. (8-80)/(8-81)), `b`/`c` their per-sample slope in x/y,
            // `a` eq. (8-77)'s `16 * (p[-1,15] + p[15,-1])`. Clause 8.3.2.4
            // requires this mode is only ever selected when every one of
            // these samples (including the corner) is already available,
            // so no availability case split is needed here (unlike DC).
            //
            // `p_top`/`p_left` give the spec's own unified `p[x,-1]`/
            // `p[-1,y]` addressing, including the `x == -1`/`y == -1`
            // corner case -- the same shape `crate::intra`'s own `p4`
            // helper already uses for `Intra_4x4`.
            let p_top = |x: i32| -> i32 {
                if x < 0 {
                    i32::from(n.corner)
                } else {
                    usize::try_from(x)
                        .ok()
                        .and_then(|i| n.top.get(i))
                        .copied()
                        .map_or(0, i32::from)
                }
            };
            let p_left = |y: i32| -> i32 {
                if y < 0 {
                    i32::from(n.corner)
                } else {
                    usize::try_from(y)
                        .ok()
                        .and_then(|i| n.left.get(i))
                        .copied()
                        .map_or(0, i32::from)
                }
            };
            let h: i32 = (0i32..8)
                .map(|x_prime| (x_prime + 1) * (p_top(8 + x_prime) - p_top(6 - x_prime)))
                .sum();
            let v: i32 = (0i32..8)
                .map(|y_prime| (y_prime + 1) * (p_left(8 + y_prime) - p_left(6 - y_prime)))
                .sum();
            let a = 16 * (p_left(15) + p_top(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            let mut out = [[0u8; 16]; 16];
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v_out) in row.iter_mut().enumerate() {
                    let x = i32::try_from(x).unwrap_or(0);
                    let y = i32::try_from(y).unwrap_or(0);
                    let sum = a + b * (x - 7) + c * (y - 7) + 16;
                    *v_out = (sum >> 5).clamp(0, 255) as u8;
                }
            }
            out
        }
        _ => {
            // eq. (8-72)..(8-75): the four DC sub-cases, by availability --
            // delegated to `vaco_codec_dsp_intrapred::dc_predict` (D-09):
            // this is exactly that function's own average/fallback formula
            // at size 16 (count 16 or 32, both powers of two), verified
            // bit-exact against this module's own pre-existing tests below
            // (which pin real expected bytes) before landing.
            let top_u16: [u16; 16] =
                core::array::from_fn(|i| u16::from(n.top.get(i).copied().unwrap_or(0)));
            let left_u16: [u16; 16] =
                core::array::from_fn(|i| u16::from(n.left.get(i).copied().unwrap_or(0)));
            let top: &[u16] = if n.top_available { &top_u16 } else { &[] };
            let left: &[u16] = if n.left_available { &left_u16 } else { &[] };
            let dc = vaco_codec_dsp_intrapred::dc_predict(top, left, 16, 8);
            let dc = u8::try_from(dc).unwrap_or(u8::MAX);
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
    /// `p[-1, -1]`, needed only by Plane (eq. (8-102)/(8-103): the
    /// `x' == 3`/`y' == 3` term of `H`/`V` lands exactly here) -- the
    /// same role [`Neighbours16::corner`] plays for luma.
    pub(crate) corner: u8,
}

/// Clause 8.3.3, chroma prediction mode (Table 8-4, same numbering as
/// luma's `Intra16x16PredMode`): `0` DC, `1` Horizontal, `2` Vertical,
/// `3` Plane (eq. (8-98)..(8-103)).
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "h/v/a/b/c/x/y mirror clause 8.3.3.4's own equation variable names (8-98)..(8-103)"
)]
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
        3 => {
            // eq. (8-98)..(8-103): the same construction as luma Plane
            // (`predict_intra16x16`'s own `3 =>` arm) at chroma's own 8x8
            // size and constants (17/16/5 in place of luma's 5/32/6) --
            // `ChromaArrayType == 1` only, this crate's sole supported
            // chroma format, so `xCF == yCF == 0` throughout and neither
            // ever appears below.
            let p_top = |x: i32| -> i32 {
                if x < 0 {
                    i32::from(n.corner)
                } else {
                    usize::try_from(x)
                        .ok()
                        .and_then(|i| n.top.get(i))
                        .copied()
                        .map_or(0, i32::from)
                }
            };
            let p_left = |y: i32| -> i32 {
                if y < 0 {
                    i32::from(n.corner)
                } else {
                    usize::try_from(y)
                        .ok()
                        .and_then(|i| n.left.get(i))
                        .copied()
                        .map_or(0, i32::from)
                }
            };
            let h: i32 = (0i32..4)
                .map(|x_prime| (x_prime + 1) * (p_top(4 + x_prime) - p_top(2 - x_prime)))
                .sum();
            let v: i32 = (0i32..4)
                .map(|y_prime| (y_prime + 1) * (p_left(4 + y_prime) - p_left(2 - y_prime)))
                .sum();
            let a = 16 * (p_left(7) + p_top(7));
            let b = (17 * h + 16) >> 5;
            let c = (17 * v + 16) >> 5;
            let mut out = [[0u8; 8]; 8];
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v_out) in row.iter_mut().enumerate() {
                    let x = i32::try_from(x).unwrap_or(0);
                    let y = i32::try_from(y).unwrap_or(0);
                    let sum = a + b * (x - 3) + c * (y - 3) + 16;
                    *v_out = (sum >> 5).clamp(0, 255) as u8;
                }
            }
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
/// Widens a `[[u8; 4]; 4]` row/column index (always `0..4`, a fixed array
/// size, never bitstream-derived) to the signed coordinate space `p4`'s
/// eq. (8-45)..(8-69) arithmetic needs. The fallback can never actually run
/// for a 4-element array, but it is honest rather than silent if that ever
/// changes.
fn block4_index_to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// [`block4_index_to_i32`]'s own `Intra_8x8` counterpart -- widens a
/// `[[u8; 8]; 8]` row/column index (always `0..8`) the same way.
fn block8_index_to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

#[must_use]
pub(crate) fn predict_intra4x4(mode: u8, n: Neighbours4) -> [[u8; 4]; 4] {
    let mut out = [[0u8; 4]; 4];
    match mode {
        0 => {
            // eq. (8-45), Vertical.
            out.fill(n.top);
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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
                    let (x, y) = (block4_index_to_i32(x), block4_index_to_i32(y));
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

/// Clause 8.3.2.2's own 25 neighbouring samples for one `Intra_8x8` luma
/// block, already resolved (or substituted, per the rule below) by the
/// caller -- the same "resolved before this module sees it" contract
/// [`Neighbours4`]/[`Neighbours16`] keep. `top`/`top_right` are kept
/// separate for the same reason [`Neighbours4::top_right`] is: clause
/// 8.3.2.2's own substitution ("when `p[8..15,-1]` are marked as not
/// available and `p[7,-1]` is available, substitute `p[7,-1]`'s value for
/// all eight and mark them available") is a caller-side resolution step,
/// not this struct's job.
///
/// This crate's on-hand `iso-iec-14496-10-2002-draft` source predates
/// `Intra_8x8` entirely (`mb.rs`'s own module doc names the same gap for
/// the 8x8 transform) -- every equation on this struct and
/// [`predict_intra8x8`] below is instead read from and cross-checked
/// against JM 19.1's `intra8x8_pred_normal.c` (BSD/Tier A per
/// `provenance/sources.toml`), not the crate's primary spec text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Neighbours8 {
    pub(crate) top_available: bool,
    pub(crate) top: [u8; 8],
    /// `p[8..16,-1]` -- already substituted per the rule above if the real
    /// top-right was unavailable but `p[7,-1]` was.
    pub(crate) top_right: [u8; 8],
    pub(crate) left_available: bool,
    pub(crate) left: [u8; 8],
    /// `p[-1,-1]`, tracked with its own availability (unlike
    /// [`Neighbours4::corner`]/[`Neighbours16::corner`]) because clause
    /// 8.3.2.2.1's reference-sample filter treats "corner available" as a
    /// genuinely independent third condition alongside top/left -- not
    /// merely a value some modes read and others don't.
    pub(crate) corner_available: bool,
    pub(crate) corner: u8,
}

/// Clause 8.3.2.2.1's reference-sample low-pass filter, applied once before
/// any of the nine `Intra_8x8` modes below reads a sample -- including DC,
/// unlike `Intra_4x4`'s own (unfiltered) DC. Transcribed from JM 19.1's
/// `LowPassForIntra8x8Pred` (`intra8x8_pred_normal.c`): every raw sample is
/// first substituted per [`Neighbours8`]'s own doc (unavailable -> 128, the
/// same flat default every "no neighbours at all" case in this module
/// already uses), *then* filtered here, gated independently by which of
/// corner/top/left is available.
///
/// Returns `(corner, top ++ top_right, left)` post-filter, in the same
/// shape [`predict_intra8x8`] consumes.
#[must_use]
/// `.get(i)` widened to `i32` with a `0` fallback -- every call site below
/// has `i` provably in range (fixed 16- or 8-element arrays, loop bounds
/// checked by hand against clause 8.3.2.2.1's own filter shape), but
/// `clippy::indexing_slicing` (denied crate-wide) wants a real fallback
/// regardless of that invariant.
fn at(arr: &[i32], i: usize) -> i32 {
    arr.get(i).copied().unwrap_or(0)
}

#[allow(
    clippy::many_single_char_names,
    reason = "z/n mirror clause 8.3.2.2.1's own p[-1,-1] notation and this module's established Neighbours* naming"
)]
fn filter_intra8x8_refs(n: Neighbours8) -> (u8, [u8; 16], [u8; 8]) {
    let z = if n.corner_available { n.corner } else { 128 };
    let mut top16 = [0u8; 16];
    if n.top_available {
        top16[..8].copy_from_slice(&n.top);
        top16[8..].copy_from_slice(&n.top_right);
    } else {
        top16 = [128; 16];
    }
    let left8 = if n.left_available { n.left } else { [128; 8] };

    // Widen to i32 for the filter taps, then narrow back -- every input is
    // 0..=255 and every tap is a non-negative weighted average (a plain
    // 3-tap low-pass, clause 8.3.2.2.1), so the result is always in range.
    let raw_top: [i32; 16] =
        core::array::from_fn(|i| i32::from(top16.get(i).copied().unwrap_or(0)));
    let raw_left: [i32; 8] =
        core::array::from_fn(|i| i32::from(left8.get(i).copied().unwrap_or(0)));
    let raw_z = i32::from(z);

    let mut filt_z = raw_z;
    if n.corner_available {
        filt_z = if n.top_available && n.left_available {
            (at(&raw_left, 0) + 2 * raw_z + at(&raw_top, 0) + 2) >> 2
        } else if n.top_available {
            (3 * raw_z + at(&raw_top, 0) + 2) >> 2
        } else if n.left_available {
            (3 * raw_z + at(&raw_left, 0) + 2) >> 2
        } else {
            raw_z
        };
    }

    let mut filt_top = raw_top;
    if n.top_available {
        if let Some(slot) = filt_top.get_mut(0) {
            *slot = if n.corner_available {
                (raw_z + 2 * at(&raw_top, 0) + at(&raw_top, 1) + 2) >> 2
            } else {
                (3 * at(&raw_top, 0) + at(&raw_top, 1) + 2) >> 2
            };
        }
        for i in 1..15usize {
            let v = (at(&raw_top, i - 1) + 2 * at(&raw_top, i) + at(&raw_top, i + 1) + 2) >> 2;
            if let Some(slot) = filt_top.get_mut(i) {
                *slot = v;
            }
        }
        if let Some(slot) = filt_top.get_mut(15) {
            *slot = (3 * at(&raw_top, 15) + at(&raw_top, 14) + 2) >> 2;
        }
    }

    let mut filt_left = raw_left;
    if n.left_available {
        if let Some(slot) = filt_left.get_mut(0) {
            *slot = if n.corner_available {
                (raw_z + 2 * at(&raw_left, 0) + at(&raw_left, 1) + 2) >> 2
            } else {
                (3 * at(&raw_left, 0) + at(&raw_left, 1) + 2) >> 2
            };
        }
        for i in 1..7usize {
            let v = (at(&raw_left, i - 1) + 2 * at(&raw_left, i) + at(&raw_left, i + 1) + 2) >> 2;
            if let Some(slot) = filt_left.get_mut(i) {
                *slot = v;
            }
        }
        if let Some(slot) = filt_left.get_mut(7) {
            *slot = (at(&raw_left, 6) + 3 * at(&raw_left, 7) + 2) >> 2;
        }
    }

    let z_out = filt_z.clamp(0, 255) as u8;
    let top_out: [u8; 16] = core::array::from_fn(|i| at(&filt_top, i).clamp(0, 255) as u8);
    let left_out: [u8; 8] = core::array::from_fn(|i| at(&filt_left, i).clamp(0, 255) as u8);
    (z_out, top_out, left_out)
}

/// Clause 8.3.2.2's own unified `p[x, y]` addressing for one `Intra_8x8`
/// block, `x = -1..=15`, `y = -1..=7` -- [`p4`]'s own doc explains the same
/// shape one size down. `top` is the 16-sample filtered top row
/// ([`filter_intra8x8_refs`]'s own output, `A..P` in JM's notation), `left`
/// the 8-sample filtered left column, `z` the filtered corner.
fn p8(z: u8, top: &[u8; 16], left: [u8; 8], x: i32, y: i32) -> i32 {
    if x == -1 && y == -1 {
        return i32::from(z);
    }
    if y == -1 {
        return usize::try_from(x)
            .ok()
            .and_then(|i| top.get(i))
            .copied()
            .map_or(0, i32::from);
    }
    usize::try_from(y)
        .ok()
        .and_then(|i| left.get(i))
        .copied()
        .map_or(0, i32::from)
}

/// The nine `Intra_8x8` modes (clause 8.3.2.2.2..8.3.2.2.10, Table 8-2's
/// same nine-mode numbering `Intra_4x4` uses, at 8x8 size and against
/// *filtered* reference samples). Every equation here is the direct
/// size-8 generalisation of [`predict_intra4x4`]'s own -- confirmed
/// against JM 19.1's `intra8x8_pred_normal.c` mode by mode, not assumed:
/// modes 0/1/2/3/4/7 generalise with no boundary-case changes at all
/// (`Vertical_Left`'s own two-branch even/odd split, for instance, needs
/// no extra case at 8x8 either); `Diagonal_Down_Left`'s corner tap moves
/// from `x == y == 3` to `x == y == 7`; `Vertical_Right`/`Horizontal_Down`'s
/// "very negative" branch naturally stays in range via `p8`'s own corner
/// fallback (never needs its own edge case); `Horizontal_Up` is the one
/// mode that needs a genuinely new edge case beyond a literal
/// generalisation -- see its own comment below.
#[must_use]
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "mirrors predict_intra4x4's own per-mode match arms, one size up; splitting the match would lose the direct mode-by-mode correspondence to JM's own functions"
)]
pub(crate) fn predict_intra8x8(mode: u8, n: Neighbours8) -> [[u8; 8]; 8] {
    let (z, top, left) = filter_intra8x8_refs(n);
    let p = |x: i32, y: i32| p8(z, &top, left, x, y);
    let mut out = [[0u8; 8]; 8];
    match mode {
        0 => {
            // eq. analogous to (8-45): predL[x,y] = p[x,-1].
            let row: [u8; 8] = core::array::from_fn(|i| top.get(i).copied().unwrap_or(0));
            out.fill(row);
        }
        1 => {
            // predL[x,y] = p[-1,y].
            for (y, row) in out.iter_mut().enumerate() {
                *row = [left.get(y).copied().unwrap_or(0); 8];
            }
        }
        3 => {
            // Diagonal_Down_Left: corner tap at x==y==7 uses the 8x8-sized
            // two-tap edge formula (`p[14,-1] + 3*p[15,-1]`, JM's
            // `PredArray[14]`); every other position is the general
            // three-tap average, needing `top` samples up to index 15 --
            // exactly why `top` is 16 long here, not 8.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    *v = if x == 7 && y == 7 {
                        (p(14, -1) + 3 * p(15, -1) + 2) >> 2
                    } else {
                        (p(x + y, -1) + 2 * p(x + y + 1, -1) + p(x + y + 2, -1) + 2) >> 2
                    } as u8;
                }
            }
        }
        4 => {
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    *v = match x.cmp(&y) {
                        core::cmp::Ordering::Greater => {
                            (p(x - y - 2, -1) + 2 * p(x - y - 1, -1) + p(x - y, -1) + 2) >> 2
                        }
                        core::cmp::Ordering::Less => {
                            (p(-1, y - x - 2) + 2 * p(-1, y - x - 1) + p(-1, y - x) + 2) >> 2
                        }
                        core::cmp::Ordering::Equal => {
                            (p(0, -1) + 2 * p(-1, -1) + p(-1, 0) + 2) >> 2
                        }
                    } as u8;
                }
            }
        }
        5 => {
            // Vertical_Right, clause 8.3.2.2.5. The `zVR < -1` branch walks
            // *up the left column* by `y - 2*x`, not by `y`: at 8x8 a
            // position can be several columns past the diagonal, and each
            // extra column steps the left-column index two rows further
            // up. Dropping that `- 2*x` (as this arm did) makes every
            // position in a row below the diagonal reuse the value the
            // leftmost such position computed, which is why this mode
            // failed on 33 of the 60 blocks that used it in a
            // directional-content fixture while passing on flat content
            // that rarely selects it.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    let z_vr = 2 * x - y;
                    *v = if z_vr >= 0 && z_vr % 2 == 0 {
                        (p(x - (y >> 1) - 1, -1) + p(x - (y >> 1), -1) + 1) >> 1
                    } else if z_vr > 0 {
                        (p(x - (y >> 1) - 2, -1)
                            + 2 * p(x - (y >> 1) - 1, -1)
                            + p(x - (y >> 1), -1)
                            + 2)
                            >> 2
                    } else if z_vr == -1 {
                        (p(-1, 0) + 2 * p(-1, -1) + p(0, -1) + 2) >> 2
                    } else {
                        let base = y - 2 * x;
                        (p(-1, base - 1) + 2 * p(-1, base - 2) + p(-1, base - 3) + 2) >> 2
                    } as u8;
                }
            }
        }
        6 => {
            // Horizontal_Down, clause 8.3.2.2.6: the exact transpose of
            // Vertical_Right above, including its `zHD < -1` branch, which
            // walks *along the top row* by `x - 2*y` rather than by `x`.
            // Same bug, same fix, same reason it survived a flat-content
            // fixture -- see mode 5.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    let z_hd = 2 * y - x;
                    *v = if z_hd >= 0 && z_hd % 2 == 0 {
                        (p(-1, y - (x >> 1) - 1) + p(-1, y - (x >> 1)) + 1) >> 1
                    } else if z_hd > 0 {
                        (p(-1, y - (x >> 1) - 2)
                            + 2 * p(-1, y - (x >> 1) - 1)
                            + p(-1, y - (x >> 1))
                            + 2)
                            >> 2
                    } else if z_hd == -1 {
                        (p(-1, 0) + 2 * p(-1, -1) + p(0, -1) + 2) >> 2
                    } else {
                        let base = x - 2 * y;
                        (p(base - 1, -1) + 2 * p(base - 2, -1) + p(base - 3, -1) + 2) >> 2
                    } as u8;
                }
            }
        }
        7 => {
            // Vertical_Left: confirmed against JM's own row assembly with
            // no extra edge case at 8x8 (max index reached is 7+3+2==12,
            // within the 16-long top row).
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    *v = if y % 2 == 0 {
                        (p(x + (y >> 1), -1) + p(x + (y >> 1) + 1, -1) + 1) >> 1
                    } else {
                        (p(x + (y >> 1), -1)
                            + 2 * p(x + (y >> 1) + 1, -1)
                            + p(x + (y >> 1) + 2, -1)
                            + 2)
                            >> 2
                    } as u8;
                }
            }
        }
        8 => {
            // Horizontal_Up: the one mode JM's own code needs a genuine
            // new edge case for at 8x8 (not merely a literal
            // generalisation) -- `z_hu > 13` is flat `p[-1,7]` (JM's own
            // `PredArray[14]`, used for every position past that), and
            // `z_hu == 13` clamps its own third tap to `p[-1,7]` rather
            // than reading the (nonexistent) `p[-1,8]` -- both confirmed
            // against `intra8x8_hor_up_pred`'s own `PredArray` construction
            // and row assembly, not assumed from the 4x4 case's `z_hu==5`
            // shape.
            for (y, row) in out.iter_mut().enumerate() {
                for (x, v) in row.iter_mut().enumerate() {
                    let (x, y) = (block8_index_to_i32(x), block8_index_to_i32(y));
                    let z_hu = x + 2 * y;
                    let base = y + (x >> 1);
                    *v = if z_hu > 13 {
                        p(-1, 7)
                    } else if z_hu % 2 == 0 {
                        (p(-1, base) + p(-1, base + 1) + 1) >> 1
                    } else {
                        (p(-1, base) + 2 * p(-1, base + 1) + p(-1, (base + 2).min(7)) + 2) >> 2
                    } as u8;
                }
            }
        }
        // mode 2 (DC) and any out-of-range value -- clause 8.3.2.2.3, the
        // same four-way availability split as Intra_16x16's own DC
        // (`predict_intra16x16`'s `_ =>` arm) at 8x8's own size/rounding
        // constants (sum of 8 or 16, round +4/+8, >>3/>>4), against the
        // *filtered* samples like every other Intra_8x8 mode -- unlike
        // Intra_4x4's own (unfiltered) DC.
        _ => {
            let dc = match (n.top_available, n.left_available) {
                (true, true) => {
                    let sum: u32 = top[..8]
                        .iter()
                        .chain(left.iter())
                        .map(|&v| u32::from(v))
                        .sum();
                    ((sum + 8) >> 4) as u8
                }
                (true, false) => {
                    let sum: u32 = top[..8].iter().map(|&v| u32::from(v)).sum();
                    ((sum + 4) >> 3) as u8
                }
                (false, true) => {
                    let sum: u32 = left.iter().map(|&v| u32::from(v)).sum();
                    ((sum + 4) >> 3) as u8
                }
                (false, false) => 128,
            };
            out = [[dc; 8]; 8];
        }
    }
    out
}

/// Clause 8.3.2.2's own mode-inference formula -- textually identical to
/// [`infer_intra4x4_pred_mode`]'s `eq. (8-42)` (`Intra8x8PredMode`'s own
/// neighbour derivation reuses the same `Min`/`prev`/`rem` combination,
/// confirmed against JM's shared `readIntraPredMode_CABAC`, which drives
/// both `Intra_4x4` and `Intra_8x8` through one function and one context
/// pair) -- kept as its own named function rather than a bare alias so a
/// caller reads which block size's mode it is inferring.
#[must_use]
pub(crate) const fn infer_intra8x8_pred_mode(
    mode_a: u8,
    mode_b: u8,
    prev_flag: bool,
    rem: u8,
) -> u8 {
    infer_intra4x4_pred_mode(mode_a, mode_b, prev_flag, rem)
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
            corner: 0,
        }
    }

    fn unavailable_chroma() -> NeighboursChroma {
        NeighboursChroma {
            top_available: false,
            top: [0; 8],
            left_available: false,
            left: [0; 8],
            corner: 0,
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
            corner: 0,
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
            corner: 0,
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
    /// all (no luma AC, no chroma AC, and `Intra_16x16`'s own luma DC block
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
                        None,
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

    fn unavailable8() -> Neighbours8 {
        Neighbours8 {
            top_available: false,
            top: [0; 8],
            top_right: [0; 8],
            left_available: false,
            left: [0; 8],
            corner_available: false,
            corner: 0,
        }
    }

    /// Clause 8.3.2.2.3's own "neither available" DC case: flat 128, same
    /// as every other "no neighbours at all" mode in this module.
    #[test]
    fn intra8x8_dc_with_no_neighbours_is_128() {
        let out = predict_intra8x8(2, unavailable8());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    /// Clause 8.3.2.2.2, Vertical: every row is a copy of the (filtered)
    /// top row -- flat input filters to itself, so a flat top row of 7s is
    /// the simplest possible hand-check.
    #[test]
    fn intra8x8_vertical_copies_filtered_top_row_down_every_row() {
        let mut n = unavailable8();
        n.top_available = true;
        n.top = [7; 8];
        n.top_right = [7; 8];
        let out = predict_intra8x8(0, n);
        assert!(out.iter().all(|row| *row == [7u8; 8]));
    }

    #[test]
    fn intra8x8_horizontal_copies_filtered_left_column_across_every_row() {
        let mut n = unavailable8();
        n.left_available = true;
        n.left = [3; 8];
        let out = predict_intra8x8(1, n);
        assert!(out.iter().all(|row| *row == [3u8; 8]));
    }

    /// Clause 8.3.2.2.1's own filter, isolated: with every neighbour flat
    /// at the same value, filtering is a no-op (every 3-tap average of a
    /// constant is that constant) -- confirmed via DC's own full-average
    /// branch, which would visibly shift for any filter-arithmetic bug
    /// (e.g. a dropped `+2` rounding term) even though a flat input
    /// usually hides one.
    #[test]
    fn intra8x8_dc_full_average_of_flat_neighbours() {
        let n = Neighbours8 {
            top_available: true,
            top: [9; 8],
            top_right: [9; 8],
            left_available: true,
            left: [9; 8],
            corner_available: true,
            corner: 9,
        };
        let out = predict_intra8x8(2, n);
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 9)));
    }

    /// Clause 8.3.2.2.1's filter, non-flat: the corner filter's own
    /// three-way case split (both available -> `(left+2*z+top+2)>>2`),
    /// hand-computed. `z=0`, `top[0]=8`, `left[0]=4` (both real, no
    /// substitution): filtered corner = `(4 + 0 + 8 + 2) >> 2 = 3`, which
    /// `Vertical_Left` mode's own `y==0,x==0` case exposes directly as
    /// `(p(0,-1)+p(1,-1)+1)>>1` using the *filtered* top row, not `z`
    /// itself -- so instead check Horizontal (mode 1, `y=0`) which reads
    /// `left[0]` after filtering: `(z + 2*left[0] + left[1] + 2) >> 2`
    /// with `z=0, left=[4,4,...], left[1]=4` gives `(0+8+4+2)>>2=3`.
    #[test]
    fn intra8x8_reference_filter_hand_computed_corner_case() {
        let n = Neighbours8 {
            top_available: true,
            top: [8; 8],
            top_right: [8; 8],
            left_available: true,
            left: [4; 8],
            corner_available: true,
            corner: 0,
        };
        let out = predict_intra8x8(1, n); // Horizontal
        assert_eq!(
            out[0][0], 3,
            "filtered left[0] must be (z + 2*left[0] + left[1] + 2) >> 2 = 3"
        );
        // Every other row of left[] is unaffected by the corner (only
        // left[0] blends in `z`), so rows 1..7 stay exactly 4.
        for row in &out[1..] {
            assert_eq!(row[0], 4);
        }
    }

    /// Clause 8.3.2.2.4 (`Diagonal_Down_Left`)'s own corner tap: `x=y=7`
    /// uses the two-tap edge formula `(p[14,-1] + 3*p[15,-1] + 2) >> 2`,
    /// not the general three-tap average -- the direct 8x8 analogue of
    /// `intra4x4_diagonal_down_left_corner_uses_its_own_formula`. Reference
    /// samples are *filtered* first (clause 8.3.2.2.1, unlike `Intra_4x4`),
    /// so the expected value is computed from the *filtered* `p[14,-1]`/
    /// `p[15,-1]`, not the raw input directly: with `top_right =
    /// [0,0,0,0,0,0,0,16]` and no corner/left available, the filter (no
    /// corner term, since `corner_available` is false) gives
    /// `p'[14,-1] = (0 + 2*0 + 16 + 2) >> 2 = 4` and
    /// `p'[15,-1] = (3*16 + 0 + 2) >> 2 = 12`, so the corner tap is
    /// `(4 + 3*12 + 2) >> 2 = 10`.
    #[test]
    fn intra8x8_diagonal_down_left_corner_uses_its_own_formula() {
        let mut n = unavailable8();
        n.top_available = true;
        n.top = [0; 8];
        n.top_right = [0, 0, 0, 0, 0, 0, 0, 16];
        let out = predict_intra8x8(3, n);
        assert_eq!(out[7][7], 10);
    }

    /// Clause 8.3.2.2.10 (`Horizontal_Up`)'s own two edge cases beyond a
    /// literal 4x4-style generalisation: `z_hu == 13` (the bottom-right-most
    /// diagonal position the general formula can reach, its own last tap
    /// clamped to index 7) and `z_hu > 13` (flat `p[-1,7]`) -- both
    /// confirmed against JM's own `PredArray[13]`/`PredArray[14]`. As
    /// above, the expected values are computed from the *filtered* left
    /// column: with `left = [0,0,0,0,0,0,6,20]` and no corner available,
    /// filtering gives `p'[-1,6] = (0 + 12 + 20 + 2) >> 2 = 8` and
    /// `p'[-1,7] = (6 + 60 + 2) >> 2 = 17`.
    #[test]
    fn intra8x8_horizontal_up_edge_cases() {
        let mut n = unavailable8();
        n.left_available = true;
        n.left = [0, 0, 0, 0, 0, 0, 6, 20];
        let out = predict_intra8x8(8, n);
        // x=7,y=3: z_hu = 7+6 = 13 -> clamped-tap formula
        // (p'[-1,6] + 3*p'[-1,7] + 2) >> 2 = (8 + 51 + 2) >> 2 = 15.
        assert_eq!(out[3][7], 15);
        // Every position with z_hu > 13 is flat p'[-1,7] == 17.
        assert_eq!(out[7][7], 17); // z_hu = 7 + 14 = 21
        assert_eq!(out[4][7], 17); // z_hu = 7 + 8 = 15
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
