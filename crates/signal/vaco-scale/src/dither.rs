//! Quantisation noise shaping on the way down in bit depth.
//!
//! The 8×8 Bayer threshold matrix is **generated**, never tabulated:
//!
//! ```text
//!   M1 = [0]
//!   M2n = [ 4·Mn + 0   4·Mn + 2 ]
//!         [ 4·Mn + 3   4·Mn + 1 ]
//! ```
//!
//! (Bayer, "An optimum method for two-level rendition of continuous-tone
//! pictures", IEEE ICC 1973.) That recursion is the definition, so the table it
//! produces is a fact rather than an authorial choice — which is what keeps it
//! clean-room safe (D7/D15) and what makes it checkable by a test rather than by
//! eye.
//!
//! # Class A
//!
//! The threshold is a pure function of `(x, y)`, so output is identical
//! regardless of band split, thread count or lane width. That is not an accident
//! of the implementation; it is why ordered dither is the default and error
//! diffusion is not implemented (see the crate docs).

/// Side of the generated matrix.
pub const BAYER_N: usize = 8;

/// The 8×8 Bayer matrix, values `0..64`.
pub const BAYER: [[u8; BAYER_N]; BAYER_N] = generate();

#[allow(
    clippy::indexing_slicing,
    reason = "const evaluation over fixed 8x8 arrays; any error is a compile-time panic"
)]
const fn generate() -> [[u8; BAYER_N]; BAYER_N] {
    let mut m = [[0u8; BAYER_N]; BAYER_N];
    let mut size = 1usize;
    while size < BAYER_N {
        let mut y = 0;
        while y < size {
            let mut x = 0;
            while x < size {
                let base = m[y][x] * 4;
                m[y][x] = base;
                m[y][x + size] = base + 2;
                m[y + size][x] = base + 3;
                m[y + size][x + size] = base + 1;
                x += 1;
            }
            y += 1;
        }
        size *= 2;
    }
    m
}

/// The additive threshold used when reducing by `shift` bits at `(x, y)`.
///
/// Scaled so the thresholds span exactly one output quantisation step: with
/// `shift` bits dropped, the step is `1 << shift` and the 64 matrix levels are
/// stretched or squeezed onto it.
#[must_use]
#[inline]
pub fn bayer_threshold(x: usize, y: usize, shift: u8) -> i32 {
    let Some(row) = BAYER.get(y % BAYER_N) else {
        return 0;
    };
    let v = i32::from(row.get(x % BAYER_N).copied().unwrap_or(0));
    if shift >= 6 {
        v << (shift - 6)
    } else {
        v >> (6 - shift)
    }
}

/// Reduce `v` from `from` bits to `to` bits, rounding to nearest.
///
/// This is the reference's behaviour and is a *shift*, not a full-scale
/// rescale: 16-bit `65535` becomes 8-bit `255` by clamping, not by dividing by
/// 257. Expansion is the other way round — see [`expand_depth`].
#[must_use]
#[inline]
pub fn reduce_depth(v: i32, from: u8, to: u8) -> i32 {
    if to >= from {
        return v;
    }
    let shift = from - to;
    let max = (1i32 << to) - 1;
    ((v + (1 << (shift - 1))) >> shift).clamp(0, max)
}

/// Reduce with an explicit additive threshold in place of the rounding term.
#[must_use]
#[inline]
pub fn reduce_depth_dithered(v: i32, from: u8, to: u8, threshold: i32) -> i32 {
    if to >= from {
        return v;
    }
    let shift = from - to;
    let max = (1i32 << to) - 1;
    ((v + threshold) >> shift).clamp(0, max)
}

/// Expand `v` from `from` bits to `to` bits by **bit replication**.
///
/// `255` at 8 bits becomes `65535` at 16 and `1023` at 10, which is what the
/// reference does and what full-scale expansion means. A plain left shift would
/// map full white to `65280`, darkening every white in the picture by a quarter
/// of a percent.
#[must_use]
#[inline]
pub fn expand_depth(v: i32, from: u8, to: u8) -> i32 {
    if to <= from || from == 0 {
        return v;
    }
    let mut acc = v as u32;
    let mut bits = u32::from(from);
    while bits < u32::from(to) {
        acc = (acc << from) | (v as u32);
        bits += u32::from(from);
    }
    i32::try_from(acc >> (bits - u32::from(to))).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;

    #[test]
    fn bayer_is_a_permutation_of_zero_to_sixty_three() {
        let mut seen = [false; 64];
        for row in BAYER {
            for v in row {
                assert!(!seen[v as usize], "{v} appears twice");
                seen[v as usize] = true;
            }
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn bayer_top_left_quadrant_is_the_recursion() {
        // M2 = [[0, 2], [3, 1]] scaled up twice gives the canonical M8, whose
        // first row is 0, 32, 8, 40, 2, 34, 10, 42.
        assert_eq!(BAYER[0], [0, 32, 8, 40, 2, 34, 10, 42]);
        assert_eq!(BAYER[1][0], 48);
        assert_eq!(BAYER[1][1], 16);
        assert_eq!(BAYER[4][0], 3);
    }

    #[test]
    fn expansion_is_full_scale_and_reduction_is_a_shift() {
        assert_eq!(expand_depth(255, 8, 16), 65535);
        assert_eq!(expand_depth(255, 8, 10), 1023);
        assert_eq!(expand_depth(1, 8, 10), 4);
        assert_eq!(expand_depth(252, 8, 10), 1011);
        // Measured against the reference: `min(255, (v + 128) >> 8)`.
        assert_eq!(reduce_depth(65535, 16, 8), 255);
        assert_eq!(reduce_depth(128, 16, 8), 1);
        assert_eq!(reduce_depth(127, 16, 8), 0);
    }

    #[test]
    fn every_depth_pair_round_trips_through_expansion() {
        for from in 1u8..=16 {
            for to in from..=16 {
                let max_from = (1i32 << from) - 1;
                assert_eq!(expand_depth(0, from, to), 0);
                assert_eq!(expand_depth(max_from, from, to), (1i32 << to) - 1);
            }
        }
    }
}
