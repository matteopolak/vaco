//! Shared helpers this crate's filters carry their own copy of (D19
//! governs shared *types*, not these tiny per-crate predicates — the same
//! call every other T2/T3 filter crate in this project makes).

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
            "vaco-filter-overlay only filters 8-bit samples",
        ));
    }
    Ok(())
}

/// `u32`/`usize` to `i32`, saturating rather than wrapping.
#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Clamp a signed intermediate to a byte.
#[must_use]
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "clamp(0, 255) always lands in u8's range"
)]
pub(crate) fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
