//! The shape every bitmap subtitle format's decoder aims at: a rectangle of
//! palette indices.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::palette::Palette;
use crate::rect::Rect;

/// A decompressed subtitle bitmap: a [`Rect`] of pixels, each an index into a
/// [`Palette`].
///
/// This is the **decoder's** output shape (see the crate docs on the
/// demuxer/decoder line): none of the four demuxers in `vaco-subtitle-bitmap`
/// construct one from real pixel data, because doing that means running the
/// format's run-length decompressor, which is decoder work belonging to
/// `crates/codec/`, a later wave. What they *do* construct directly, wherever
/// a container states it as a plain uncompressed field, are the [`Rect`] and
/// [`Palette`] this type is built from — see [`crate`]'s module docs for the
/// three concrete places that happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedBitmap {
    rect: Rect,
    palette: Palette,
    indices: Vec<u8>,
}

impl IndexedBitmap {
    /// # Errors
    /// [`Error::InvalidData`] if `rect`'s area overflows, or if
    /// `indices.len()` does not equal it.
    pub fn new(rect: Rect, palette: Palette, indices: Vec<u8>) -> Result<Self> {
        let area = rect
            .area()
            .ok_or(Error::InvalidData("bitmap: rect area overflows"))?;
        let len = u64::try_from(indices.len()).unwrap_or(u64::MAX);
        if len != area {
            return Err(Error::InvalidData(
                "bitmap: pixel index count does not match rect area",
            ));
        }
        Ok(Self {
            rect,
            palette,
            indices,
        })
    }

    /// A zero-filled bitmap of `rect`'s area, sized through `budget` rather
    /// than directly from an attacker-controlled [`Rect`] — the two-phase
    /// reserve/alloc [`vaco_limits::Budget`] exists for. A decoder calls this
    /// once it has validated the rectangle, then fills `indices_mut()` in.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `rect`'s area overflows this platform's
    /// `usize`; otherwise whatever [`Budget::alloc`] reports.
    pub fn allocate(budget: &mut Budget, rect: Rect, palette: Palette) -> Result<Self> {
        let area = rect
            .area()
            .ok_or(Error::InvalidData("bitmap: rect area overflows"))?;
        let len = usize::try_from(area)
            .map_err(|_| Error::InvalidData("bitmap: rect area too large for this platform"))?;
        let indices = budget.alloc::<u8>(len)?;
        Ok(Self {
            rect,
            palette,
            indices,
        })
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    #[must_use]
    pub fn indices(&self) -> &[u8] {
        &self.indices
    }

    /// Mutable access to the pixel indices, for a decoder filling in a bitmap
    /// built with [`IndexedBitmap::allocate`].
    pub fn indices_mut(&mut self) -> &mut [u8] {
        &mut self.indices
    }

    /// The palette index at `(x, y)`, or `None` outside the rectangle.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.rect.width || y >= self.rect.height {
            return None;
        }
        let row = u64::from(y).checked_mul(u64::from(self.rect.width))?;
        let at = row.checked_add(u64::from(x))?;
        let at = usize::try_from(at).ok()?;
        self.indices.get(at).copied()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn new_rejects_a_mismatched_index_count() {
        let limits = Limits::strict();
        let rect = Rect::new(0, 0, 2, 2, &limits).unwrap();
        let palette = Palette::new(vec![]).unwrap();
        assert!(IndexedBitmap::new(rect, palette, vec![0u8; 3]).is_err());
    }

    #[test]
    fn pixel_reads_row_major_and_bounds_checks() {
        let limits = Limits::strict();
        let rect = Rect::new(0, 0, 2, 2, &limits).unwrap();
        let palette = Palette::new(vec![]).unwrap();
        let bmp = IndexedBitmap::new(rect, palette, vec![10, 11, 12, 13]).unwrap();
        assert_eq!(bmp.pixel(0, 0), Some(10));
        assert_eq!(bmp.pixel(1, 0), Some(11));
        assert_eq!(bmp.pixel(0, 1), Some(12));
        assert_eq!(bmp.pixel(1, 1), Some(13));
        assert_eq!(bmp.pixel(2, 0), None);
        assert_eq!(bmp.pixel(0, 2), None);
    }

    #[test]
    fn allocate_sizes_from_the_budget_and_zero_fills() {
        let mut budget = Budget::new(Limits::strict());
        let rect = Rect::new(0, 0, 4, 4, &Limits::strict()).unwrap();
        let bmp =
            IndexedBitmap::allocate(&mut budget, rect, Palette::new(vec![]).unwrap()).unwrap();
        assert_eq!(bmp.indices().len(), 16);
        assert!(bmp.indices().iter().all(|&b| b == 0));
    }

    #[test]
    fn allocate_over_the_alloc_cap_is_rejected_even_though_each_axis_is_in_bounds() {
        // The nuance `Rect::new`'s per-axis check alone does not catch: a
        // rectangle whose *area* is enormous can still have each axis within
        // `max_dimension` (e.g. under `Limits::permissive`'s 65536 cap). The
        // defence in depth is here, at allocation time, via `Budget`.
        let limits = Limits::tiny();
        let mut budget = Budget::new(limits.clone());
        let rect = Rect::new(0, 0, limits.max_dimension, limits.max_dimension, &limits).unwrap();
        assert!(IndexedBitmap::allocate(&mut budget, rect, Palette::new(vec![]).unwrap()).is_err());
    }
}
