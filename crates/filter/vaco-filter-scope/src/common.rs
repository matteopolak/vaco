//! Shared 8-bit plane helpers for this crate's filters — the same small
//! per-crate fork every T2/T3 filter crate in this family carries (D19
//! governs shared *types*, not these tiny predicates; see
//! `vaco-filter-convolve::common`'s doc for the same call).

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Reject formats this crate's byte-level, 8-bit-only pixel math cannot
/// address.
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
            "vaco-filter-scope only filters 8-bit samples",
        ));
    }
    Ok(())
}

/// `u32`/`usize` to `i32`, saturating rather than wrapping.
#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Decode the reference's `components`/`planes`-style bitmask.
#[must_use]
pub(crate) const fn plane_selected(mask: i64, plane: u8) -> bool {
    (mask >> plane) & 1 != 0
}
