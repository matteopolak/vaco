//! Shared 8-bit plane helpers for this crate's neighbourhood filters.
//!
//! # Scope: 8-bit addressable formats only
//!
//! Every filter in this crate rejects any format wider than 8 bits per
//! component, exactly as `vaco-filter-video-composite::geom::ensure_addressable_8bit`
//! does and for the same reason: the reference supports higher bit depths for
//! most of these filters, but implementing generic sample-width math is a
//! separate, larger effort than this crate's brief budgets for. This is a
//! recorded, deliberate gap (see `docs/filter/vaco-filter-convolve.md`), not
//! a silent one.
//!
//! Not reused from `vaco-filter-blur`, `vaco-filter-video-composite` or
//! `vaco-filter-video-geometry`: all three crates' equivalents are
//! `pub(crate)`, and D19 governs shared *types*, not tiny format-flag
//! predicates that several crates independently need — the geometry
//! crate's own doc comment for its copy makes the same call. This module
//! is a deliberate byte-for-byte fork of `vaco-filter-blur::common`'s
//! non-`box_pass` half, from when both filter families shared one crate
//! before the crate boundary split them apart.

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
            "vaco-filter-convolve only filters 8-bit samples",
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
/// track by name but that is still worth not typing out nine times.
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
/// coordinates, for the filters in this crate measured to extend the
/// border that way (`dilation`/`erosion`, `median`; see each module's doc
/// for the probe that pinned this down). [`crate::convolution`]'s own
/// engine — reused by `sobel`/`prewitt`/`scharr` — does **not** use this:
/// see that module's doc for the measured "force zero" rule instead.
#[must_use]
pub(crate) fn sample_clamped(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> u8 {
    let cy = y.clamp(0, h.saturating_sub(1).max(0));
    let cx = x.clamp(0, w.saturating_sub(1).max(0));
    let (Ok(uy), Ok(ux)) = (usize::try_from(cy), usize::try_from(cx)) else {
        return 0;
    };
    rows.get(uy).and_then(|r| r.get(ux)).copied().unwrap_or(0)
}

/// Reflect an out-of-range index into `[0, n)` without duplicating the
/// edge sample (`reflect-101`/`gDL_BORDER_REFLECT_101`: index `-1` maps to
/// `1`, not `0`; index `n` maps to `n-2`, not `n-1`). `n <= 1` collapses
/// everything to `0` (there is nothing to mirror off of).
///
/// Pinned for [`crate::convolution::Kernel::value_at`] (see that module's
/// doc for the corner/edge probe that ruled out zero-pad and plain
/// clamp-to-edge in favour of this rule specifically).
#[must_use]
fn reflect_101(index: i32, n: i32) -> i32 {
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let m = index.rem_euclid(period);
    if m >= n { period - m } else { m }
}

/// Sample an 8-bit-unit plane at signed coordinates using the `reflect-101`
/// border extension (mirror without duplicating the edge pixel), applied
/// independently per axis — including simultaneously at a corner, where
/// both axes are out of range at once. See [`reflect_101`] and
/// [`crate::convolution::Kernel::value_at`]'s doc for the measurement that
/// pinned this down.
#[must_use]
pub(crate) fn sample_reflect101(rows: &[&[u8]], x: i32, y: i32, w: i32, h: i32) -> u8 {
    let ry = reflect_101(y, h);
    let rx = reflect_101(x, w);
    let (Ok(uy), Ok(ux)) = (usize::try_from(ry), usize::try_from(rx)) else {
        return 0;
    };
    rows.get(uy).and_then(|r| r.get(ux)).copied().unwrap_or(0)
}

/// Collect `plane`'s rows as borrowed slices, for repeated clamp-indexed
/// sampling by [`sample_clamped`]. Missing rows (never expected, but a frame
/// pool bug would rather show up as an empty slice than a panic) read as
/// all-zero via [`sample_clamped`]'s own fallback.
#[must_use]
pub(crate) fn collect_rows(plane: vaco_frame::PlaneRef<'_>, height: usize) -> Vec<&[u8]> {
    (0..height).map(|y| plane.row(y).unwrap_or(&[])).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

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

    #[test]
    fn reflect_101_mirrors_without_duplicating_the_edge() {
        // n=5: valid indices 0..=4. -1 mirrors to 1 (not 0), 5 mirrors to
        // 3 (not 4) — the "101" distinction from plain clamp/replicate.
        assert_eq!(reflect_101(-1, 5), 1);
        assert_eq!(reflect_101(-2, 5), 2);
        assert_eq!(reflect_101(5, 5), 3);
        assert_eq!(reflect_101(6, 5), 2);
        assert_eq!(reflect_101(2, 5), 2);
        assert_eq!(reflect_101(0, 5), 0);
        assert_eq!(reflect_101(4, 5), 4);
    }

    #[test]
    fn sample_reflect101_matches_a_measured_corner_probe() {
        // Real 5x5 source (value = 1 + 10*row + col), and the exact
        // `sobel` output ffmpeg 8.1 produces at the corner and its
        // neighbour — see `convolution`'s doc for the full derivation.
        // This pins the per-axis reflect-101 rule independently of any
        // particular filter's kernel math.
        let rows: [&[u8]; 5] = [
            &[1, 2, 3, 4, 5],
            &[11, 12, 13, 14, 15],
            &[21, 22, 23, 24, 25],
            &[31, 32, 33, 34, 35],
            &[41, 42, 43, 44, 45],
        ];
        // Corner tap (-1,-1) reflects to (1,1) = 12.
        assert_eq!(sample_reflect101(&rows, -1, -1, 5, 5), 12);
        // Edge tap (-1, 0) reflects x only, to (1, 0) = 2.
        assert_eq!(sample_reflect101(&rows, -1, 0, 5, 5), 2);
        // In-bounds sample is untouched.
        assert_eq!(sample_reflect101(&rows, 2, 2, 5, 5), 23);
    }
}
