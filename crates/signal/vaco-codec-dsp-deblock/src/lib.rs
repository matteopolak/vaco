//! The in-loop deblocking filter shared across block-based video codecs
//! (H.264 clause 8.7; other codecs' equivalents can land here later the
//! same way `vaco-codec-dsp-idct` grew from H.264-only to four families).
//!
//! # Scope of this crate, and what stays in the caller
//!
//! This crate started scalar and correctness-first, deliberately shaped
//! around **edges and blocks rather than per-pixel callbacks** precisely so
//! a later masked-lane-select SIMD kernel (the technique #127 names) could
//! slot in behind [`filter_luma_line`]/[`filter_chroma_line`]'s own call
//! sites without changing the interface a caller programs against. That
//! shape paid off: `#619` added the primitive it was waiting on
//! (`vaco-simd::ops::select_i16`) and [`batch`] is that kernel, built
//! against the scalar functions as its own proptest oracle rather than
//! replacing them. `filter_luma_line`/`filter_chroma_line` stay exactly as
//! they were -- both the correctness reference and the tail handler for a
//! batch that is not a multiple of the native vector width.
//!
//! This crate computes **one edge's filtered samples given the samples and
//! QPs on both sides**. It does not:
//!
//! - Derive boundary strength from macroblock/coding-mode context (clause
//!   8.7.2.1's own neighbour-availability derivation) -- that needs a
//!   picture's real macroblock map, which belongs to the calling codec
//!   crate, the same split `vaco-codec-dsp-idct` draws by taking
//!   already-scaled coefficients rather than performing dequantisation
//!   itself.
//! - Walk a picture, decide filtering order, or know anything about
//!   `disable_deblocking_filter_idc` or slice boundaries.
//! - Touch chroma reconstruction at all beyond the filter equations
//!   themselves -- whether chroma is even reconstructed yet is a
//!   codec-crate question (`vaco-codec-h264`'s own `PictureBuffer` does
//!   not store chroma samples yet, so its own deblocking pass is
//!   luma-only for now, an explicit, narrower scope than this crate's).
//!
//! # Vectorised batch entry point
//!
//! `#619` closed the blocker this doc used to record: `vaco-simd` now has
//! masked-lane select at `i16` width (`ops::select_i16`), and [`batch`]
//! builds the vectorised kernel behind it -- [`batch::filter_luma_edge`]/
//! [`batch::filter_chroma_edge`] batch every line along one edge (16 for
//! luma, 8 for 4:2:0 chroma) rather than one line at a time, which is what
//! it takes to fill a vector register. See that module's own doc for the
//! technique (compute both `bS==4`/`bS<4` candidates, select per lane) and
//! for why this crate now depends on `vaco-simd`. [`filter_luma_line`]/
//! [`filter_chroma_line`] remain the scalar reference every batched result
//! is checked against, and the tail of any batch not a multiple of the
//! native vector width still falls back to them directly.
//!
//! # The two shapes: normal (`bS < 4`) and strong (`bS == 4`)
//!
//! Clause 8.7.2.1's own boundary-strength derivation feeds two entirely
//! different filtering processes: [`filter_luma_line`] and
//! [`filter_chroma_line`] dispatch between them internally based on `bs`,
//! so a caller never has to. `bs == 0` means "do not call this function at
//! all" -- both take a `NonZeroU8` in `1..=4` for exactly that reason,
//! making the no-op case unrepresentable rather than a silent early return
//! a caller could forget to check.

#![forbid(unsafe_code)]
// `#[inline(always)]` is not a tuning knob in `batch`'s kernel bodies: it is
// how a dispatched level's target-feature context reaches them (see
// `vaco-simd`'s own crate doc, "Authoring a kernel" step 2). A body that
// fails to inline is compiled at the ambient baseline -- still correct,
// silently slow, and invisible to every correctness test. Turned off once,
// at the root, rather than annotated onto every kernel function.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation in batch's kernel bodies; see crate docs"
)]

pub mod batch;
pub mod tables;

use core::num::NonZeroU8;

/// `Clip1_Y`/`Clip1_C`, clause 8.7.2.3's own clip to valid 8-bit sample
/// range -- both luma and chroma at 8-bit depth share this.
const fn clip1(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        // Bounds are checked above; this cast is exact by construction.
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

/// `Clip3(-c, c, v)`, used throughout clause 8.7.2.3/8.7.2.4 to bound a
/// filter delta by `±tC` or `±tC0`.
const fn clip3_sym(c: i32, v: i32) -> i32 {
    if v < -c {
        -c
    } else if v > c {
        c
    } else {
        v
    }
}

/// `α`/`β`/`tC0` for one edge (clause 8.7.2.2), computed once and reused
/// across every line that crosses it -- alpha, beta and tC0 depend only on
/// the two sides' QP and the slice's own filter offsets, never on the
/// sample values, so this is genuinely per-edge, not per-line, state. This
/// is the "operates on edges" half of this crate's own interface shape.
#[derive(Debug, Clone, Copy)]
pub struct EdgeThresholds {
    alpha: i32,
    beta: i32,
    tc0: [i32; 3],
}

impl EdgeThresholds {
    /// `indexA`/`indexB` (clause 8.7.2.2, eq. 8-456/8-457):
    /// `qPav = (qPp + qPq + 1) >> 1`, then each index is `Clip3(0, 51,
    /// qPav + filterOffset{A,B})`. `filter_offset_a`/`filter_offset_b` are
    /// already `slice_alpha_c0_offset_div2 * 2`/`slice_beta_offset_div2 *
    /// 2` -- the caller's job, since only it has the slice header.
    #[must_use]
    pub fn derive(qp_p: u8, qp_q: u8, filter_offset_a: i32, filter_offset_b: i32) -> Self {
        let qp_av = (i32::from(qp_p) + i32::from(qp_q) + 1) >> 1;
        let index_a = (qp_av + filter_offset_a).clamp(0, 51);
        let index_b = (qp_av + filter_offset_b).clamp(0, 51);
        #[allow(
            clippy::cast_sign_loss,
            clippy::indexing_slicing,
            reason = "index_a/index_b are Clip3(0, 51, _) above, always in range"
        )]
        let (alpha, beta) = (
            i32::from(tables::ALPHA_TABLE[index_a as usize]),
            i32::from(tables::BETA_TABLE[index_b as usize]),
        );
        #[allow(
            clippy::cast_sign_loss,
            clippy::indexing_slicing,
            reason = "index_a is Clip3(0, 51, _) above, always in range"
        )]
        let tc0_row = tables::TC0_TABLE[index_a as usize];
        Self {
            alpha,
            beta,
            tc0: [
                i32::from(tc0_row[0]),
                i32::from(tc0_row[1]),
                i32::from(tc0_row[2]),
            ],
        }
    }

    /// Whether this edge's samples pass clause 8.7.2.1's own
    /// `filterSamplesFlag` test for one specific line (`|p0-q0| < α`,
    /// `|p1-p0| < β`, `|q1-q0| < β`) -- computed per line because it reads
    /// sample values, unlike `α`/`β`/`tC0` themselves.
    fn samples_pass(&self, p0: i32, p1: i32, q0: i32, q1: i32) -> bool {
        (p0 - q0).abs() < self.alpha && (p1 - p0).abs() < self.beta && (q1 - q0).abs() < self.beta
    }
}

/// One line of luma samples straddling a filtered edge, clause 8.7.2's own
/// `p0..p3`/`q0..q3` naming kept verbatim: `p[0]` = p0 (nearest the edge)
/// .. `p[3]` = p3 (farthest); `q` mirrors it on the other side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LumaLine {
    pub p: [u8; 4],
    pub q: [u8; 4],
}

/// Filters one line of luma samples in place against a precomputed edge
/// (clause 8.7.2.3 for `bs < 4`, clause 8.7.2.4 for `bs == 4`). A no-op if
/// `filterSamplesFlag` does not hold for this specific line -- most lines
/// crossing most edges in real content do pass it, but this function, not
/// the caller, is where that check lives.
pub fn filter_luma_line(line: &mut LumaLine, bs: NonZeroU8, edge: EdgeThresholds) {
    let [p3, p2, p1, p0] = [line.p[3], line.p[2], line.p[1], line.p[0]].map(i32::from);
    let [q0, q1, q2, q3] = [line.q[0], line.q[1], line.q[2], line.q[3]].map(i32::from);
    if !edge.samples_pass(p0, p1, q0, q1) {
        return;
    }
    let bs = bs.get();
    if bs == 4 {
        let strong_side_threshold = (edge.alpha >> 2) + 2;
        let ap = (p2 - p0).abs();
        let aq = (q2 - q0).abs();
        let strong = (p0 - q0).abs() < strong_side_threshold;

        let (p0n, p1n, p2n) = if ap < edge.beta && strong {
            (
                (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3,
                (p2 + p1 + p0 + q0 + 2) >> 2,
                (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3,
            )
        } else {
            ((2 * p1 + p0 + q1 + 2) >> 2, p1, p2)
        };
        let (q0n, q1n, q2n) = if aq < edge.beta && strong {
            (
                (q2 + 2 * q1 + 2 * q0 + 2 * p0 + p1 + 4) >> 3,
                (q2 + q1 + q0 + p0 + 2) >> 2,
                (2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3,
            )
        } else {
            ((2 * q1 + q0 + p1 + 2) >> 2, q1, q2)
        };
        line.p = [clip1(p0n), clip1(p1n), clip1(p2n), line.p[3]];
        line.q = [clip1(q0n), clip1(q1n), clip1(q2n), line.q[3]];
        return;
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "bs in 1..=3 here (4 handled above); TC0_TABLE rows have exactly 3 columns"
    )]
    let tc0 = edge.tc0[usize::from(bs - 1)];
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    let tc = tc0 + i32::from(ap < edge.beta) + i32::from(aq < edge.beta);
    let delta = clip3_sym(tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
    let p0n = clip1(p0 + delta);
    let q0n = clip1(q0 - delta);
    let p1n = if ap < edge.beta {
        clip1(p1 + clip3_sym(tc0, (p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1))
    } else {
        line.p[1]
    };
    let q1n = if aq < edge.beta {
        clip1(q1 + clip3_sym(tc0, (q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1))
    } else {
        line.q[1]
    };
    line.p = [p0n, p1n, line.p[2], line.p[3]];
    line.q = [q0n, q1n, line.q[2], line.q[3]];
}

/// One line of chroma samples straddling a filtered edge. Chroma only ever
/// modifies `p0`/`q0` (clause 8.7.2.4's own chroma case), so only `p1`/`q1`
/// are needed as extra context -- no `p2`/`p3`/`q2`/`q3` at all, unlike
/// luma.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChromaLine {
    pub p: [u8; 2],
    pub q: [u8; 2],
}

/// Filters one line of chroma samples in place against a precomputed edge.
/// `edge` must be derived from the **chroma** QPs (`QPC`, clause 8.7.2.2's
/// own `qPav` note that this differs between luma and chroma) -- this
/// function does not know or care which, it only ever sees the resulting
/// thresholds.
pub fn filter_chroma_line(line: &mut ChromaLine, bs: NonZeroU8, edge: EdgeThresholds) {
    let [p1, p0] = [line.p[1], line.p[0]].map(i32::from);
    let [q0, q1] = [line.q[0], line.q[1]].map(i32::from);
    if !edge.samples_pass(p0, p1, q0, q1) {
        return;
    }
    let bs = bs.get();
    if bs == 4 {
        line.p[0] = clip1((2 * p1 + p0 + q1 + 2) >> 2);
        line.q[0] = clip1((2 * q1 + q0 + p1 + 2) >> 2);
        return;
    }
    #[allow(
        clippy::indexing_slicing,
        reason = "bs in 1..=3 here (4 handled above); TC0_TABLE rows have exactly 3 columns"
    )]
    let tc0 = edge.tc0[usize::from(bs - 1)];
    // Clause 8.7.2.4's own chroma case: tC = tC0 + 1, unconditionally --
    // chroma has no ap/aq-gated "+1" the way luma does, because chroma
    // never touches p1/q1's own values at bs < 4.
    let tc = tc0 + 1;
    let delta = clip3_sym(tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
    line.p[0] = clip1(p0 + delta);
    line.q[0] = clip1(q0 - delta);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn nz(v: u8) -> NonZeroU8 {
        NonZeroU8::new(v).unwrap()
    }

    #[test]
    fn a_flat_line_is_never_modified_regardless_of_bs() {
        // p0 == q0 and no gradient at all: filterSamplesFlag holds (0 < any
        // positive alpha/beta), but every filter equation's own delta is
        // zero on genuinely flat input -- the "nothing to do" case a real
        // decoder hits constantly (interior of a smooth surface) must be
        // an exact identity, not merely "close".
        for bs in 1..=4u8 {
            let mut line = LumaLine {
                p: [128; 4],
                q: [128; 4],
            };
            let edge = EdgeThresholds::derive(28, 28, 0, 0);
            filter_luma_line(&mut line, nz(bs), edge);
            assert_eq!(
                line,
                LumaLine {
                    p: [128; 4],
                    q: [128; 4]
                },
                "bs={bs}"
            );
        }
    }

    #[test]
    fn high_qp_step_edge_is_smoothed_towards_the_boundary() {
        // A real step edge (dark left, light right) at a QP high enough to
        // give a non-zero alpha/beta/tC0: filtering must pull p0/q0 towards
        // each other, not push them apart or leave them untouched.
        let edge = EdgeThresholds::derive(36, 36, 0, 0);
        let mut line = LumaLine {
            p: [40, 42, 44, 46],
            q: [200, 198, 196, 194],
        };
        let before = (i32::from(line.p[0]) - i32::from(line.q[0])).abs();
        filter_luma_line(&mut line, nz(1), edge);
        let after = (i32::from(line.p[0]) - i32::from(line.q[0])).abs();
        assert!(
            after <= before,
            "filtering should not increase the p0/q0 step: {before} -> {after}"
        );
    }

    #[test]
    fn bs_four_strong_filter_can_reach_p1_p2_unlike_normal_filter() {
        // The defining structural difference between the two shapes: at
        // bs<4 only p0/p1/q0/q1 can ever change, never p2/q2. At bs==4,
        // p2/q2 can change too, when the "strong" side condition holds.
        let edge = EdgeThresholds::derive(30, 30, 0, 0);
        let flat_ish = LumaLine {
            p: [120, 121, 122, 123],
            q: [124, 125, 126, 127],
        };
        let mut normal = flat_ish;
        filter_luma_line(&mut normal, nz(1), edge);
        assert_eq!(normal.p[2], flat_ish.p[2], "bs<4 must never touch p2");
        assert_eq!(normal.q[2], flat_ish.q[2], "bs<4 must never touch q2");
    }

    #[test]
    fn chroma_never_touches_more_than_p0_q0() {
        let edge = EdgeThresholds::derive(36, 36, 0, 0);
        for bs in 1..=4u8 {
            let before = ChromaLine {
                p: [40, 60],
                q: [200, 180],
            };
            let mut after = before;
            filter_chroma_line(&mut after, nz(bs), edge);
            assert_eq!(
                after.p[1], before.p[1],
                "bs={bs}: chroma must never touch p1"
            );
            assert_eq!(
                after.q[1], before.q[1],
                "bs={bs}: chroma must never touch q1"
            );
        }
    }

    #[test]
    fn filter_offsets_shift_which_index_a_index_b_are_used() {
        // A positive filterOffsetA should never produce a *smaller* alpha
        // than offset 0 at the same QP, since ALPHA_TABLE is non-decreasing
        // and Clip3 only saturates at the top, never wraps.
        let base = EdgeThresholds::derive(30, 30, 0, 0);
        let boosted = EdgeThresholds::derive(30, 30, 12, 0);
        assert!(boosted.alpha >= base.alpha);
    }
}
