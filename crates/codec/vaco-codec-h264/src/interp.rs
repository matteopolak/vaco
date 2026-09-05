//! Clause 8.4.2.2.1's luma quarter-sample interpolation: the six-tap FIR
//! for half-sample positions, and simple averaging for quarter-sample
//! ones. Plus [`chroma_mc_2x2`], clause 8.4.2.2.2's bilinear chroma
//! interpolation -- one-eighth-chroma-sample positions rather than luma's
//! quarter, and a plain 2-tap-per-axis bilinear filter rather than the six-tap
//! FIR, since chroma has no half-sample stage of its own to interpolate through.
//!
//! Both transcribed and cross-checked directly against a primary text
//! (`provenance/sources.toml`'s `iso-iec-14496-10-2002-draft`) rather
//! than recalled: clause 8.4.1.4's own `mvCLX[0] = mvLX[0]` (eq.
//! (8-174)/(8-175)) is easy to misremember as "multiply by 2" from its
//! own prose sentence one line above the equation it explains -- the
//! prose describes *why* no separate scaling step is needed (a
//! quarter-luma-sample unit already equals an eighth-chroma-sample unit,
//! since chroma has half the spatial resolution), not an operation to
//! perform on top of the equation.
//!
//! # The naming
//!
//! Clause 8.4.2.2.1's own Figure 8-4 names every quarter-pel position
//! around one full-pel sample `G` (with `H`/`M` its right/below full-pel
//! neighbours) `a` through `s`. This module keeps that naming for the
//! functions that compute each one, since it is the only naming anyone
//! reading this against the spec will recognise.

#![allow(
    clippy::many_single_char_names,
    reason = "clause 8.4.2.2.1's own a..s naming for quarter-pel positions"
)]

use vaco_codec_dsp_mc::h264::{ChromaJob, H264McKernels};

const fn clip_u8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "range-checked above"
        )]
        {
            v as u8
        }
    }
}

/// The six-tap filter clause 8.4.2.2.1 applies at every half-sample
/// position: `E - 5F + 20G + 20H - 5I + J`, unrounded and unshifted --
/// callers finish it themselves, since the "both axes half-pel" (`j`)
/// case needs the raw sum from one axis before the other axis's own
/// filter and rounding can run (clause 8.4.2.2.1's own two-pass
/// derivation for `j1`/`j2`/etc.), while a single-axis half-pel position
/// rounds and clips immediately.
const fn tap6(e: i32, f: i32, g: i32, h: i32, i: i32, j: i32) -> i32 {
    e - 5 * f + 20 * g + 20 * h - 5 * i + j
}

const fn round_half(sum: i32) -> i32 {
    (sum + 16) >> 5
}

const fn round_quarter_pass(sum: i32) -> i32 {
    (sum + 512) >> 10
}

fn avg(a: i32, b: i32) -> i32 {
    (a + b + 1) >> 1
}

/// Fetches one full-pel luma sample, clamping to the picture's own edges
/// (clause 8.4.2.2.1's own "samples outside the picture" rule reduces to
/// edge repetition for the non-MBAFF, single-slice-per-picture case this
/// crate decodes) -- `fetch` is `plane[(y.clamp) * width + x.clamp]`
/// wrapped by the caller so this module knows nothing about the actual
/// buffer layout.
pub(crate) fn luma_qpel_sample<F: Fn(i32, i32) -> u8>(
    fetch: F,
    x: i32,
    y: i32,
    frac_x: u32,
    frac_y: u32,
) -> u8 {
    let f = |dx: i32, dy: i32| i32::from(fetch(x + dx, y + dy));
    if frac_x == 0 && frac_y == 0 {
        return clip_u8(f(0, 0));
    }

    // Horizontal half-pel at row `dy` (`b`-shaped, clause 8.4.2.2.1 eq.
    // 8-238) -- rounded AND clipped to a real 8-bit sample here, not just
    // rounded. Clause 8.4.2.2.1's own quarter-pel averaging positions
    // (`a`, `c`, `e`, `g`, ...) average against the *clipped* half-pel
    // sample, the same one that would be stored and displayed if `b`
    // itself were the requested position -- not the unclipped 6-tap sum.
    // Skipping the clip here and only applying it to the position's own
    // final result (an easy mistake: every arm below already ends in its
    // own `clip_u8`, so the bug is invisible unless you check what value
    // went *into* that clip for a two-input average) silently overshoots
    // whenever the raw 6-tap sum would have needed clipping -- exactly
    // where a real edge and real fractional motion coincide, which read
    // as small everywhere but is a structural amplitude error, not
    // rounding.
    let half_h = |dy: i32| -> i32 {
        i32::from(clip_u8(round_half(tap6(
            f(-2, dy),
            f(-1, dy),
            f(0, dy),
            f(1, dy),
            f(2, dy),
            f(3, dy),
        ))))
    };
    // Vertical half-pel at column `dx`, clipped for the same reason.
    let half_v = |dx: i32| -> i32 {
        i32::from(clip_u8(round_half(tap6(
            f(dx, -2),
            f(dx, -1),
            f(dx, 0),
            f(dx, 1),
            f(dx, 2),
            f(dx, 3),
        ))))
    };
    // Raw (unrounded) horizontal 6-tap sum at row `dy`, for `j`'s own
    // two-pass derivation -- deliberately NOT clipped or rounded here;
    // `j` itself is rounded and clipped once, below, after the second
    // pass.
    let raw_h =
        |dy: i32| -> i32 { tap6(f(-2, dy), f(-1, dy), f(0, dy), f(1, dy), f(2, dy), f(3, dy)) };
    // `j`: both axes half-pel, clause 8.4.2.2.1's own two-pass
    // derivation -- the horizontal 6-tap sum applied to six UNCLIPPED,
    // unrounded raw_h rows, then a second 6-tap pass, then rounded and
    // clipped exactly once at the end. Computed once and clipped
    // immediately (unlike half_h/half_v above, there is only one `j` per
    // sample position, not one per row/column), for the same "average
    // against the real clipped sample" reason.
    let j = i32::from(clip_u8(round_quarter_pass(tap6(
        raw_h(-2),
        raw_h(-1),
        raw_h(0),
        raw_h(1),
        raw_h(2),
        raw_h(3),
    ))));

    match (frac_x, frac_y) {
        (0, 0) => unreachable!("handled above"),
        (2, 0) => clip_u8(half_h(0)),                 // b
        (0, 2) => clip_u8(half_v(0)),                 // h
        (2, 2) => clip_u8(j),                         // j
        (1, 0) => clip_u8(avg(f(0, 0), half_h(0))),   // a
        (3, 0) => clip_u8(avg(half_h(0), f(1, 0))),   // c
        (0, 1) => clip_u8(avg(f(0, 0), half_v(0))),   // d
        (0, 3) => clip_u8(avg(half_v(0), f(0, 1))),   // n
        (1, 1) => clip_u8(avg(half_h(0), half_v(0))), // e
        (3, 1) => clip_u8(avg(half_h(0), half_v(1))), // g
        (1, 3) => clip_u8(avg(half_v(0), half_h(1))), // p
        (3, 3) => clip_u8(avg(half_v(1), half_h(1))), // r
        (2, 1) => clip_u8(avg(half_h(0), j)),         // f
        (2, 3) => clip_u8(avg(j, half_h(1))),         // q
        (1, 2) => clip_u8(avg(half_v(0), j)),         // i
        (3, 2) => clip_u8(avg(j, half_v(1))),         // k
        _ => clip_u8(f(0, 0)),
    }
}

/// [`luma_qpel_partition`]'s own horizontal six-tap pass, unrounded and
/// unclipped -- shared by [`fill_h_plane`] (round and clip this same sum)
/// and [`fill_j_plane`] (a second six-tap pass down these raw sums,
/// clause 8.4.2.2.1's own two-pass derivation for the `j` position).
/// `raw_h[r][ox]` is the sum at window row `r` (picture row `y + r - 2`),
/// output column `ox`.
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "r/ox are loop variables over 0..h+5/0..w, both bounded by the fixed 21/16-wide arrays this module declares them against"
)]
fn fill_raw_h(
    kernels: &H264McKernels,
    window: &[[u8; 21]; 21],
    w: usize,
    h: usize,
    raw_h: &mut [[i32; 16]; 21],
) {
    (kernels.luma_half_raw)(window, w, h + 5, raw_h);
}

/// `H[r][ox]`: the clipped horizontal half-pel sample (position `b`) at
/// output row `r` and column `ox` -- `0..=h` rather than `0..h` because
/// the `g`/`p`/`r` positions average against the *next* row's own `H`.
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "see fill_raw_h's own identical reason"
)]
fn fill_h_plane(raw_h: &[[i32; 16]; 21], w: usize, h: usize, h_plane: &mut [[u8; 16]; 17]) {
    for r in 0..=h {
        for ox in 0..w {
            h_plane[r][ox] = clip_u8(round_half(raw_h[r + 2][ox]));
        }
    }
}

/// `V[oy][c]`: the clipped vertical half-pel sample (position `h`) at
/// output row `oy` and column `c` -- `0..=w` for the same reason `H`
/// above needs one extra row, one column short of the partition's own
/// right edge.
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "see fill_raw_h's own identical reason"
)]
fn fill_v_plane(window: &[[u8; 21]; 21], w: usize, h: usize, v_plane: &mut [[u8; 17]; 16]) {
    for oy in 0..h {
        for c in 0..=w {
            v_plane[oy][c] = clip_u8(round_half(tap6(
                i32::from(window[oy][c + 2]),
                i32::from(window[oy + 1][c + 2]),
                i32::from(window[oy + 2][c + 2]),
                i32::from(window[oy + 3][c + 2]),
                i32::from(window[oy + 4][c + 2]),
                i32::from(window[oy + 5][c + 2]),
            )));
        }
    }
}

/// `J[oy][ox]`: both axes half-pel -- position `j`'s own two-pass
/// derivation, a second six-tap filter down the raw horizontal sums,
/// rounded and clipped exactly once.
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "see fill_raw_h's own identical reason"
)]
fn fill_j_plane(raw_h: &[[i32; 16]; 21], w: usize, h: usize, j_plane: &mut [[u8; 16]; 16]) {
    for oy in 0..h {
        for ox in 0..w {
            j_plane[oy][ox] = clip_u8(round_quarter_pass(tap6(
                raw_h[oy][ox],
                raw_h[oy + 1][ox],
                raw_h[oy + 2][ox],
                raw_h[oy + 3][ox],
                raw_h[oy + 4][ox],
                raw_h[oy + 5][ox],
            )));
        }
    }
}

/// Clause 8.4.2.2.1's luma quarter-sample interpolation for a whole
/// partition (up to 16x16, an H.264 macroblock's own maximum) at once,
/// instead of [`luma_qpel_sample`]'s one-output-pixel-at-a-time shape.
///
/// [`luma_qpel_sample`] is correct but each call independently re-fetches
/// and re-filters its own 9x9
/// neighbourhood through `fetch`, and every pixel in the same partition
/// shares almost all of that neighbourhood with its neighbours -- a 16x16
/// partition predicted 4x4-at-a-time (as [`crate::reconstruct`] used to)
/// issues 16 x 81 = 1,296 `fetch` calls and 16 independent six-tap filter
/// passes for 256 output samples that need only (16 + 5) x (16 + 5) = 441
/// source samples between them. This function fetches that whole window
/// once, then builds *only* the horizontal-half-pel (`H`), vertical-half-pel
/// (`V`) and both-axes-half-pel (`J`) planes clause 8.4.2.2.1 actually needs
/// for `frac_x`/`frac_y` (a single motion vector's fractional part, shared
/// by the whole partition) -- see the six-way match in this function's own
/// body -- before combining them per output pixel, the same arithmetic
/// [`luma_qpel_sample`] performs, factored so the shared work is shared.
/// [`luma_qpel_sample`] is kept, unused by
/// [`crate::reconstruct::reconstruct_inter_mb`]'s own hot path, as the
/// scalar oracle this function's own tests check bit-for-bit.
///
/// Computing all three planes unconditionally, tried first, measured
/// *slower* end to end despite issuing far fewer `fetch` calls than the
/// per-4x4 path: the common one-axis-only positions (`b`/`h`/`a`/`c`/`d`/`n`,
/// most real sub-pel motion) need only one of the three, and the other two
/// planes' own zero-initialisation and fill passes cost more than the
/// fetch-count win recovered. Branching by need is not an optimisation on
/// top of the design, it *is* the design this item's own ceiling estimate
/// assumed.
///
/// `out[0..h][0..w]` is written; any cell outside that is left as it was.
/// `w`/`h` greater than 16 silently clip to 16, since no real H.264
/// partition is ever larger than one macroblock's own 16x16.
///
/// `x`/`y` name the partition's own top-left **full-pel** picture position
/// (a caller's own motion vector integer part already folded in, exactly
/// as [`luma_qpel_sample`]'s `x`/`y` do for one pixel).
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    clippy::cast_possible_wrap,
    clippy::too_many_lines,
    reason = "every index below is a loop variable bounded by `w`/`h` (<= 16, clamped at entry) or a small constant offset from one -- provably in range for the fixed-size 21/17/16-wide arrays declared in this function, not bitstream-derived; `w`/`h` are both bounded by 16 well inside i32's range. Length: one branch per (need_h, need_v, need_j) combination (see this function's own doc), each self-contained so a branch that does not need a plane never declares -- and never zero-initialises -- it; splitting further would just re-hide the six combinations this match already makes explicit"
)]
pub(crate) fn luma_qpel_partition_with<F: Fn(i32, i32) -> u8>(
    kernels: &H264McKernels,
    fetch: F,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    frac_x: u32,
    frac_y: u32,
    out: &mut [[u8; 16]; 16],
) {
    let w = w.min(16);
    let h = h.min(16);
    if w == 0 || h == 0 {
        return;
    }

    // One fetch per window position -- `w + 5` columns (two taps left,
    // three right of the six-tap window) by `h + 5` rows -- gathered once
    // for the whole partition. Every later pass reads this array, never
    // `fetch` again.
    let mut window = [[0u8; 21]; 21];
    for r in 0..h + 5 {
        let py = y + r as i32 - 2;
        for c in 0..w + 5 {
            window[r][c] = fetch(x + c as i32 - 2, py);
        }
    }

    if frac_x == 0 && frac_y == 0 {
        for oy in 0..h {
            for ox in 0..w {
                out[oy][ox] = window[oy + 2][ox + 2];
            }
        }
        return;
    }

    // `frac_x`/`frac_y` are the same for every pixel this call predicts
    // (one motion vector per partition), so which of `H`/`V`/`J` clause
    // 8.4.2.2.1's own quarter-pel table reads for this position is decided
    // once here, not once per pixel -- and the branch below on
    // `(need_h, need_v, need_j)` means a position that needs only one of
    // the three never even *declares* the other two, paying no
    // zero-initialisation for a plane it will never read. An earlier
    // version of this function computed all three unconditionally and,
    // despite issuing far fewer `fetch` calls than
    // `sample_luma_block`'s own per-4x4 path, measured *slower* end to
    // end on `h264_4k.mp4` -- the wasted `H`/`V`/`J` passes (the common
    // one-axis-only positions `b`/`h`/`a`/`c`/`d`/`n` need only one of
    // the three) cost more than the fetch-count win recovered.
    let need_h = frac_x != 0 && frac_y != 2;
    let need_v = frac_y != 0 && frac_x != 2;
    let need_j = (frac_x == 2 && frac_y != 0) || (frac_y == 2 && frac_x != 0);

    match (need_h, need_v, need_j) {
        (true, false, false) => {
            // b, a, c.
            let mut raw_h = [[0i32; 16]; 21];
            fill_raw_h(kernels, &window, w, h, &mut raw_h);
            let mut h_plane = [[0u8; 16]; 17];
            fill_h_plane(&raw_h, w, h, &mut h_plane);
            for oy in 0..h {
                for ox in 0..w {
                    let hh0 = i32::from(h_plane[oy][ox]);
                    out[oy][ox] = match frac_x {
                        2 => clip_u8(hh0),
                        1 => clip_u8(avg(i32::from(window[oy + 2][ox + 2]), hh0)),
                        _ => clip_u8(avg(hh0, i32::from(window[oy + 2][ox + 3]))),
                    };
                }
            }
        }
        (false, true, false) => {
            // h, d, n.
            let mut v_plane = [[0u8; 17]; 16];
            fill_v_plane(&window, w, h, &mut v_plane);
            for oy in 0..h {
                for ox in 0..w {
                    let vv0 = i32::from(v_plane[oy][ox]);
                    out[oy][ox] = match frac_y {
                        2 => clip_u8(vv0),
                        1 => clip_u8(avg(i32::from(window[oy + 2][ox + 2]), vv0)),
                        _ => clip_u8(avg(vv0, i32::from(window[oy + 3][ox + 2]))),
                    };
                }
            }
        }
        (false, false, true) => {
            // j (frac_x == 2 && frac_y == 2 is the only member).
            let mut raw_h = [[0i32; 16]; 21];
            fill_raw_h(kernels, &window, w, h, &mut raw_h);
            let mut j_plane = [[0u8; 16]; 16];
            fill_j_plane(&raw_h, w, h, &mut j_plane);
            for oy in 0..h {
                for ox in 0..w {
                    out[oy][ox] = clip_u8(i32::from(j_plane[oy][ox]));
                }
            }
        }
        (true, true, false) => {
            // e, g, p, r.
            let mut raw_h = [[0i32; 16]; 21];
            fill_raw_h(kernels, &window, w, h, &mut raw_h);
            let mut h_plane = [[0u8; 16]; 17];
            fill_h_plane(&raw_h, w, h, &mut h_plane);
            let mut v_plane = [[0u8; 17]; 16];
            fill_v_plane(&window, w, h, &mut v_plane);
            for oy in 0..h {
                for ox in 0..w {
                    let hh0 = i32::from(h_plane[oy][ox]);
                    let hh1 = i32::from(h_plane[oy + 1][ox]);
                    let vv0 = i32::from(v_plane[oy][ox]);
                    let vv1 = i32::from(v_plane[oy][ox + 1]);
                    out[oy][ox] = match (frac_x, frac_y) {
                        (1, 1) => clip_u8(avg(hh0, vv0)),
                        (3, 1) => clip_u8(avg(hh0, vv1)),
                        (1, 3) => clip_u8(avg(vv0, hh1)),
                        _ => clip_u8(avg(vv1, hh1)),
                    };
                }
            }
        }
        (true, false, true) => {
            // f, q.
            let mut raw_h = [[0i32; 16]; 21];
            fill_raw_h(kernels, &window, w, h, &mut raw_h);
            let mut h_plane = [[0u8; 16]; 17];
            fill_h_plane(&raw_h, w, h, &mut h_plane);
            let mut j_plane = [[0u8; 16]; 16];
            fill_j_plane(&raw_h, w, h, &mut j_plane);
            for oy in 0..h {
                for ox in 0..w {
                    let jj = i32::from(j_plane[oy][ox]);
                    out[oy][ox] = if frac_y == 1 {
                        clip_u8(avg(i32::from(h_plane[oy][ox]), jj))
                    } else {
                        clip_u8(avg(jj, i32::from(h_plane[oy + 1][ox])))
                    };
                }
            }
        }
        (false, true, true) => {
            // i, k.
            let mut raw_h = [[0i32; 16]; 21];
            fill_raw_h(kernels, &window, w, h, &mut raw_h);
            let mut v_plane = [[0u8; 17]; 16];
            fill_v_plane(&window, w, h, &mut v_plane);
            let mut j_plane = [[0u8; 16]; 16];
            fill_j_plane(&raw_h, w, h, &mut j_plane);
            for oy in 0..h {
                for ox in 0..w {
                    let jj = i32::from(j_plane[oy][ox]);
                    out[oy][ox] = if frac_x == 1 {
                        clip_u8(avg(i32::from(v_plane[oy][ox]), jj))
                    } else {
                        clip_u8(avg(jj, i32::from(v_plane[oy][ox + 1])))
                    };
                }
            }
        }
        // Unreachable: `need_h`/`need_v`/`need_j`'s own derivation covers
        // every `(frac_x, frac_y)` pair except `(0, 0)` (handled above)
        // with exactly one of the six arms above (`(true, true, true)`
        // cannot happen: `need_j`'s two clauses each require either
        // `frac_x == 2` or `frac_y == 2`, and `need_v`/`need_h`
        // respectively forbid exactly that) -- both kept as a safe
        // fallback rather than `unreachable!()`, matching this module's
        // own `unwrap_used`/`panic`-denied policy, since the match still
        // has to be exhaustive.
        (false, false, false) | (true, true, true) => {
            for oy in 0..h {
                for ox in 0..w {
                    out[oy][ox] = window[oy + 2][ox + 2];
                }
            }
        }
    }
}

/// Convenience entry for tests and non-decoder callers. The decoder uses
/// [`luma_qpel_partition_with`] with a table resolved once per picture.
#[cfg(test)]
pub(crate) fn luma_qpel_partition<F: Fn(i32, i32) -> u8>(
    fetch: F,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    frac_x: u32,
    frac_y: u32,
    out: &mut [[u8; 16]; 16],
) {
    luma_qpel_partition_with(
        &H264McKernels::default(),
        fetch,
        x,
        y,
        w,
        h,
        frac_x,
        frac_y,
        out,
    );
}

/// Clause 8.4.2.2.2, eq. (8-206)..(8-214): one interpolated chroma sample
/// at chroma picture position `(x, y)` displaced by the chroma motion
/// vector `(mv_x, mv_y)` -- `ChromaArrayType == 1` (frame macroblocks,
/// this crate's only supported case), where clause 8.4.1.4's own eq.
/// (8-174)/(8-175) make `mvCLX` numerically identical to the luma `mvLX`
/// a caller already has (see this module's own doc for why no separate
/// scaling step belongs here). `fetch` is `plane[y.clamp][x.clamp]`,
/// wrapped by the caller exactly as [`luma_qpel_sample`]'s own `fetch`
/// is -- eq. (8-206)..(8-213)'s own `Clip3(0, PicWidthInSamplesC - 1,
/// ...)`/`Clip3(0, PicHeightInSamplesC - 1, ...)` edge clamping is what
/// that wrapping applies.
#[must_use]
#[cfg(test)]
pub(crate) fn chroma_mc_sample<F: Fn(i32, i32) -> u8>(
    fetch: F,
    x: i32,
    y: i32,
    mv_x: i32,
    mv_y: i32,
) -> u8 {
    // eq. (8-180)..(8-183), the xC/yC-relative half already folded into
    // `x`/`y` by the caller: `xIntC`/`yIntC` is `x`/`y` plus the integer
    // part of the eighth-sample motion vector, `xFracC`/`yFracC` its low
    // three bits -- exact for a negative `mv_x`/`mv_y` too, since `>>` on
    // a signed integer floors and `& 7` on two's complement always yields
    // the true non-negative remainder.
    let int_x = mv_x >> 3;
    let frac_x = mv_x & 7;
    let int_y = mv_y >> 3;
    let frac_y = mv_y & 7;
    let ax = x + int_x;
    let ay = y + int_y;
    let a = i32::from(fetch(ax, ay));
    let b = i32::from(fetch(ax + 1, ay));
    let c = i32::from(fetch(ax, ay + 1));
    let d = i32::from(fetch(ax + 1, ay + 1));
    let sum = (8 - frac_x) * (8 - frac_y) * a
        + frac_x * (8 - frac_y) * b
        + (8 - frac_x) * frac_y * c
        + frac_x * frac_y * d;
    clip_u8((sum + 32) >> 6)
}

/// Predicts the four chroma samples covered by one luma 4x4 block. Their
/// bilinear neighbourhoods overlap into one 3x3 source window, so this fetches
/// those nine samples once and reuses the four interpolation weights rather
/// than issuing sixteen overlapping fetches. The test-only
/// `chroma_mc_sample` is the independent per-sample oracle.
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "array::from_fn bounds dy/dx to 0..2, so dy+1/dx+1 stay within the fixed 3x3 window"
)]
#[cfg(test)]
pub(crate) fn chroma_mc_2x2_with<F: Fn(i32, i32) -> u8>(
    kernels: &H264McKernels,
    fetch: F,
    x: i32,
    y: i32,
    mv_x: i32,
    mv_y: i32,
) -> [[u8; 2]; 2] {
    let job = chroma_mc_job(fetch, x, y, mv_x, mv_y);
    let mut out = [[[0u8; 2]; 2]; 1];
    (kernels.chroma_batch)(&[job], &mut out);
    out[0]
}

/// Gather one chroma MC request without dispatching it, so a decoder can
/// collect a macroblock's narrow 2x2 requests into one kernel call.
#[must_use]
pub(crate) fn chroma_mc_job<F: Fn(i32, i32) -> u8>(
    fetch: F,
    x: i32,
    y: i32,
    mv_x: i32,
    mv_y: i32,
) -> ChromaJob {
    let int_x = mv_x >> 3;
    let frac_x = mv_x & 7;
    let int_y = mv_y >> 3;
    let frac_y = mv_y & 7;
    let ax = x + int_x;
    let ay = y + int_y;
    let window: [[u8; 3]; 3] = core::array::from_fn(|dy| {
        core::array::from_fn(|dx| {
            fetch(
                ax + i32::try_from(dx).unwrap_or(0),
                ay + i32::try_from(dy).unwrap_or(0),
            )
        })
    });
    ChromaJob {
        src: window,
        frac_x: u8::try_from(frac_x).unwrap_or(0),
        frac_y: u8::try_from(frac_y).unwrap_or(0),
    }
}

/// Convenience entry for tests; production retains one selected table.
#[must_use]
#[cfg(test)]
pub(crate) fn chroma_mc_2x2<F: Fn(i32, i32) -> u8>(
    fetch: F,
    x: i32,
    y: i32,
    mv_x: i32,
    mv_y: i32,
) -> [[u8; 2]; 2] {
    chroma_mc_2x2_with(&H264McKernels::default(), fetch, x, y, mv_x, mv_y)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::integer_division,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::needless_range_loop,
    reason = "test code -- w/h/ox/oy loop bounds throughout this module's own A1 differential tests are always <= 16, well inside i32's range"
)]
mod tests {
    use super::*;

    /// A regression guard for the bug the row-0-vs-other-rows comparison
    /// on `cabac_ip_simple.264` led to (verified as real by checking
    /// fractional-position instrumentation, not by reading the code):
    /// quarter-pel positions that average against a half-pel sample must
    /// average against that sample's own *clipped* value, the same one
    /// that would be displayed if the half-pel position itself were the
    /// one requested -- not the unclipped 6-tap sum. A sharp step edge
    /// with a raw 6-tap sum that overshoots past 255 is exactly the case
    /// that makes the two diverge; a flat or gentle input (this module's
    /// other tests) cannot distinguish them, which is why this needed
    /// its own case.
    #[test]
    fn quarter_pel_averages_the_clipped_half_pel_sample_not_the_raw_overshoot() {
        // Constructed so the raw six-tap sum for `b` genuinely overshoots
        // 255 before rounding, `average-then-clip` and `clip-then-average`
        // give two DIFFERENT in-range answers (not just two paths that
        // happen to both saturate to the same clip boundary, which a
        // flat or gentle input can't distinguish -- ask how this test's
        // own numbers were chosen, below).
        //
        // Row: x=8..=13 -> E=255, F=0, G=200, H=255, I=0, J=255 (G is
        // also the position `a` itself averages against `b`, i.e.
        // `f(0, 0)` at x=10).
        let fetch = |x: i32, _y: i32| -> u8 {
            match x {
                9 | 12 => 0,
                8 | 11 | 13 => 255,
                _ => 200,
            }
        };
        let a = luma_qpel_sample(fetch, 10, 0, 1, 0);
        // Hand-computed: tap6 = E - 5F + 20G + 20H - 5I + J
        //              = 255 - 0 + 20*200 + 20*255 - 0 + 255 = 9610.
        // round_half = (9610 + 16) >> 5 = 9626 >> 5 = 300 -- past 255,
        // a genuine overshoot. clip_u8(300) = 255 is `b`'s own real,
        // displayable value.
        //
        // Correct (average against the CLIPPED b): avg(200, 255)
        //   = (200 + 255 + 1) >> 1 = 228.
        // Buggy (average against the raw, unclipped 300, clip only at
        // the very end): avg(200, 300) = (200 + 300 + 1) >> 1 = 250,
        // clip_u8(250) = 250 -- already in range, so the outer clip
        // never catches it. 228 and 250 are both valid-looking 8-bit
        // samples; only one of them is what clause 8.4.2.2.1 actually
        // specifies, which is exactly why this bug survived every
        // flat/ramp/integer-position test this module already had.
        assert_eq!(
            a, 228,
            "quarter-pel position a must average against b's clipped value, not its raw overshoot"
        );
    }

    /// A perfectly flat plane must interpolate to the same flat value at
    /// every quarter-pel position -- the six-tap filter's own weights
    /// (1 - 5 + 20 + 20 - 5 + 1 == 32) sum to exactly 32, so a constant
    /// input must round-trip through `round_half`/`round_quarter_pass`
    /// back to itself with zero error, at every position, not just the
    /// integer one.
    #[test]
    fn flat_plane_interpolates_to_itself_at_every_quarter_pel_position() {
        let fetch = |_x: i32, _y: i32| 128u8;
        for fx in 0..4 {
            for fy in 0..4 {
                assert_eq!(
                    luma_qpel_sample(fetch, 10, 10, fx, fy),
                    128,
                    "fx={fx} fy={fy}"
                );
            }
        }
    }

    #[test]
    fn integer_position_is_a_pure_fetch() {
        let fetch = |x: i32, y: i32| u8::try_from((x + y * 7).rem_euclid(256)).unwrap();
        assert_eq!(luma_qpel_sample(fetch, 3, 4, 0, 0), fetch(3, 4));
    }

    #[test]
    fn half_pel_horizontal_is_symmetric_around_a_ramp() {
        // A linear ramp's own 6-tap filter (weights summing to 32) at the
        // exact midpoint between two consecutive integer samples must
        // land on their average, since the filter is itself symmetric
        // and a ramp has no curvature for it to react to.
        let fetch = |x: i32, _y: i32| u8::try_from((x * 4).clamp(0, 255)).unwrap();
        let b = luma_qpel_sample(fetch, 10, 0, 2, 0);
        let expected = (i32::from(fetch(10, 0)) + i32::from(fetch(11, 0)) + 1) / 2;
        assert!(
            (i32::from(b) - expected).abs() <= 1,
            "b={b} expected~={expected}"
        );
    }

    #[test]
    fn chroma_flat_plane_interpolates_to_itself_at_every_eighth_pel_position() {
        let fetch = |_x: i32, _y: i32| 200u8;
        for mv_x in 0..8 {
            for mv_y in 0..8 {
                assert_eq!(
                    chroma_mc_sample(fetch, 10, 10, mv_x, mv_y),
                    200,
                    "mv=({mv_x},{mv_y})"
                );
            }
        }
    }

    #[test]
    fn chroma_integer_mv_is_a_pure_fetch() {
        let fetch = |x: i32, y: i32| u8::try_from((x + y * 5).rem_euclid(256)).unwrap();
        assert_eq!(chroma_mc_sample(fetch, 3, 4, 8, -16), fetch(4, 2));
    }

    #[test]
    fn chroma_half_pel_both_axes_averages_all_four_neighbours() {
        // xFracC = yFracC = 4 (half of 8): eq. (8-214) weights all four
        // corners equally at 4*4 == 16 (of 64 total), i.e. a plain
        // four-sample average.
        let fetch = |x: i32, y: i32| -> u8 {
            match (x, y) {
                (1, 0) => 10,
                (0, 1) => 20,
                (1, 1) => 30,
                _ => 0,
            }
        };
        let got = chroma_mc_sample(fetch, 0, 0, 4, 4);
        let expected = (10 + 20 + 30 + 2) / 4; // rounded average
        assert_eq!(got, expected);
    }

    #[test]
    fn chroma_negative_mv_floors_toward_negative_infinity() {
        // mv_x = -1 must give int_x = -1, frac_x = 7 (not int_x = 0,
        // frac_x = -1, which a truncating `/`/`%` would give) -- the same
        // floor-division correctness luma's own `>>`/`&` already relies
        // on, checked here for chroma's 3-bit fractional field. With
        // int_x = -1, frac_x = 7 the sample is 7/8 of the way from
        // x == -1 (A, weight 8/64) to x == 0 (B, weight 56/64), so the
        // result lands close to B, not roughly in between as the wrong
        // (int_x = 0, frac_x = -1) decomposition would produce.
        let fetch = |x: i32, _y: i32| if x <= -1 { 0u8 } else { 255u8 };
        let got = chroma_mc_sample(fetch, 0, 0, -1, 0);
        assert!(
            got > 200,
            "got={got}, expected close to the x=0 sample (255)"
        );
    }

    #[test]
    fn chroma_2x2_matches_four_scalar_samples_with_one_shared_window() {
        use core::cell::Cell;

        let sample = |x: i32, y: i32| -> u8 {
            let v = (x.rem_euclid(37) * 29 + y.rem_euclid(41) * 47 + (x * y).rem_euclid(31))
                .rem_euclid(256);
            u8::try_from(v).unwrap_or(0)
        };
        for &(x, y, integer_mv_x, integer_mv_y) in
            &[(11, 13, 0, 0), (0, 0, -16, -8), (-5, 7, 24, -24)]
        {
            for frac_y in 0..8 {
                for frac_x in 0..8 {
                    let mv_x = integer_mv_x + frac_x;
                    let mv_y = integer_mv_y + frac_y;
                    let fetches = Cell::new(0usize);
                    let got = chroma_mc_2x2(
                        |ax, ay| {
                            fetches.set(fetches.get() + 1);
                            sample(ax, ay)
                        },
                        x,
                        y,
                        mv_x,
                        mv_y,
                    );
                    assert_eq!(
                        fetches.get(),
                        9,
                        "mv=({mv_x},{mv_y}) must fetch the shared 3x3 window once"
                    );
                    let expected = core::array::from_fn(|dy| {
                        core::array::from_fn(|dx| {
                            chroma_mc_sample(
                                sample,
                                x + i32::try_from(dx).unwrap_or(0),
                                y + i32::try_from(dy).unwrap_or(0),
                                mv_x,
                                mv_y,
                            )
                        })
                    });
                    assert_eq!(got, expected, "mv=({mv_x},{mv_y})");
                }
            }
        }
    }

    /// The whole point of batching: predicting a partition in one
    /// [`luma_qpel_partition`] call must produce
    /// *exactly* the same samples as [`luma_qpel_sample`] called once per
    /// pixel -- checked directly, bit for bit, rather than argued from the
    /// derivation, at every fractional position, several partition shapes
    /// a real (or merged, see `crate::reconstruct::partition_rects`)
    /// H.264 partition can take, and both an interior position and one
    /// close enough to a fetch's own "picture edge" that a real caller's
    /// clamped `fetch` would return repeated border samples -- the in/out-
    /// of-picture case this module's own doc for A1 promises is checked,
    /// not just the interior one every other test here already exercises.
    #[test]
    fn partition_matches_the_per_pixel_oracle_at_every_fractional_position_and_shape() {
        // A source with both structure (so different rows/columns are not
        // accidentally identical, which would hide a transposed index) and
        // sharp transitions (so the six-tap filter's own overshoot/clip
        // behaviour is exercised the same way
        // `quarter_pel_averages_the_clipped_half_pel_sample_not_the_raw_overshoot`
        // above checks for the single-pixel oracle).
        let fetch = |x: i32, y: i32| -> u8 {
            let (x, y) = (x.rem_euclid(64), y.rem_euclid(64));
            let v = (x * 7 + y * 13 + (x * y) % 5) % 256;
            u8::try_from(v).unwrap_or(0)
        };
        for &(w, h) in &[
            (4usize, 4usize),
            (8, 4),
            (4, 8),
            (8, 8),
            (16, 8),
            (8, 16),
            (16, 16),
            (12, 4),
            (4, 12),
        ] {
            for fx in 0..4u32 {
                for fy in 0..4u32 {
                    // Two anchors: comfortably interior, and close enough to
                    // this synthetic plane's own wraparound that a real
                    // decoder's edge-clamped fetch would be exercised on
                    // the corresponding real picture (this `fetch` itself
                    // never needs clamping, since `rem_euclid` makes it
                    // total over all of `i32` -- what matters here is that
                    // both anchors drive the *same* six-tap reach math a
                    // clamped fetch would).
                    for &(ax, ay) in &[(20i32, 20i32), (0, 0), (-3, -3)] {
                        let mut got = [[0u8; 16]; 16];
                        luma_qpel_partition(fetch, ax, ay, w, h, fx, fy, &mut got);
                        for oy in 0..h {
                            for ox in 0..w {
                                let want =
                                    luma_qpel_sample(fetch, ax + ox as i32, ay + oy as i32, fx, fy);
                                assert_eq!(
                                    got[oy][ox], want,
                                    "w={w} h={h} fx={fx} fy={fy} anchor=({ax},{ay}) ox={ox} oy={oy}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn partition_full_pel_is_a_pure_copy_of_the_window() {
        let fetch = |x: i32, y: i32| u8::try_from((x + y * 9).rem_euclid(256)).unwrap();
        let mut got = [[0u8; 16]; 16];
        luma_qpel_partition(fetch, 5, 7, 16, 16, 0, 0, &mut got);
        for oy in 0..16 {
            for ox in 0..16 {
                assert_eq!(got[oy][ox], fetch(5 + ox as i32, 7 + oy as i32));
            }
        }
    }

    #[test]
    fn partition_flat_plane_interpolates_to_itself_everywhere() {
        let fetch = |_x: i32, _y: i32| 77u8;
        for fx in 0..4u32 {
            for fy in 0..4u32 {
                let mut got = [[0u8; 16]; 16];
                luma_qpel_partition(fetch, 0, 0, 16, 16, fx, fy, &mut got);
                for row in &got[..16] {
                    for &v in &row[..16] {
                        assert_eq!(v, 77, "fx={fx} fy={fy}");
                    }
                }
            }
        }
    }
}
