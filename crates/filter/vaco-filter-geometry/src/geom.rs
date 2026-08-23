//! Byte-level plane helpers shared by this crate's filters.
//!
//! Deliberately a smaller copy of `vaco-filter-video-geometry`'s own
//! `geom.rs` rather than a shared dependency: that module is
//! crate-private (`mod geom;`, not `pub mod`) in its crate, so nothing here
//! can reuse it without that crate exporting it — and D19/dup-check is about
//! not registering the same *filter* twice, not about every byte-mover helper
//! living in one place. Keeping this crate's copy small and re-deriving it
//! from the same public `vaco-pixfmt` API is cheaper than requesting an
//! export from a crate this agent does not own.

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Bytes spanned by one sample position in `plane`.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] if `plane` has no addressable components.
pub(crate) fn plane_unit_bytes(format: PixFmt, plane: u8) -> Result<usize> {
    format
        .descriptor()
        .components
        .iter()
        .find(|c| c.plane == plane)
        .map(|c| c.step as usize)
        .ok_or(Error::Unsupported("plane has no addressable components"))
}

/// Reject pixel formats these byte-mover filters cannot handle.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn yuv420p_unit_bytes_is_one_per_plane() {
        assert_eq!(plane_unit_bytes(PixFmt::Yuv420p, 0).unwrap(), 1);
    }
}
