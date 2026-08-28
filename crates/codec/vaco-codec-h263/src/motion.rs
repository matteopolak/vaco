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
) -> u8 {
    let int_x = mv_x.div_euclid(2);
    let int_y = mv_y.div_euclid(2);
    let half_x = mv_x.rem_euclid(2) != 0;
    let half_y = mv_y.rem_euclid(2) != 0;
    let x = src_x + int_x;
    let y = src_y + int_y;

    match (half_x, half_y) {
        (false, false) => refp.sample(plane, x, y),
        (true, false) => avg2(refp.sample(plane, x, y), refp.sample(plane, x + 1, y)),
        (false, true) => avg2(refp.sample(plane, x, y), refp.sample(plane, x, y + 1)),
        (true, true) => avg4(
            refp.sample(plane, x, y),
            refp.sample(plane, x + 1, y),
            refp.sample(plane, x, y + 1),
            refp.sample(plane, x + 1, y + 1),
        ),
    }
}

/// `b = (A + B + 1) / 2` — round to nearest, ties away from zero, which
/// for a non-negative sum is simply "round half up".
fn avg2(a: u8, b: u8) -> u8 {
    (u16::from(a) + u16::from(b)).div_ceil(2) as u8
}

#[allow(
    clippy::integer_division,
    reason = "`d = (A + B + C + D + 2) / 4` is the literal formula (H.263 Figure 12 / H.261 §3.2.1's identical bilinear scheme) — the +2-then-truncate form is round-to-nearest for a divisor of 4, not an approximation"
)]
fn avg4(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((u16::from(a) + u16::from(b) + u16::from(c) + u16::from(d) + 2) / 4) as u8
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
            if let Some(slot) = tmp.get_mut(usize::try_from(y).unwrap_or(0) * w + usize::try_from(x).unwrap_or(0)) {
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
            if let Some(slot) = block.get_mut(usize::try_from(y).unwrap_or(0) * w + usize::try_from(x).unwrap_or(0)) {
                *slot = v.clamp(0, 255) as u8;
            }
        }
    }
}
