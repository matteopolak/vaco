//! Shared 8-bit plane helpers, a small fork of the same predicate every
//! byte-level filter crate in this tree carries independently (see
//! `vaco-filter-artistic::common`'s own doc for why this is not shared).

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
        return Err(Error::Unsupported("cannot address a sub-byte-packed format"));
    }
    if format.has(PixFmtFlags::PALETTE) {
        return Err(Error::Unsupported("cannot address a palette format without its side table"));
    }
    if format.max_depth() != 8 {
        return Err(Error::Unsupported("vaco-filter-motion only filters 8-bit samples"));
    }
    Ok(())
}

#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}
