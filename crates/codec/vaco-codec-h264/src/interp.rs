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
    // 8-238), rounded and clipped -- used directly for `frac_y == 0`.
    let half_h = |dy: i32| -> i32 {
        round_half(tap6(
            f(-2, dy),
            f(-1, dy),
            f(0, dy),
            f(1, dy),
            f(2, dy),
            f(3, dy),
        ))
    };
    // Vertical half-pel at column `dx` (`h`-shaped), rounded and clipped.
    let half_v = |dx: i32| -> i32 {
        round_half(tap6(
            f(dx, -2),
            f(dx, -1),
            f(dx, 0),
            f(dx, 1),
            f(dx, 2),
            f(dx, 3),
        ))
    };
    // Raw (unrounded) horizontal 6-tap sum at row `dy`, for `j`'s own
    // two-pass derivation.
    let raw_h =
        |dy: i32| -> i32 { tap6(f(-2, dy), f(-1, dy), f(0, dy), f(1, dy), f(2, dy), f(3, dy)) };

    match (frac_x, frac_y) {
        (0, 0) => unreachable!("handled above"),
        (2, 0) => clip_u8(half_h(0)), // b
        (0, 2) => clip_u8(half_v(0)), // h
        (2, 2) => clip_u8(round_quarter_pass(tap6(
            raw_h(-2),
            raw_h(-1),
            raw_h(0),
            raw_h(1),
            raw_h(2),
            raw_h(3),
        ))), // j
        (1, 0) => clip_u8(avg(f(0, 0), half_h(0))), // a
        (3, 0) => clip_u8(avg(half_h(0), f(1, 0))), // c
        (0, 1) => clip_u8(avg(f(0, 0), half_v(0))), // d
        (0, 3) => clip_u8(avg(half_v(0), f(0, 1))), // n
        (1, 1) => clip_u8(avg(half_h(0), half_v(0))), // e
        (3, 1) => clip_u8(avg(half_h(0), half_v(1))), // g
        (1, 3) => clip_u8(avg(half_v(0), half_h(1))), // p
        (3, 3) => clip_u8(avg(half_v(1), half_h(1))), // r
        (2, 1) => {
            // f: average of j (both-half) and b-at-row-0.
            let j = round_quarter_pass(tap6(
                raw_h(-2),
                raw_h(-1),
                raw_h(0),
                raw_h(1),
                raw_h(2),
                raw_h(3),
            ));
            clip_u8(avg(half_h(0), j))
        }
        (2, 3) => {
            let j = round_quarter_pass(tap6(
                raw_h(-2),
                raw_h(-1),
                raw_h(0),
                raw_h(1),
                raw_h(2),
                raw_h(3),
            ));
            clip_u8(avg(j, half_h(1)))
        }
        (1, 2) => {
            let j = round_quarter_pass(tap6(
                raw_h(-2),
                raw_h(-1),
                raw_h(0),
                raw_h(1),
                raw_h(2),
                raw_h(3),
            ));
            clip_u8(avg(half_v(0), j))
        }
        (3, 2) => {
            let j = round_quarter_pass(tap6(
                raw_h(-2),
                raw_h(-1),
                raw_h(0),
                raw_h(1),
                raw_h(2),
                raw_h(3),
            ));
            clip_u8(avg(j, half_v(1)))
        }
        _ => clip_u8(f(0, 0)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::integer_division, reason = "test code")]
mod tests {
    use super::*;

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
