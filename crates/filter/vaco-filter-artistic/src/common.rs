//! Shared 8-bit plane helpers for this crate's two filters.
//!
//! A deliberate, small fork of the same helpers `vaco-filter-convolve::common`
//! and `vaco-filter-video-geometry`'s equivalent carry — D19 governs shared
//! *types*, not tiny format-flag predicates every crate in this family
//! independently needs, and each of those crates' own doc comments make the
//! same call for the same reason.

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
            "vaco-filter-artistic only filters 8-bit samples",
        ));
    }
    Ok(())
}

/// `u32`/`usize` to `i32`, saturating rather than wrapping. Frame dimensions
/// in this crate never approach `i32::MAX`; this avoids `clippy::cast_possible_wrap`
/// at every call site, matching `vaco-filter-convolve::common::to_i32`.
#[must_use]
pub(crate) fn to_i32<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Copy every metadata field a frame carries besides its pixel data.
pub(crate) fn copy_frame_meta(out: &mut Frame, input: &Frame) {
    out.pts = input.pts;
    out.time_base = input.time_base;
    out.duration = input.duration;
    out.color = input.color;
    out.flags = input.flags;
    out.sample_aspect_ratio = input.sample_aspect_ratio;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_i32_saturates_rather_than_wraps() {
        assert_eq!(to_i32(u32::MAX), i32::MAX);
        assert_eq!(to_i32(10u32), 10);
    }
}
