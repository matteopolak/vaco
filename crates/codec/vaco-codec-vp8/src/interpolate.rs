//! Sub-pixel motion-compensated prediction, RFC 6386 §18.
//!
//! Operates on a caller-supplied source sampler rather than a concrete
//! plane type, so it works identically against the reference-frame buffers
//! [`crate::decode`] owns without this module needing to know their layout.
//! Out-of-frame reads are the caller's job too (via the sampler), matching
//! the "extend the border" requirement §18.1 states informally.

use crate::tables::{BILINEAR_FILTERS, SIXTAP_FILTERS};

fn clamp255(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
}

/// A small block coordinate (0..~20) as a signed tap offset.
fn si(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// One 1-D 6-tap (or degenerate bilinear) convolution centred so that
/// `sample(2)` is the filter's nominal origin — RFC 6386 §18.3's `interp`.
fn interp(fil: &[i32; 6], sample: impl Fn(i32) -> u8) -> u8 {
    let mut acc = 0i32;
    for (k, &tap) in fil.iter().enumerate() {
        acc += i32::from(sample(si(k) - 2)) * tap;
    }
    clamp255((acc + 64) >> 7)
}

/// Predict one `W x H` block via two-pass sub-pixel interpolation.
/// `src(x, y)` returns the reference plane's sample at integer position
/// `(x, y)`, with edge extension already applied by the caller. `origin`
/// is the integer part of the motion vector already added to the block's
/// frame position (i.e. `src(0,0)` is this block's top-left reference
/// pixel before considering the fractional phase). `hfrac`/`vfrac` are
/// eighth-pel phases 0..7. `bilinear` selects RFC 6386 §18.3's
/// `BilinearFilters` instead of the 6-tap `filters` table (chosen per
/// frame from the frame-tag version number).
pub fn predict_block<const W: usize, const H: usize>(
    src: impl Fn(i32, i32) -> u8,
    hfrac: usize,
    vfrac: usize,
    bilinear: bool,
) -> [[u8; W]; H] {
    let table = if bilinear {
        &BILINEAR_FILTERS
    } else {
        &SIXTAP_FILTERS
    };
    let hfil = table.get(hfrac).copied().unwrap_or([0, 0, 128, 0, 0, 0]);
    let vfil = table.get(vfrac).copied().unwrap_or([0, 0, 128, 0, 0, 0]);

    if hfrac == 0 && vfrac == 0 {
        let mut out = [[0u8; W]; H];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, px) in row.iter_mut().enumerate() {
                *px = src(si(c), si(r));
            }
        }
        return out;
    }

    // Horizontal pass: H + 5 rows (2 above, 3 below) so the vertical pass
    // has its full 6-tap support. Sized from a compile-time constant (W/H
    // are const generics fixed by the call site, never attacker data), so
    // this is not the header-derived-allocation case `Budget` guards
    // against.
    let mut temp_rows: Vec<[u8; W]> = Vec::new();
    for r in -2..(si(H) + 3) {
        let mut row = [0u8; W];
        for (c, px) in row.iter_mut().enumerate() {
            *px = interp(&hfil, |k| src(si(c) + k, r));
        }
        temp_rows.push(row);
    }

    let mut out = [[0u8; W]; H];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, px) in row.iter_mut().enumerate() {
            *px = interp(&vfil, |k| {
                let idx = usize::try_from(si(r) + 2 + k).unwrap_or(0);
                temp_rows
                    .get(idx)
                    .and_then(|row| row.get(c))
                    .copied()
                    .unwrap_or(0)
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_pixel_motion_is_a_direct_copy() {
        let out = predict_block::<4, 4>(|x, y| ((x + y * 4) & 0xff) as u8, 0, 0, false);
        for (r, row) in out.iter().enumerate() {
            for (c, &px) in row.iter().enumerate() {
                assert_eq!(px, ((si(c) + si(r) * 4) & 0xff) as u8);
            }
        }
    }

    #[test]
    fn a_flat_field_stays_flat_under_any_phase() {
        let out = predict_block::<4, 4>(|_, _| 100u8, 3, 5, false);
        for row in out {
            for px in row {
                assert_eq!(px, 100);
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn interpolation_never_produces_a_panic_or_out_of_range_pixel(
            hfrac in 0usize..8, vfrac in 0usize..8, bilinear: bool, base: u8,
        ) {
            let out = predict_block::<4, 4>(|_, _| base, hfrac, vfrac, bilinear);
            for row in out {
                for px in row {
                    let _ = px; // u8 is inherently in-range; this asserts no panic occurred
                }
            }
        }
    }
}
