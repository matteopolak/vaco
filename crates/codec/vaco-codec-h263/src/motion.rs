//! Motion vector reconstruction and half-pel interpolation, shared in
//! shape by H.261 and H.263 but not in the vector-prediction rule: H.261
//! predicts a vector from the *previous macroblock* alone (§4.2.3.4, reset
//! to zero at specific points); H.263 predicts the *median* of three
//! neighbouring macroblocks' vectors (§6.1.1). Both then add a decoded
//! difference and wrap the result into range exactly like H.262's own
//! motion vectors do (`vaco-codec-mpeg12::motion::decode_component`'s
//! `range`/`low`/`high` clamp) — H.261 mod 32 (full-pel), H.263 mod 64
//! (half-pel).

use crate::picture::RefPicture;

/// H.261 §4.2.3.4: reconstruct one full-pel MVD component. `f` is always 1
/// (H.261 has no `f_code`-style range extension), so the valid range is a
/// fixed `-15..=15` before wraparound and `delta` is the table value
/// as-is.
pub(crate) fn h261_vector(prediction: i32, delta: i32) -> i32 {
    let mut v = prediction + delta;
    if v < -15 {
        v += 32;
    }
    if v > 15 {
        v -= 32;
    }
    v
}

/// H.263 §6.1.1: reconstruct one half-pel MVD component. The valid range
/// for the *reconstructed* vector component is `[-16, 15.5]` full-pel
/// (`-32..=31` in this function's half-pel units) — half of Table 11's own
/// 64-entry span, not all of it: "only one of the pair will yield a
/// macroblock vector component falling within the permitted range" is
/// exactly a mod-64 correction, not mod-128.
pub(crate) fn h263_vector(prediction: i32, delta: i32) -> i32 {
    let mut v = prediction + delta;
    if v < -32 {
        v += 64;
    }
    if v > 31 {
        v -= 64;
    }
    v
}

/// Annex D §D.2 (`Vaco-Spec-Ref: itu-t-h263` D.2): reconstruct one
/// half-pel `MVD` component in the Unrestricted Motion Vector mode when
/// `PLUSPTYPE` is *not* present in the picture header (the original
/// H.263 version 1 `UMV` bit, `PTYPE` bit 10). `delta` is the same raw
/// Table 14 value [`h263_vector`] uses; the difference is entirely in how
/// it is combined with the predictor.
///
/// §D.2's own text: "If the predictor... is in the range `[-15.5, 16]`,
/// only the first column of vector differences applies" — i.e. no
/// wraparound at all, since Table 14's 64 codes already span exactly one
/// window relative to any predictor in that band (`pred + delta` cannot
/// leave `[pred-32, pred+31]`, which is entirely inside the representable
/// `[-63, 63]` half-pel span whenever `pred` itself is so bounded).
/// Outside that band (only reachable in this mode, since a bounded
/// predictor is `[-32, 31]` in the baseline mode `h263_vector` produces),
/// §D.2 requires "the vector difference from Table 14 that results in a
/// vector component inside `[-31.5, 31.5]` with the same sign as the
/// predictor (including zero)" — a ±64 correction against that wider,
/// sign-matched (rather than absolute, as in [`h263_vector`]) window.
#[must_use]
pub(crate) fn h263_umv_vector_legacy(pred: i32, delta: i32) -> i32 {
    if (-31..=32).contains(&pred) {
        return pred + delta;
    }
    let mut v = pred + delta;
    let mut guard = 0;
    while (v > 63 || (pred < 0 && v > 0)) && guard < 8 {
        v -= 64;
        guard += 1;
    }
    while (v < -63 || (pred > 0 && v < 0)) && guard < 16 {
        v += 64;
        guard += 1;
    }
    v.clamp(-63, 63)
}

/// Annex D §D.2 (`Vaco-Spec-Ref: itu-t-h263` D.2): reconstruct one
/// half-pel `MVD`/`MVD2-4` component decoded from Table D.3 (see
/// [`crate::block::decode_table_d3`]) when `PLUSPTYPE` is present. Unlike
/// the legacy path above, this needs no wraparound correction at all:
/// "Every entry in Table D.3 has a single value (in contrast to Table
/// 14)" — the difference it decodes is already unambiguous, so the
/// reconstructed vector is simply the sum. Tables D.1/D.2's per-format
/// ranges (`UUI == "1"`) and the unrestricted case (`UUI == "01"`) are
/// both encoder-side sending restrictions, not a decoder-side
/// reconstruction rule — a conforming encoder never sends a `pred+delta`
/// outside them, so this crate does not need to special-case either.
#[must_use]
pub(crate) fn h263_umv_vector_plus(pred: i32, delta: i32) -> i32 {
    pred + delta
}

/// H.263 §6.1.1/Table 15: derive one chrominance motion vector component
/// (half-pel units, chroma grid) from the corresponding luma component
/// `m` (half-pel units, luma grid). Halving `m` gives a *quarter*-pel
/// value on the chroma grid (chroma samples are spaced twice as far
/// apart), which Table 15 then snaps to the nearest half-pel: the exact
/// whole-pixel phase (remainder 0) is kept, every other quarter-pel phase
/// (1/4, 1/2, 3/4) snaps to 1/2. Using `div_euclid`/`rem_euclid` rather
/// than plain `/`/`%` makes this the same rule for a negative `m`: the
/// remainder is always the *forward* offset from the next lower whole
/// chroma pixel, never a sign-flipped one.
#[must_use]
pub(crate) fn h263_chroma_mv(m: i32) -> i32 {
    let base = m.div_euclid(4);
    let phase = m.rem_euclid(4);
    2 * base + i32::from(phase != 0)
}

/// Annex F §F.2 (`Vaco-Spec-Ref: itu-t-h263` Table F.1): the chrominance
/// motion vector for a four-vector macroblock — "the sum of the four
/// luminance vectors [divided] by 8", then Table F.1's own three-bucket
/// snap of the sixteenth-pixel remainder to the nearest half-pixel
/// chrominance position. Distinct from [`h263_chroma_mv`]'s own rule
/// (single-vector case, `/4` with a two-bucket snap) — F.2's text
/// restricts this `/8`, three-bucket rule explicitly to "if four vectors
/// are used"; a one-vector macroblock, even under Advanced Prediction
/// mode, keeps the plain default rule (summing four *identical* values
/// and dividing by 8 would silently halve the chroma displacement
/// `h263_chroma_mv` gives that same vector — not an equivalent
/// reformulation, a different, wrong answer).
#[must_use]
pub(crate) fn annex_f_chroma_mv(mvs: [i32; 4]) -> i32 {
    // Table F.1: sixteenths 0-2 snap down (bucket 0), 3-13 snap to the
    // half-pixel bucket (1), 14-15 snap up to the next full pixel (2) —
    // transcribed as a direct lookup, not a formula, to match the
    // primary text's own table cell-for-cell.
    const BUCKET: [i32; 16] = [0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2];
    let total = mvs[0] + mvs[1] + mvs[2] + mvs[3];
    let base = total.div_euclid(16);
    let sixteenths = total.rem_euclid(16);
    let snapped = BUCKET
        .get(usize::try_from(sixteenths).unwrap_or(0))
        .copied()
        .unwrap_or(0);
    2 * base + snapped
}

/// H.261 §3.2.2: "The motion vector for both colour difference blocks is
/// derived by halving the component values of the macroblock vector and
/// truncating the magnitude parts towards zero" — Rust's own `/` on `i32`
/// already truncates toward zero, so this is that literal formula, not an
/// approximation of it.
#[allow(
    clippy::integer_division,
    reason = "H.261 §3.2.2 specifies exactly this: halve then truncate the magnitude towards zero, which is precisely what `i32`'s own `/` does"
)]
#[must_use]
pub(crate) fn h261_chroma_mv(m: i32) -> i32 {
    m / 2
}

/// H.263 §6.1.1: the predictor for one component is the median of the
/// three candidate macroblocks' own (already-reconstructed) vectors for
/// that component, after the not-coded/out-of-picture substitution rules
/// in that clause have been applied by the caller.
pub(crate) fn median3(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

/// Annex F §F.2 (`Vaco-Spec-Ref: itu-t-h263` Figure F.1): which three
/// already-decoded 8x8 luma blocks feed `MV1`/`MV2`/`MV3` for one block
/// (0..=3, Figure 5's numbering: 0 top-left, 1 top-right, 2 bottom-left,
/// 3 bottom-right) of a macroblock using the Advanced Prediction mode's
/// per-block motion vectors — one vector's own predictor, not the OBMC
/// pixel-reconstruction weighting (Figures F.2-F.4/`§F.3`), which is a
/// separate, not-yet-implemented piece.
///
/// Returned as `(dgx, dgy)` offsets in a fine grid of 8x8 blocks (two
/// columns/rows per macroblock, raster order) *relative to this block's
/// own position* — the caller adds them to `(block_gx, block_gy)` and
/// looks up whatever macroblock (already-decoded neighbour or the
/// current one) owns that grid cell. A cell inside the *current*
/// macroblock is what Figure F.1 draws with a thick border ("internal");
/// a cell in a different, already-decoded macroblock is what it draws as
/// a separate thin-bordered box ("external") — this function does not
/// distinguish the two cases itself, since the distinction falls out
/// automatically from which macroblock the resulting absolute grid
/// position belongs to.
///
/// Block 0 is the exception the primary text calls out explicitly (F.2:
/// "If only one vector per macroblock is present, MV1, MV2 and MV3 are
/// defined as for the 8*8 block numbered 1" — i.e. block 0 here keeps
/// the *macroblock*-granularity §6.1.1/Figure 12 rule bit-for-bit, not a
/// naive extension of the fine-grid rule below to its own position: its
/// `MV3` is the above-*macroblock*-to-the-right's own block 2, two grid
/// columns over, not the block one grid column over (which is still
/// inside the directly-above macroblock). Blocks 1, 2 and 3 all use one
/// uniform rule instead — left/above/above-right in the fine grid,
/// exactly Figure 12's shape one level finer — which was measured
/// directly off Figure F.1 (pixel-thickness analysis of a 300dpi render,
/// distinguishing the thick/thin border convention; see
/// `docs/codec/vaco-codec-h263.md`'s own account, including a correction
/// to this project's own earlier claim that block 2 was "fully
/// internal" — its `MV1` is external), then cross-checked bit-exact
/// against a real `ffmpeg -flags +mv4 -obmc 1` fixture: for every one of
/// 63 interior macroblocks (non-uniform, per-macroblock-varying motion,
/// deliberately constructed so an internal-vs-external misreading would
/// diverge numerically) in a real encode, `predictor + MVD` computed
/// with the offsets below reproduces `ffmpeg`'s own decoded final vector
/// exactly, for all four blocks — 100%, where a plausible wrong
/// alternate reading (e.g. the pre-correction "block 2 fully internal"
/// claim) matches only by coincidence on a small fraction of blocks.
#[must_use]
pub(crate) const fn annex_f_predictor_sources(block: u8) -> [(i32, i32); 3] {
    match block {
        0 => [(-1, 0), (0, -1), (2, -1)],
        1 | 2 => [(-1, 0), (0, -1), (1, -1)],
        _ => [(-1, 0), (-1, -1), (0, -1)],
    }
}

/// §7.6.4-style half-pel sample: `mv` is in half-pel units. Read one
/// sample from `refp` at integer/half-pel position `(src_x, src_y) +
/// mv/2`, using bilinear interpolation (H.263 §6.1.2, Figure 12:
/// `a=A; b=(A+B+1)/2; c=(A+C+1)/2; d=(A+B+C+D+2)/4`) — the identical
/// formula, same rounding, as H.262's own half-pel scheme.
#[allow(
    clippy::too_many_arguments,
    reason = "the sampling equation genuinely has this many independent inputs (reference, plane, both positions, both mv components); a struct would not make any call site clearer"
)]
pub(crate) fn sample_half_pel(
    refp: &RefPicture,
    plane: usize,
    src_x: i32,
    src_y: i32,
    mv_x: i32,
    mv_y: i32,
    rcontrol: bool,
) -> u8 {
    let int_x = mv_x.div_euclid(2);
    let int_y = mv_y.div_euclid(2);
    let half_x = mv_x.rem_euclid(2) != 0;
    let half_y = mv_y.rem_euclid(2) != 0;
    let x = src_x + int_x;
    let y = src_y + int_y;

    match (half_x, half_y) {
        (false, false) => refp.sample(plane, x, y),
        (true, false) => avg2(
            refp.sample(plane, x, y),
            refp.sample(plane, x + 1, y),
            rcontrol,
        ),
        (false, true) => avg2(
            refp.sample(plane, x, y),
            refp.sample(plane, x, y + 1),
            rcontrol,
        ),
        (true, true) => avg4(
            refp.sample(plane, x, y),
            refp.sample(plane, x + 1, y),
            refp.sample(plane, x, y + 1),
            refp.sample(plane, x + 1, y + 1),
            rcontrol,
        ),
    }
}

/// Annex F §F.3 (`Vaco-Spec-Ref: itu-t-h263` Figure F.2): weighting
/// values `H0(i, j)` for the prediction using the current luminance
/// block's own motion vector — indexed `[j][i]` (row `j`, then column
/// `i`, matching the primary text's own "`(i, j)` denotes the column and
/// row, respectively" and this crate's existing `pred[y * 8 + x]`
/// row-major convention for an 8x8 block).
const OBMC_H0: [i32; 64] = [
    4, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 5, 5, 5, 5, 6, 6, 6, 6, 5, 5,
    5, 5, 6, 6, 6, 6, 5, 5, 5, 5, 6, 6, 6, 6, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 5, 5, 5, 5, 5, 5, 4,
];

/// Annex F §F.3 (Figure F.3): weighting values `H1(i, j)` for the
/// "vertical remote" prediction — the block above the current one for
/// row `j < 4`, the block below for `j >= 4` (§F.3's own text: "for the
/// upper half of the block the motion vector corresponding to the block
/// above... is used, while for the lower half... the block below").
/// Mirror-symmetric top/bottom by construction, since the same
/// distance-from-the-relevant-border weighting applies whichever half a
/// row falls in.
const OBMC_H1: [i32; 64] = [
    2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2,
];

/// Annex F §F.3 (Figure F.4): weighting values `H2(i, j)` for the
/// "horizontal remote" prediction — the block to the left for column
/// `i < 4`, the block to the right for `i >= 4`. Mirror-symmetric
/// left/right, the same shape as `OBMC_H1` rotated 90 degrees in
/// concept, but transcribed independently from the primary text rather
/// than derived from `OBMC_H1` by assumption (the two tables are *not*
/// exact transposes of one another cell-for-cell, verified directly
/// against the rendered figures — only `OBMC_H0 + OBMC_H1 + OBMC_H2 ==
/// 8` at every one of the 64 cells is a shape invariant, checked by this
/// module's own tests).
const OBMC_H2: [i32; 64] = [
    2, 1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 2, 2,
    2, 2, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 2,
];

/// One weighting value from an OBMC table (`OBMC_H0`/`OBMC_H1`/`OBMC_H2`,
/// each flattened row-major, row `j` then column `i`) — `0` on an
/// out-of-range index, which never happens for the `0..8` loop bounds
/// every caller uses, but keeps this crate's blanket
/// `indexing_slicing = "deny"` satisfied without a panic path.
fn obmc_weight(table: &[i32; 64], j: usize, i: usize) -> i32 {
    table.get(j * 8 + i).copied().unwrap_or(0)
}

/// Annex F §F.3 (`Vaco-Spec-Ref: itu-t-h263` Figure F.3/F.4's own
/// prediction equation): one 8x8 luminance OBMC prediction block.
/// `mv_own` is the current block's own (already fully reconstructed,
/// including the `annex_f_predictor_sources`-based predictor) motion
/// vector; `mv_above`/`mv_below`/`mv_left`/`mv_right` are the four
/// "remote" vectors, already resolved by the caller through §F.3's own
/// not-coded/INTRA/border/bottom-of-macroblock substitution rules — this
/// function only does the per-pixel weighted combination, not the
/// neighbour-resolution policy.
#[allow(
    clippy::too_many_arguments,
    reason = "the OBMC equation genuinely combines this many independent per-direction motion vectors (own, above, below, left, right) with the reference/position/rounding context every other sampling function in this module already takes"
)]
#[allow(
    clippy::integer_division,
    reason = "`P(x,y) = (q*H0 + r*H1 + s*H2 + 4) / 8` is Annex F's own literal formula (`Vaco-Spec-Ref: itu-t-h263` F.3) — the add-then-truncate form is round-to-nearest for a divisor of 8, not an approximation of it, the same convention `avg2`/`avg4` already use for their own divisors"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "q/r/s and x/y are Annex F's own primary-text variable names (`Vaco-Spec-Ref: itu-t-h263` F.3's own P(x,y) = (q*H0 + r*H1 + s*H2 + 4)/8 equation) - renaming them would make this harder to check against the spec, not easier"
)]
pub(crate) fn annex_f_obmc_luma_block(
    refp: &RefPicture,
    src_x: i32,
    src_y: i32,
    mv_own: [i32; 2],
    mv_above: [i32; 2],
    mv_below: [i32; 2],
    mv_left: [i32; 2],
    mv_right: [i32; 2],
    rcontrol: bool,
) -> [u8; 64] {
    let mut out = [0u8; 64];
    for j in 0..8usize {
        let mv_vert = if j < 4 { mv_above } else { mv_below };
        for i in 0..8usize {
            let mv_horiz = if i < 4 { mv_left } else { mv_right };
            let x = src_x + i32::try_from(i).unwrap_or(0);
            let y = src_y + i32::try_from(j).unwrap_or(0);
            let q = sample_half_pel(refp, 0, x, y, mv_own[0], mv_own[1], rcontrol);
            let rr = sample_half_pel(refp, 0, x, y, mv_vert[0], mv_vert[1], rcontrol);
            let s = sample_half_pel(refp, 0, x, y, mv_horiz[0], mv_horiz[1], rcontrol);
            let p = obmc_combine(
                q,
                rr,
                s,
                obmc_weight(&OBMC_H0, j, i),
                obmc_weight(&OBMC_H1, j, i),
                obmc_weight(&OBMC_H2, j, i),
            );
            if let Some(slot) = out.get_mut(j * 8 + i) {
                *slot = p;
            }
        }
    }
    out
}

/// The per-pixel half of Annex F's own equation, `P(x,y) = (q*H0 + r*H1 +
/// s*H2 + 4) / 8`, factored out from [`annex_f_obmc_luma_block`]'s
/// reference-sampling loop so it is testable on plain `u8` inputs
/// without needing a [`RefPicture`].
#[allow(
    clippy::integer_division,
    reason = "Annex F's own literal formula (`Vaco-Spec-Ref: itu-t-h263` F.3) — add-then-truncate for a divisor of 8, the same rounding convention `avg2`/`avg4` already use"
)]
fn obmc_combine(q: u8, r: u8, s: u8, h0: i32, h1: i32, h2: i32) -> u8 {
    let p = (i32::from(q) * h0 + i32::from(r) * h1 + i32::from(s) * h2 + 4) / 8;
    p.clamp(0, 255) as u8
}

/// `b = (A + B + 1) / 2` — round to nearest, ties away from zero, which
/// for a non-negative sum is simply "round half up".
#[allow(
    clippy::integer_division,
    reason = "`b = c = (A + B + 1 - RCONTROL) / 2` is the literal formula (H.263 Figure 13) — the add-then-truncate form is round-to-nearest for a divisor of 2 when RCONTROL == 0, or plain truncation when RCONTROL == 1, not an approximation of either"
)]
fn avg2(a: u8, b: u8, rcontrol: bool) -> u8 {
    // `Vaco-Spec-Ref: itu-t-h263` 6.1.2, Figure 13: `b = c = (A + B + 1 -
    // RCONTROL) / 2`. `RCONTROL == 0` (every non-PLUSPTYPE picture, and
    // any PLUSPTYPE one with RTYPE off) is the round-up-on-ties case this
    // crate always used before RTYPE was read at all; `RCONTROL == 1`
    // truncates instead, matching the encoder's own rounding-alternation
    // recommendation (5.1.4.3's own text) for its reference picture.
    let bias = u16::from(!rcontrol);
    ((u16::from(a) + u16::from(b) + bias) / 2) as u8
}

#[allow(
    clippy::integer_division,
    reason = "`d = (A + B + C + D + 2 - RCONTROL) / 4` is the literal formula (H.263 Figure 13 / H.261 §3.2.1's identical bilinear scheme) — the add-then-truncate form is round-to-nearest for a divisor of 4 (or plain truncation when RCONTROL == 1), not an approximation"
)]
fn avg4(a: u8, b: u8, c: u8, d: u8, rcontrol: bool) -> u8 {
    let bias: u16 = if rcontrol { 1 } else { 2 };
    ((u16::from(a) + u16::from(b) + u16::from(c) + u16::from(d) + bias) / 4) as u8
}

/// H.261 §3.2.3's optional loop filter (`FIL`): separable 2-D, taps
/// `1/4, 1/2, 1/4` except at a block edge (taps `0, 1, 0` there), full
/// precision kept until the final rounding (`+2 then /4`, ties rounded
/// up). Operates on any block size the caller passes (this crate only
/// ever calls it with an 8x8 block), since the tap rule only depends on
/// being at an edge or not.
#[allow(
    clippy::integer_division,
    reason = "the 2-D filter's own final step, H.261 §3.2.3, is defined as '+2 then /4' — round-to-nearest for a divisor of 4 (ties round up), not a truncating approximation"
)]
pub(crate) fn h261_loop_filter(block: &mut [u8], w: usize, h: usize) {
    let wi = i32::try_from(w).unwrap_or(0);
    let hi = i32::try_from(h).unwrap_or(0);
    let get_at = |buf: &[i32], xx: i32, yy: i32| -> i32 {
        let xx = usize::try_from(xx.clamp(0, wi - 1)).unwrap_or(0);
        let yy = usize::try_from(yy.clamp(0, hi - 1)).unwrap_or(0);
        buf.get(yy * w + xx).copied().unwrap_or(0)
    };

    let src: Vec<i32> = block.iter().map(|&b| i32::from(b)).collect();
    let mut tmp = vec![0i32; w * h];
    for y in 0..hi {
        for x in 0..wi {
            let v = if x == 0 || x == wi - 1 {
                get_at(&src, x, y)
            } else {
                (get_at(&src, x - 1, y) + 2 * get_at(&src, x, y) + get_at(&src, x + 1, y) + 2) / 4
            };
            if let Some(slot) =
                tmp.get_mut(usize::try_from(y).unwrap_or(0) * w + usize::try_from(x).unwrap_or(0))
            {
                *slot = v;
            }
        }
    }
    for x in 0..wi {
        for y in 0..hi {
            let v = if y == 0 || y == hi - 1 {
                get_at(&tmp, x, y)
            } else {
                (get_at(&tmp, x, y - 1) + 2 * get_at(&tmp, x, y) + get_at(&tmp, x, y + 1) + 2) / 4
            };
            if let Some(slot) =
                block.get_mut(usize::try_from(y).unwrap_or(0) * w + usize::try_from(x).unwrap_or(0))
            {
                *slot = v.clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umv_legacy_in_range_predictor_needs_no_wraparound() {
        // Predictor 10 (half-pel) is within [-31, 32]: §D.2's "first
        // column applies" case, identical to a plain sum.
        assert_eq!(h263_umv_vector_legacy(10, 5), 15);
        assert_eq!(h263_umv_vector_legacy(-20, -30), -50);
    }

    #[test]
    fn umv_legacy_out_of_range_predictor_matches_its_sign() {
        // Predictor 40 (half-pel) is outside [-31, 32] (positive side):
        // the result must land in [0, 63].
        let v = h263_umv_vector_legacy(40, -32);
        assert!(
            (0..=63).contains(&v),
            "expected sign-matched result, got {v}"
        );
        // Predictor -40: result must land in [-63, 0].
        let v = h263_umv_vector_legacy(-40, 31);
        assert!(
            (-63..=0).contains(&v),
            "expected sign-matched result, got {v}"
        );
    }

    #[test]
    fn umv_vector_plus_is_a_plain_sum() {
        assert_eq!(h263_umv_vector_plus(60, 10), 70);
        assert_eq!(h263_umv_vector_plus(-60, -10), -70);
    }

    #[test]
    fn avg2_rounds_up_on_ties_when_rcontrol_is_off() {
        // A=10, B=11: (10+11+1)/2 = 11 with RCONTROL off (this crate's
        // own behaviour before RTYPE was read at all).
        assert_eq!(avg2(10, 11, false), 11);
        // Same inputs, RCONTROL on: (10+11)/2 = 10, truncated.
        assert_eq!(avg2(10, 11, true), 10);
    }

    #[test]
    fn avg4_matches_figure_13s_two_bias_values() {
        // A+B+C+D = 10: RCONTROL off -> (10+2)/4 = 3; RCONTROL on ->
        // (10+1)/4 = 2 (truncated either way) — the one bias value that
        // actually crosses an integer boundary between the two rules.
        assert_eq!(avg4(2, 2, 3, 3, false), 3);
        assert_eq!(avg4(2, 2, 3, 3, true), 2);
    }

    #[test]
    fn obmc_weighting_tables_sum_to_eight_at_every_cell() {
        // Annex F's own rounding formula divides by 8, so a conforming
        // transcription of Figures F.2-F.4 must sum to exactly 8 at
        // every one of the 64 cells - a shape invariant independent of
        // which table is "H0" vs "H1" vs "H2", so this catches a
        // transcription slip a mere shape/plausibility check would miss.
        for j in 0..8 {
            for i in 0..8 {
                let sum = obmc_weight(&OBMC_H0, j, i)
                    + obmc_weight(&OBMC_H1, j, i)
                    + obmc_weight(&OBMC_H2, j, i);
                assert_eq!(sum, 8, "cell ({i}, {j}) summed to {sum}, not 8");
            }
        }
    }

    #[test]
    fn obmc_h0_matches_figure_f2s_corner_and_centre_values() {
        // Spot-check against the primary text directly, not just the
        // sum invariant: corners are 4, the four centre cells are 6,
        // everything else on the table is 5.
        assert_eq!(obmc_weight(&OBMC_H0, 0, 0), 4);
        assert_eq!(obmc_weight(&OBMC_H0, 0, 7), 4);
        assert_eq!(obmc_weight(&OBMC_H0, 7, 0), 4);
        assert_eq!(obmc_weight(&OBMC_H0, 7, 7), 4);
        assert_eq!(obmc_weight(&OBMC_H0, 3, 3), 6);
        assert_eq!(obmc_weight(&OBMC_H0, 3, 4), 6);
        assert_eq!(obmc_weight(&OBMC_H0, 4, 3), 6);
        assert_eq!(obmc_weight(&OBMC_H0, 4, 4), 6);
        assert_eq!(obmc_weight(&OBMC_H0, 0, 1), 5);
        assert_eq!(obmc_weight(&OBMC_H0, 1, 0), 5);
    }

    #[test]
    fn obmc_h1_and_h2_are_not_simple_transposes() {
        // Documented explicitly in `OBMC_H2`'s own comment: verify it,
        // not just assert it in prose. Row 1 of H1 and column 1 of H2
        // differ (1,1,2,2,2,2,1,1 vs 1,2,2,2,2,2,2,1) - the two tables
        // were independently transcribed from independently rendered
        // figures, not derived from one another.
        let h1_row1: Vec<i32> = (0..8).map(|i| obmc_weight(&OBMC_H1, 1, i)).collect();
        let h2_col1: Vec<i32> = (0..8).map(|j| obmc_weight(&OBMC_H2, j, 1)).collect();
        assert_ne!(h1_row1, h2_col1);
    }

    #[test]
    fn obmc_combine_reduces_to_the_shared_value_when_all_three_predictions_agree() {
        // q == r == s == v: (v*h0 + v*h1 + v*h2 + 4) / 8 == v exactly,
        // since h0+h1+h2 == 8 at every cell (the sum-to-eight invariant
        // tested above) - this is OBMC degenerating to plain motion
        // compensation when a block's own and all four remote vectors
        // happen to produce the same reference sample.
        for (h0, h1, h2) in [(4, 2, 2), (5, 1, 2), (6, 1, 1), (5, 2, 1)] {
            for v in [0u8, 1, 100, 200, 255] {
                assert_eq!(obmc_combine(v, v, v, h0, h1, h2), v);
            }
        }
    }

    #[test]
    fn obmc_combine_matches_a_hand_worked_corner_pixel() {
        // Corner (i=0, j=0): H0=4, H1=2, H2=2. q=100 (own), r=50 (vertical
        // remote), s=10 (horizontal remote): (100*4 + 50*2 + 10*2 + 4)/8
        // = (400+100+20+4)/8 = 524/8 = 65 (truncated).
        assert_eq!(obmc_combine(100, 50, 10, 4, 2, 2), 65);
    }

    #[test]
    fn annex_f_chroma_mv_matches_table_f1s_bucket_boundaries() {
        // total=0: base=0, sixteenths=0 -> bucket 0 -> result 0.
        assert_eq!(annex_f_chroma_mv([0, 0, 0, 0]), 0);
        // total=32 (four identical vectors of 8 half-pel each, i.e. a
        // whole 4-pixel luma shift): base=2, sixteenths=0 -> result 4 -
        // sanity-checks the /8 combination against a clean multiple.
        assert_eq!(annex_f_chroma_mv([8, 8, 8, 8]), 4);
        // sixteenths=2 -> bucket 0 (still snaps down).
        assert_eq!(annex_f_chroma_mv([2, 0, 0, 0]), 0);
        // sixteenths=3 -> bucket 1 (the boundary Table F.1 actually
        // draws: 2 snaps down, 3 snaps to the half-pixel bucket).
        assert_eq!(annex_f_chroma_mv([3, 0, 0, 0]), 1);
        // sixteenths=13 -> bucket 1, sixteenths=14 -> bucket 2 (the
        // other boundary).
        assert_eq!(annex_f_chroma_mv([13, 0, 0, 0]), 1);
        assert_eq!(annex_f_chroma_mv([14, 0, 0, 0]), 2);
    }

    #[test]
    fn annex_f_chroma_mv_handles_negative_totals_via_euclidean_division() {
        // total=-3: div_euclid(-3, 16) = -1, rem_euclid = 13 -> bucket 1
        // -> result 2*(-1)+1 = -1, not the sign-following (-3/16 ~ 0,
        // remainder -3) a plain truncating division would give.
        assert_eq!(annex_f_chroma_mv([-3, 0, 0, 0]), -1);
    }

    #[test]
    fn annex_f_block0_matches_the_base_single_vector_rule() {
        // F.2's own text: block 0 equals §6.1.1/Figure 12 exactly — left,
        // above, above-*macroblock*-right (two grid columns over, not
        // one: the grid cell one column over is still inside the
        // directly-above macroblock).
        assert_eq!(annex_f_predictor_sources(0), [(-1, 0), (0, -1), (2, -1)]);
    }

    #[test]
    fn annex_f_blocks_1_and_2_share_the_uniform_fine_grid_rule() {
        // Both use the plain left/above/above-right fine-grid rule
        // (Figure 12's own shape, one 8x8-block level finer) — for block
        // 1 this lands on an *internal* MV1 (offset (-1,0) reaches block
        // 0, inside the same macroblock) and *external* MV2/MV3; for
        // block 2 the same three offsets land on an *internal* MV2/MV3
        // (blocks 0 and 1) and an *external* MV1 — the reading that
        // corrects this project's own earlier "block 2 fully internal"
        // documentation error. The two blocks' offset triples are
        // identical; only their own base position differs, which is what
        // the caller adds them to.
        assert_eq!(annex_f_predictor_sources(1), [(-1, 0), (0, -1), (1, -1)]);
        assert_eq!(annex_f_predictor_sources(2), [(-1, 0), (0, -1), (1, -1)]);
    }

    #[test]
    fn annex_f_block3_is_fully_internal() {
        // All three offsets land inside the current macroblock: MV1 =
        // block 2, MV2 = block 0, MV3 = block 1 — none external.
        assert_eq!(annex_f_predictor_sources(3), [(-1, 0), (-1, -1), (0, -1)]);
    }
}
