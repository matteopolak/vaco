//! Shared 8-bit plane helpers for this crate's neighbourhood filters.
//!
//! # Scope: 8-bit addressable formats only
//!
//! Every filter in this crate rejects any format wider than 8 bits per
//! component, exactly as `vaco-filter-video-composite::geom::ensure_addressable_8bit`
//! does and for the same reason: the reference supports higher bit depths for
//! most of these filters, but implementing generic sample-width math is a
//! separate, larger effort than this crate's brief budgets for. This is a
//! recorded, deliberate gap (see `docs/filter/vaco-filter-blur.md`), not a
//! silent one.
//!
//! Not reused from `vaco-filter-video-composite` or `vaco-filter-video-geometry`:
//! both crates' equivalents are `pub(crate)`, and D19 governs shared *types*,
//! not tiny format-flag predicates that several crates independently need —
//! the geometry crate's own doc comment for its copy makes the same call.

use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Reject formats this crate's byte-level, 8-bit-only pixel math cannot
/// address: a hardware surface, sub-byte packing, a palette needing a side
/// table, or any depth other than 8 bits.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] naming which property is the problem.
pub(crate) fn ensure_8bit_addressable(format: PixFmt) -> Result<()> {
    if format.has(PixFmtFlags::HW_ACCEL) {
        return Err(Error::Unsupported("cannot address a hardware surface"));
    }
    if format.has(PixFmtFlags::BITSTREAM) {
        return Err(Error::Unsupported(
            "cannot address a sub-byte-packed format",
        ));
    }
    if format.has(PixFmtFlags::PALETTE) {
        return Err(Error::Unsupported(
            "cannot address a palette format without its side table",
        ));
    }
    if format.max_depth() != 8 {
        return Err(Error::Unsupported(
            "vaco-filter-blur only filters 8-bit samples",
        ));
    }
    Ok(())
}

/// `u32` (or `usize`) to `i32`, saturating rather than wrapping.
///
/// Frame dimensions and kernel indices in this crate never approach
/// `i32::MAX`; this avoids `clippy::cast_possible_wrap` at every call site
/// without an `#[allow]` per site, the same way
/// `vaco-filter-video-geometry::crop::to_i32` does for the same reason.
#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Copy every metadata field a frame carries besides its pixel data.
///
/// Every filter in this crate reuses this rather than repeating the same six
/// assignments, which is exactly the kind of small duplication D19 does not
/// track by name but that is still worth not typing out fourteen times.
pub(crate) fn copy_frame_meta(out: &mut Frame, input: &Frame) {
    out.pts = input.pts;
    out.time_base = input.time_base;
    out.duration = input.duration;
    out.color = input.color;
    out.flags = input.flags;
    out.sample_aspect_ratio = input.sample_aspect_ratio;
}

/// Decode the reference's `planes` bitmask (bit `p` = plane `p` is touched).
///
/// Shared by every filter in this crate that has a `planes` option: `0..15`
/// covers up to four planes, matching every option table probed for this
/// crate (`ffmpeg -h filter=<name>`, 2026-08-23).
#[must_use]
pub(crate) const fn plane_selected(mask: i64, plane: u8) -> bool {
    (mask >> plane) & 1 != 0
}

/// Clamp-to-edge (replicate) sample of an 8-bit-unit plane at signed
/// coordinates, for filters measured to extend the border by repeating the
/// nearest real sample (`boxblur`, `avgblur`, `unsharp`'s internal blur,
/// `dilation`/`erosion`, `median`; see each module's doc for the probe that
/// pinned this down against the alternative of zero-padding).
///
/// `unit` is the byte stride of one sample in this plane
/// ([`plane_unit_bytes`]); only unit `1` (the formats this crate accepts, per
/// [`ensure_8bit_addressable`]) is exercised, but the row/col clamp itself
/// does not care.
#[must_use]
pub(crate) fn sample_clamped(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> u8 {
    let cy = y.clamp(0, h.saturating_sub(1).max(0));
    let cx = x.clamp(0, w.saturating_sub(1).max(0));
    let (Ok(uy), Ok(ux)) = (usize::try_from(cy), usize::try_from(cx)) else {
        return 0;
    };
    rows.get(uy).and_then(|r| r.get(ux)).copied().unwrap_or(0)
}

/// Bilinear sample of an 8-bit-unit plane at fractional coordinates,
/// clamp-to-edge at the border (consistent with [`sample_clamped`]). Used by
/// filters that need off-grid sampling along an arbitrary direction
/// ([`crate::dblur`]'s rotated line), which [`sample_clamped`]'s
/// nearest-pixel lookup cannot provide.
#[must_use]
pub(crate) fn sample_bilinear(rows: &[&[u8]], x: f64, y: f64, w: i32, h: i32) -> f64 {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let x0i = x0 as i32;
    let y0i = y0 as i32;
    let p00 = f64::from(sample_clamped(rows, x0i, y0i, w, h));
    let p10 = f64::from(sample_clamped(rows, x0i + 1, y0i, w, h));
    let p01 = f64::from(sample_clamped(rows, x0i, y0i + 1, w, h));
    let p11 = f64::from(sample_clamped(rows, x0i + 1, y0i + 1, w, h));
    let top = p00 * (1.0 - fx) + p10 * fx;
    let bottom = p01 * (1.0 - fx) + p11 * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Collect `plane`'s rows as borrowed slices, for repeated clamp-indexed
/// sampling by [`sample_clamped`]. Missing rows (never expected, but a frame
/// pool bug would rather show up as an empty slice than a panic) read as
/// all-zero via [`sample_clamped`]'s own fallback.
#[must_use]
pub(crate) fn collect_rows(plane: vaco_frame::PlaneRef<'_>, height: usize) -> Vec<&[u8]> {
    (0..height).map(|y| plane.row(y).unwrap_or(&[])).collect()
}

/// How a box average rounds its integer division.
///
/// [`boxblur`](crate::boxblur) and [`avgblur`](crate::avgblur) compute the
/// same `(2r+1)`-wide running average but were measured (their modules'
/// docs) to round the result differently — this is what lets one `box_pass`
/// serve both rather than two near-identical copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rounding {
    /// `boxblur`: round to nearest, ties away from zero.
    Nearest,
    /// `avgblur`: truncate toward zero.
    Trunc,
}

/// Round a box-average sum to a `u8`, per [`Rounding`]. Shared by the fast
/// separable path and the brute-force reference so the two cannot drift on
/// this half of the computation independently of the drift proptest checks.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "computing an average: the division is the whole point, not a \
              precision bug"
)]
fn round_avg(sum: i64, count: i64, rounding: Rounding) -> u8 {
    let value = if count > 0 {
        match rounding {
            Rounding::Nearest => {
                // Round-half-away-from-zero on a non-negative sum.
                (sum * 2 + count) / (count * 2)
            }
            Rounding::Trunc => sum / count,
        }
    } else {
        sum
    };
    u8::try_from(value.clamp(0, 255)).unwrap_or(255)
}

/// One clamp-bordered box average pass over a whole plane.
///
/// `rx`/`ry` are radii (window width `2*rx+1`, height `2*ry+1`). Measured
/// (see [`crate::boxblur`]'s doc) against a point impulse: the border is
/// extended by replicating the nearest edge sample, not by zero-padding.
///
/// A box filter is separable into an incremental (sliding-window) horizontal
/// pass followed by an incremental vertical pass: `O(w*h)` total rather than
/// the `O(w*h*(2rx+1)*(2ry+1))` a direct rectangle sum costs, which used to
/// be this function's whole body (kept below as [`box_pass_naive`], the
/// oracle [`tests::fast_path_agrees_with_naive_reference_everywhere`] checks
/// this against). Non-negative radii and positive dimensions are the only
/// shapes worth the sliding-window bookkeeping — every real caller in this
/// crate already rejects `radius <= 0` before reaching here (see
/// `boxblur`'s own `radius <= 0` early-out) — so any other combination
/// (negative radius, zero/negative `w`/`h`) falls back to the naive
/// reference directly rather than special-casing it in the fast path too.
#[must_use]
pub(crate) fn box_pass(
    rows: &[&[u8]],
    w: i32,
    h: i32,
    rx: i32,
    ry: i32,
    rounding: Rounding,
) -> Vec<Vec<u8>> {
    let (Ok(uw), Ok(uh)) = (usize::try_from(w), usize::try_from(h)) else {
        return box_pass_naive(rows, w, h, rx, ry, rounding);
    };
    if rx < 0 || ry < 0 || uw == 0 || uh == 0 {
        return box_pass_naive(rows, w, h, rx, ry, rounding);
    }
    let count = i64::from(2 * rx + 1) * i64::from(2 * ry + 1);

    // Horizontal pass: a running sum along each row, clamped at the row's
    // own edges exactly as `sample_clamped` would. `entering`/`leaving` are
    // the two samples the window gains/drops as `x` advances by one.
    let mut horiz: Vec<Vec<i64>> = Vec::new();
    for y in 0..uh {
        let row: &[u8] = rows.get(y).copied().unwrap_or(&[]);
        let clamped = |xi: i32| -> i64 {
            let cx = xi.clamp(0, w - 1);
            usize::try_from(cx)
                .ok()
                .and_then(|i| row.get(i))
                .map_or(0, |&v| i64::from(v))
        };
        let mut sum: i64 = (-rx..=rx).map(clamped).sum();
        let mut h_row = Vec::new();
        h_row.push(sum);
        for x in 1..uw {
            let xi = to_i32(x);
            sum += clamped(xi + rx) - clamped(xi - rx - 1);
            h_row.push(sum);
        }
        horiz.push(h_row);
    }

    // Vertical pass: keep one running sum per column, but visit rows in
    // storage order. The old column-major walk repeatedly jumped between
    // rows, making the hot loop pay for pointer chasing and cache misses.
    let mut out: Vec<Vec<u8>> = (0..uh).map(|_| vec![0u8; uw]).collect();
    let mut sums = vec![0i64; uw];
    let row_at = |yi: i32| -> &[i64] {
        let cy = yi.clamp(0, h - 1);
        usize::try_from(cy)
            .ok()
            .and_then(|i| horiz.get(i))
            .map_or(&[][..], Vec::as_slice)
    };
    for yi in -ry..=ry {
        for (sum, &sample) in sums.iter_mut().zip(row_at(yi).iter()) {
            *sum += sample;
        }
    }
    for y in 0..uh {
        if y != 0 {
            let yi = to_i32(y);
            let entering = row_at(yi + ry);
            let leaving = row_at(yi - ry - 1);
            for (sum, (&enter, &leave)) in sums.iter_mut().zip(entering.iter().zip(leaving.iter()))
            {
                *sum += enter - leave;
            }
        }
        if let Some(dst_row) = out.get_mut(y) {
            for (cell, &sum) in dst_row.iter_mut().zip(sums.iter()) {
                *cell = round_avg(sum, count, rounding);
            }
        }
    }
    out
}

/// The brute-force rectangle-sum reference [`box_pass`] used to be, kept as
/// the oracle its fast separable path is checked against (both the fixed
/// corner-impulse test and the proptest below) and as the fallback for the
/// radius/dimension shapes the fast path does not bother special-casing.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "computing an average: the division is the whole point, not a \
              precision bug"
)]
pub(crate) fn box_pass_naive(
    rows: &[&[u8]],
    w: i32,
    h: i32,
    rx: i32,
    ry: i32,
    rounding: Rounding,
) -> Vec<Vec<u8>> {
    let count = i64::from(2 * rx + 1) * i64::from(2 * ry + 1);
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let mut sum: i64 = 0;
            for dy in -ry..=ry {
                for dx in -rx..=rx {
                    sum += i64::from(sample_clamped(rows, x + dx, y + dy, w, h));
                }
            }
            row.push(round_avg(sum, count, rounding));
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The fast separable [`box_pass`] must agree with [`box_pass_naive`]
        /// pixel-for-pixel on every radius/size/rounding combination this
        /// generates -- including radii wider than the image itself, which
        /// exercises the clamp-to-edge wraparound on both passes at once.
        #[test]
        fn fast_path_agrees_with_naive_reference_everywhere(
            w in 1i32..12,
            h in 1i32..12,
            rx in 0i32..6,
            ry in 0i32..6,
            trunc in any::<bool>(),
            seed in any::<u64>(),
        ) {
            let rounding = if trunc { Rounding::Trunc } else { Rounding::Nearest };
            // A cheap deterministic PRNG (splitmix64) is enough here -- this
            // only needs *some* varied byte content, not statistical quality.
            let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
            let mut next_byte = || {
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                u8::try_from((z ^ (z >> 31)) & 0xFF).unwrap()
            };
            let uw = usize::try_from(w).unwrap();
            let uh = usize::try_from(h).unwrap();
            let img: Vec<Vec<u8>> = (0..uh)
                .map(|_| (0..uw).map(|_| next_byte()).collect())
                .collect();
            let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();

            let fast = box_pass(&rows, w, h, rx, ry, rounding);
            let naive = box_pass_naive(&rows, w, h, rx, ry, rounding);
            prop_assert_eq!(fast, naive);
        }
    }

    #[test]
    fn plane_selected_reads_the_expected_bits() {
        assert!(plane_selected(0b1111, 0));
        assert!(plane_selected(0b1111, 3));
        assert!(!plane_selected(0b0001, 1));
        assert!(plane_selected(0b0010, 1));
    }

    #[test]
    fn sample_clamped_replicates_the_edge() {
        let row0: &[u8] = &[1, 2, 3];
        let row1: &[u8] = &[4, 5, 6];
        let rows: [&[u8]; 2] = [row0, row1];
        assert_eq!(sample_clamped(&rows, -1, -1, 3, 2), 1);
        assert_eq!(sample_clamped(&rows, 3, 5, 3, 2), 6);
        assert_eq!(sample_clamped(&rows, 1, 0, 3, 2), 2);
    }

    /// Pinned against `ffmpeg -f lavfi -i
    /// "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,0)*eq(Y,0),255,0)'"
    /// -vf boxblur=luma_radius=1:luma_power=1` (2026-08-23): a 3x3,
    /// round-to-nearest box average with the border replicated, not
    /// zero-padded — the corner pixel's own neighbourhood sees four copies
    /// of itself.
    #[test]
    fn box_pass_matches_the_reference_on_a_corner_impulse() {
        let mut img = vec![vec![0u8; 5]; 5];
        if let Some(row) = img.first_mut()
            && let Some(px) = row.first_mut()
        {
            *px = 255;
        }
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = box_pass(&rows, 5, 5, 1, 1, Rounding::Nearest);
        assert_eq!(out[0][0], 113);
        assert_eq!(out[0][1], 57);
        assert_eq!(out[1][0], 57);
        assert_eq!(out[1][1], 28);
        assert_eq!(out[2][2], 0);
    }
}
