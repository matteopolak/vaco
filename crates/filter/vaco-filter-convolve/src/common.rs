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
//! before the plan's own crate boundary (`planning/16-filters.md` §4.2)
//! split them apart.

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
}
