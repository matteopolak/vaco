//! Byte-level plane manipulation shared by `crop`, `pad`, `flip` and
//! `transpose`.
//!
//! None of these filters need to interpret a sample's *value* — only move
//! bytes around — so they operate generically on any byte-addressable pixel
//! format via [`plane_unit_bytes`] rather than special-casing each format's
//! component layout. That is the same generality argument `vaco-pixfmt`'s own
//! docs make for `PlaneRef`/`PlaneMut`.

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Bytes spanned by one sample position in `plane` — the stride between
/// consecutive pixels (or pixel groups, for a packed format) in that plane.
///
/// Derived from the component table rather than from an allocated frame: a
/// packed plane's components share one `step` (that is what "packed" means),
/// and a planar plane has exactly one component, so "the step of any
/// component in this plane" is well defined either way.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] if `plane` has no components (an
/// out-of-range index) or the format has already failed [`ensure_addressable`].
pub(crate) fn plane_unit_bytes(format: PixFmt, plane: u8) -> Result<usize> {
    format
        .descriptor()
        .components
        .iter()
        .find(|c| c.plane == plane)
        .map(|c| c.step as usize)
        .ok_or(Error::Unsupported("plane has no addressable components"))
}

/// Reject pixel formats these byte-mover filters cannot handle: no accessible
/// bytes ([`PixFmtFlags::HW_ACCEL`]), sub-byte packing
/// ([`PixFmtFlags::BITSTREAM`]), or a plane of palette indices needing a
/// side table this crate never sees ([`PixFmtFlags::PALETTE`]).
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] naming which property is the problem.
pub(crate) fn ensure_addressable(format: PixFmt) -> Result<()> {
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
    Ok(())
}

/// The chroma-subsampling divisor pair `(1 << log2_w, 1 << log2_h)` for
/// `format`. `(1, 1)` for any format with no subsampling (RGB, 4:4:4, gray).
#[must_use]
pub(crate) fn subsample_factors(format: PixFmt) -> (u32, u32) {
    let (w, h) = format.log2_chroma();
    (1u32 << w, 1u32 << h)
}

/// Round `value` down to the nearest multiple of `factor` (`factor` a power
/// of two from [`subsample_factors`]). `factor <= 1` is a no-op.
#[must_use]
pub(crate) const fn floor_to_multiple(value: u32, factor: u32) -> u32 {
    if factor <= 1 {
        value
    } else {
        value - (value % factor)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn floor_to_multiple_is_a_true_floor() {
        assert_eq!(floor_to_multiple(7, 2), 6);
        assert_eq!(floor_to_multiple(6, 2), 6);
        assert_eq!(floor_to_multiple(0, 2), 0);
        assert_eq!(floor_to_multiple(7, 1), 7);
    }

    #[test]
    fn yuv420p_unit_bytes_is_one_per_plane() {
        assert_eq!(plane_unit_bytes(PixFmt::Yuv420p, 0).unwrap(), 1);
        assert_eq!(plane_unit_bytes(PixFmt::Yuv420p, 1).unwrap(), 1);
    }

    #[test]
    fn rgb24_unit_bytes_is_three() {
        assert_eq!(plane_unit_bytes(PixFmt::Rgb24, 0).unwrap(), 3);
    }

    #[test]
    fn nv12_chroma_unit_bytes_is_two() {
        assert_eq!(plane_unit_bytes(PixFmt::Nv12, 1).unwrap(), 2);
    }
}
