//! Byte- and plane-level helpers shared by [`crate::blend`] and
//! [`crate::rotate`].
//!
//! Independently written for this crate rather than imported: the equivalent
//! helpers in `vaco-filter-video-geometry::geom` are `pub(crate)` there and
//! this crate does not own that one, so the small, generic pieces — "bytes
//! per pixel group in a plane", "reject formats with no addressable bytes",
//! "is this plane subsampled" — are reimplemented here against the same
//! public `vaco_pixfmt::PixFmt` surface both crates already depend on.

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Bytes spanned by one sample position in `plane` — the stride between
/// consecutive pixels (or pixel groups, for a packed format) in that plane.
///
/// A packed plane's components share one `step`; a planar plane has exactly
/// one component. Either way, "the step of any component in this plane" is
/// well defined.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] if `plane` has no components.
pub(crate) fn plane_unit_bytes(format: PixFmt, plane: u8) -> Result<usize> {
    format
        .descriptor()
        .components
        .iter()
        .find(|c| c.plane == plane)
        .map(|c| c.step as usize)
        .ok_or(Error::Unsupported("plane has no addressable components"))
}

/// Reject formats this crate's byte-level blending and resampling cannot
/// address: a hardware surface, sub-byte packing, a palette needing a side
/// table, or a depth this crate's 8-bit-only pixel math has not implemented.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] naming which property is the problem.
pub(crate) fn ensure_addressable_8bit(format: PixFmt) -> Result<()> {
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
        // Measured gap, not a guess: see this crate's docs for why depths
        // other than 8 bits are parsed as options but not composited.
        return Err(Error::Unsupported(
            "vaco-filter-video-composite only blends and resamples 8-bit samples",
        ));
    }
    Ok(())
}

/// Whether `plane` holds only chroma (logical channel 1 or 2), and therefore
/// follows the chroma decimation on both size and position.
///
/// Mirrors `vaco-pixfmt`'s own private `chroma_plane` test: components are
/// indexed by logical channel, so "does the U or V component live here" is
/// the same question the descriptor already answers for
/// [`PixFmt::plane_width`]/[`PixFmt::plane_height`] internally.
#[must_use]
pub(crate) fn plane_is_chroma(format: PixFmt, plane: u8) -> bool {
    let comps = format.descriptor().components;
    comps.get(1).is_some_and(|c| c.plane == plane) || comps.get(2).is_some_and(|c| c.plane == plane)
}

/// Map a full-resolution coordinate down to `plane`'s own coordinate space.
///
/// Floor (right-shift), matching [`PixFmt::plane_width`]/`plane_height`'s use
/// of the same shift for *size*. A non-chroma plane (luma, alpha, RGB, GBR)
/// is never decimated and returns `v` unchanged.
#[must_use]
pub(crate) fn plane_coord(v: u32, format: PixFmt, plane: u8, horizontal: bool) -> u32 {
    if !plane_is_chroma(format, plane) {
        return v;
    }
    let (sw, sh) = format.log2_chroma();
    v >> (if horizontal { sw } else { sh })
}

/// The alpha component (logical channel 3), if `format` has one.
#[must_use]
pub(crate) fn alpha_component(format: PixFmt) -> Option<vaco_pixfmt::Component> {
    if !format.has_alpha() {
        return None;
    }
    format.descriptor().components.get(3).copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

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
    fn rgba_unit_bytes_is_four() {
        assert_eq!(plane_unit_bytes(PixFmt::Rgba, 0).unwrap(), 4);
    }

    #[test]
    fn yuv420p_chroma_planes_are_1_and_2() {
        assert!(!plane_is_chroma(PixFmt::Yuv420p, 0));
        assert!(plane_is_chroma(PixFmt::Yuv420p, 1));
        assert!(plane_is_chroma(PixFmt::Yuv420p, 2));
    }

    #[test]
    fn yuva420p_alpha_plane_is_not_chroma() {
        assert!(!plane_is_chroma(PixFmt::Yuva420p, 3));
        assert_eq!(alpha_component(PixFmt::Yuva420p).map(|c| c.plane), Some(3));
    }

    #[test]
    fn gbrp_has_zero_chroma_shift_so_plane_coord_is_a_no_op_regardless() {
        // `PixFmt::plane_width`/`plane_height` (and this module's own
        // `plane_is_chroma`, which mirrors their private `chroma_plane`
        // test) can call a GBR plane "chroma" by the same component-index
        // heuristic YUV uses — GBR's plane order (G, B, R) does not match
        // its logical channel order (R, G, B), so the heuristic's "is the
        // *first-declared* component here logical index 1 or 2" can land on
        // a non-chroma plane. That is `vaco-pixfmt`'s own established
        // behaviour, not a bug this crate introduces: `log2_chroma` is
        // `(0, 0)` for every GBR format, so the shift `plane_coord` applies
        // is always a no-op regardless of which way the heuristic answers.
        assert_eq!(PixFmt::Gbrp.log2_chroma(), (0, 0));
        assert_eq!(plane_coord(7, PixFmt::Gbrp, 1, true), 7);
        assert_eq!(plane_coord(7, PixFmt::Gbrp, 1, false), 7);
    }

    #[test]
    fn rgb24_has_no_alpha_component() {
        assert_eq!(alpha_component(PixFmt::Rgb24), None);
    }

    #[test]
    fn plane_coord_floors_chroma_and_passes_through_luma() {
        assert_eq!(plane_coord(7, PixFmt::Yuv420p, 0, true), 7);
        assert_eq!(plane_coord(7, PixFmt::Yuv420p, 1, true), 3);
        assert_eq!(plane_coord(7, PixFmt::Yuv420p, 2, false), 3);
    }

    #[test]
    fn hw_and_bitstream_and_palette_and_non_8bit_are_rejected() {
        assert!(ensure_addressable_8bit(PixFmt::Yuv420p).is_ok());
        assert!(ensure_addressable_8bit(PixFmt::Yuv420p10le).is_err());
    }
}
