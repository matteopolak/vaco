//! Shared helpers this crate's three filters carry their own copy of (D19
//! governs shared *types*, not these tiny per-crate predicates — the same
//! call every other T2/T3 filter crate in this project makes).

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Reject formats this crate's byte-level plane copies cannot address.
///
/// Deliberately not 8-bit-only: concatenating rows is a pure byte move that
/// works at any bit depth `PlaneMut::row`/`PlaneRef::row` can address, so
/// restricting to 8-bit the way this project's pixel-*math* filters do would
/// be a narrower claim than the implementation actually needs.
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

/// `u32`/`usize` to `i32`, saturating rather than wrapping.
#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}
