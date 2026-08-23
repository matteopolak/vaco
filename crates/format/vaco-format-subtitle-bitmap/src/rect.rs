//! A subtitle bitmap's on-screen bounding box.

use vaco_core::{Error, Result};
use vaco_limits::Limits;

/// The position and size of a bitmap region, in pixels, relative to the
/// subtitle canvas's top-left corner.
///
/// Every field a container states this from — a DVB `region_width`, a PGS
/// object `width`, a `VobSub` `.idx` `size:` line — is attacker-controlled, so
/// the only way to build one is [`Rect::new`], which checks it against a
/// [`Limits`] budget before anything downstream allocates a pixel buffer from
/// it. A 65535×65535 rectangle parses fine as two `u16`s; it does not parse as
/// a [`Rect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl Rect {
    /// A validated rectangle: `width`/`height` within `limits.max_dimension`,
    /// and `x + width` / `y + height` representable in a `u32`.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if either axis exceeds `limits.max_dimension`;
    /// [`Error::InvalidData`] if the position would overflow.
    pub fn new(x: u32, y: u32, width: u32, height: u32, limits: &Limits) -> Result<Self> {
        if width > limits.max_dimension {
            return Err(Error::LimitExceeded {
                limit: "subtitle_bitmap_width",
                requested: u64::from(width),
                cap: u64::from(limits.max_dimension),
            });
        }
        if height > limits.max_dimension {
            return Err(Error::LimitExceeded {
                limit: "subtitle_bitmap_height",
                requested: u64::from(height),
                cap: u64::from(limits.max_dimension),
            });
        }
        x.checked_add(width).ok_or(Error::InvalidData(
            "subtitle bitmap rect: x + width overflows",
        ))?;
        y.checked_add(height).ok_or(Error::InvalidData(
            "subtitle bitmap rect: y + height overflows",
        ))?;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// `width * height`, checked. Two `u32`s always fit the product in a
    /// `u64`, but nothing downstream should have to re-derive that.
    #[must_use]
    pub fn area(&self) -> Option<u64> {
        u64::from(self.width).checked_mul(u64::from(self.height))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_rect_wider_than_the_limit() {
        let limits = Limits::strict();
        assert!(Rect::new(0, 0, limits.max_dimension + 1, 1, &limits).is_err());
    }

    #[test]
    fn rejects_a_rect_taller_than_the_limit() {
        let limits = Limits::strict();
        assert!(Rect::new(0, 0, 1, limits.max_dimension + 1, &limits).is_err());
    }

    #[test]
    fn accepts_a_rect_at_the_limit() {
        let limits = Limits::strict();
        assert!(Rect::new(0, 0, limits.max_dimension, limits.max_dimension, &limits).is_ok());
    }

    #[test]
    fn a_65535_by_65535_rectangle_is_rejected_under_library_defaults() {
        // The exact shape of finding `planning/AGENT-CONSTRAINTS.md` calls out.
        // `Limits::strict` — what an embedder gets by default, per
        // `vaco-limits`'s own docs — caps a single axis at 8192, well under
        // what a `u16` width/height field can claim.
        let limits = Limits::strict();
        assert!(limits.max_dimension < 65_535);
        assert!(Rect::new(0, 0, 65_535, 65_535, &limits).is_err());
    }

    #[test]
    fn area_does_not_overflow_for_the_largest_permitted_rect() {
        let limits = Limits::permissive();
        let r = Rect::new(0, 0, limits.max_dimension, limits.max_dimension, &limits).unwrap();
        assert!(r.area().is_some());
    }

    #[test]
    fn position_overflow_is_rejected() {
        let limits = Limits::permissive();
        assert!(Rect::new(u32::MAX, 0, 1, 1, &limits).is_err());
    }
}
