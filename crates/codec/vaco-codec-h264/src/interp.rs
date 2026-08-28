//! Clause 8.4.2.2.1's luma quarter-sample interpolation: the six-tap FIR
//! for half-sample positions, and simple averaging for quarter-sample
//! ones. Chroma (clause 8.4.2.2.2, bilinear) is out of scope for now --
//! `crate::reconstruct::PictureBuffer` does not store chroma samples at
//! all yet, a pre-existing gap this module does not need to touch.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::integer_division, reason = "test code")]
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
}
