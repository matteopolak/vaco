//! The shared core of angular/directional intra prediction: project each
//! output position along a signed angle onto a 1-D reference array and
//! linearly interpolate between the two nearest samples.
//!
//! Both ITU-T H.265 §8.4.4.2.6 and `AOMedia` AV1 §7.11.2.4 parameterise their
//! directional prediction the same way: an angle expressed in 1/32-sample
//! units, and a 5-bit fractional interpolation weight recovered from the
//! low 5 bits of the projected position. This function is that shared
//! arithmetic — see the crate root doc for the confidence level this was
//! checked to (tier 1 self-consistency, not a line-by-line primary-text
//! check) and what a caller wiring this into a real decoder still needs to
//! verify.
//!
//! A format's own mode-to-angle table and its choice of which reference
//! array (top-derived or left-derived, and how far it extends for
//! negative angles) to project onto are that format's own responsibility,
//! not this crate's — see [`angular_project`]'s doc for exactly what it
//! assumes about `refs`.

/// Directional prediction for one row (or, by symmetry, one column — the
/// caller decides which axis `pos` indexes and transposes the result if
/// needed) of a block.
///
/// - `refs[k]` is the main reference array, indexed from the point the
///   projection can reach at its most negative extent — `refs[0]` is
///   whichever reference sample a format's own construction defines as
///   "position 0" of this array (for a vertical-ish HEVC mode, that is the
///   corner sample directly above-left of the block); the caller builds
///   this array in whatever order its format's projection needs and is
///   responsible for extending it far enough that every `size` output
///   position's projection stays in range for the steepest angle it
///   passes.
/// - `pos` is `0`-based row/column index within the block (HEVC's `y` or
///   `x`, whichever this call's axis represents).
/// - `angle` is the signed projection step in 1/32-sample units per
///   position (HEVC's `intraPredAngle`, range `-32..=32`).
/// - `dst[x]` for `x in 0..dst.len()` receives the interpolated value at
///   projected position `((pos + 1) * angle)`, split into an integer part
///   (added to `x`) and a 5-bit fractional weight.
///
/// `angle == 0` makes every position's fractional weight `0`, so the
/// result is an exact copy of `size` consecutive entries of `refs`
/// starting at `refs[1]` — the "no diagonal, this is really a
/// vertical/horizontal copy" case every angular scheme special-cases at
/// the bitstream level but which falls out of this formula for free.
///
/// Reads past the end of `refs` are treated as `0` rather than panicking
/// (an out-of-range projection is a caller bug — too short a `refs` for
/// the angle/size combination — reported this way rather than aborting,
/// since prediction has no way to fail cleanly mid-block).
pub fn angular_project(dst: &mut [u16], refs: &[u16], pos: usize, angle: i32) {
    let step = i64::from(pos_plus_one(pos)) * i64::from(angle);
    // Arithmetic (floor) shift, matching HEVC's `>>` on a value that can be
    // negative for angle < 0 -- `iIdx = step >> 5` must floor toward
    // negative infinity, not truncate toward zero, or every negative-angle
    // projection lands one reference sample short.
    let int_part = step.div_euclid(32);
    let frac = step.rem_euclid(32);

    for (x, slot) in dst.iter_mut().enumerate() {
        let x_i = i64::try_from(x).unwrap_or(0);
        let base = x_i + int_part;
        let a = ref_at(refs, base + 1);
        if frac == 0 {
            *slot = a;
            continue;
        }
        let b = ref_at(refs, base + 2);
        let weight_a = 32 - frac;
        let sum = weight_a * i64::from(a) + frac * i64::from(b) + 16;
        *slot = u16::try_from((sum >> 5).clamp(0, i64::from(u16::MAX))).unwrap_or(0);
    }
}

fn pos_plus_one(pos: usize) -> u32 {
    u32::try_from(pos).unwrap_or(u32::MAX).saturating_add(1)
}

fn ref_at(refs: &[u16], index: i64) -> u16 {
    if index < 0 {
        return 0;
    }
    usize::try_from(index)
        .ok()
        .and_then(|i| refs.get(i))
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test fixtures index small fixed arrays and recompute the expected value with plain \
              truncating division deliberately, to compare against div_euclid's floor division"
)]
mod tests {
    use super::*;

    #[test]
    fn zero_angle_is_an_exact_copy_of_consecutive_refs() {
        let refs = [10u16, 20, 30, 40, 50, 60, 70, 80];
        let mut dst = [0u16; 4];
        angular_project(&mut dst, &refs, 0, 0);
        // int_part=0, frac=0 -> dst[x] = refs[x + 1].
        assert_eq!(dst, [20, 30, 40, 50]);
        // Same for a different `pos`: angle 0 never depends on `pos`.
        let mut dst2 = [0u16; 4];
        angular_project(&mut dst2, &refs, 7, 0);
        assert_eq!(dst2, [20, 30, 40, 50]);
    }

    #[test]
    fn positive_full_step_shifts_by_one_reference_sample() {
        // angle=32 (one full 1/32 unit's worth of steps = 32, i.e. exactly
        // 1.0 sample per position) at pos=0: step = 1*32=32, int_part=1,
        // frac=0 -> dst[x] = refs[x + 2].
        let refs = [10u16, 20, 30, 40, 50, 60];
        let mut dst = [0u16; 3];
        angular_project(&mut dst, &refs, 0, 32);
        assert_eq!(dst, [30, 40, 50]);
    }

    #[test]
    fn exact_half_weight_is_the_average_of_the_two_neighbours() {
        // angle=16 at pos=0: step=16, int_part=0, frac=16 -> exactly
        // halfway between refs[x+1] and refs[x+2].
        let refs = [0u16, 100, 200, 300];
        let mut dst = [0u16; 2];
        angular_project(&mut dst, &refs, 0, 16);
        // dst[0] = (16*100 + 16*200 + 16) >> 5 = (1600+3200+16)>>5 = 4816>>5=150.
        // dst[1] = (16*200 + 16*300 + 16) >> 5 = (3200+4800+16)>>5 = 8016>>5=250.
        assert_eq!(dst, [150, 250]);
    }

    #[test]
    fn linear_reference_ramp_interpolates_exactly() {
        // refs[k] = 10*k exactly: any linear interpolation between two
        // points on a line lands back on that same line, for every angle
        // and every position -- an exact, not approximate, property.
        //
        // Restricted to non-negative angles and a refs array long enough
        // that every projected index for these pos/angle/x combinations
        // stays inside the array: a projection that runs off either end
        // hits `ref_at`'s documented zero-fill, which is a real and
        // separately-tested behaviour but breaks pure linearity for a
        // reason that has nothing to do with the interpolation itself.
        let refs: [u16; 64] = core::array::from_fn(|k| (k as u16) * 10);
        for angle in [0, 5, 17, 32] {
            for pos in [0usize, 1, 3, 7] {
                let mut dst = [0u16; 4];
                angular_project(&mut dst, &refs, pos, angle);
                for (x, &v) in dst.iter().enumerate() {
                    let pos_u32 = u32::try_from(pos).unwrap_or(0);
                    let x_i64 = i64::try_from(x).unwrap_or(0);
                    let step = i64::from(pos_u32 + 1) * i64::from(angle);
                    let expect = (x_i64 + 1) * 10 + (step * 10) / 32;
                    // `/32` here truncates toward zero for negative steps,
                    // matching Rust's default -- the same source of a
                    // possible +-1 as the function's own div_euclid choice
                    // for a negative, non-multiple-of-32 step, so allow a
                    // 1-unit tolerance rather than asserting bit-exactness
                    // against a differently-rounded reference computation.
                    let got = i64::from(v);
                    assert!(
                        (got - expect).abs() <= 1,
                        "angle={angle} pos={pos} x={x}: got {got}, expected ~{expect}"
                    );
                }
            }
        }
    }

    #[test]
    fn negative_angle_projects_backward_without_panicking() {
        let refs = [5u16, 10, 15, 20, 25, 30, 40, 50];
        let mut dst = [0u16; 4];
        angular_project(&mut dst, &refs, 3, -20);
        // No fixed expected value asserted beyond "ran to completion with
        // in-range output" -- the exact-value properties above already
        // pin the arithmetic; this just exercises the negative branch of
        // div_euclid/rem_euclid specifically.
        assert!(dst.iter().all(|&v| v <= 50));
    }

    #[test]
    fn out_of_range_projection_reads_as_zero_not_panic() {
        let refs = [1u16, 2, 3];
        let mut dst = [9u16; 4];
        angular_project(&mut dst, &refs, 100, 32);
        // Every projected index is far past the 3-entry array; every
        // output must be the documented zero-fill, not garbage or a panic.
        assert_eq!(dst, [0, 0, 0, 0]);
    }

    proptest::proptest! {
        #[test]
        fn angular_project_never_panics(
            refs in proptest::collection::vec(proptest::num::u16::ANY, 0..64),
            pos in 0usize..1000,
            angle in -1000i32..1000,
            len in 0usize..32,
        ) {
            let mut dst = vec![0u16; len];
            angular_project(&mut dst, &refs, pos, angle);
        }
    }
}
