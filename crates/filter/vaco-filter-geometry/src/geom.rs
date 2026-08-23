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
use vaco_frame::Frame;
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

/// Copy a `cell_w`x`cell_h` region of `src` into `dst` at `(dst_x, dst_y)`,
/// plane by plane, subsampling-aware via `PixFmt::plane_width`/
/// `plane_height`.
///
/// A second, additive copy of `tile.rs`'s private `blit` — not a shared
/// export of it, since that one is not `pub(crate)` and touching an
/// already-shipped, tested filter to make it so is a larger risk than a
/// second ~30-line copy for a filter this crate did not have yet
/// (`framepack`'s `sbs`/`tab`/`lines`/`columns` packing). Any *third* caller
/// should factor both into this one rather than adding a fourth.
///
/// # Errors
/// Whatever [`plane_unit_bytes`] reports for an inaddressable plane.
pub(crate) fn blit(
    src: &Frame,
    dst: &mut Frame,
    format: PixFmt,
    dst_x: u32,
    dst_y: u32,
    cell_w: u32,
    cell_h: u32,
) -> Result<()> {
    for p in 0..format.plane_count() {
        let plane_idx = p as u8;
        let unit = plane_unit_bytes(format, plane_idx)?;
        let sx = format.plane_width(dst_x, plane_idx) as usize;
        let sy = format.plane_height(dst_y, plane_idx) as usize;
        let pw = format.plane_width(cell_w, plane_idx) as usize;
        let ph = format.plane_height(cell_h, plane_idx) as usize;
        let Some(src_plane) = src.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = dst.plane_mut(p) else {
            continue;
        };
        let row_bytes = pw.saturating_mul(unit);
        for row in 0..ph {
            let Some(src_row) = src_plane.row(row) else {
                continue;
            };
            let Some(src_slice) = src_row.get(..row_bytes.min(src_row.len())) else {
                continue;
            };
            if let Some(dst_row) = dst_plane.row_mut(sy.saturating_add(row)) {
                let start = sx.saturating_mul(unit);
                if let Some(dst_slice) = dst_row.get_mut(start..) {
                    let n = dst_slice.len().min(src_slice.len());
                    if let (Some(d), Some(s)) = (dst_slice.get_mut(..n), src_slice.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
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
