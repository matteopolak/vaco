//! Expand a decoded [`IndexedBitmap`] to packed RGBA8 bytes, row-major, one
//! [`vaco_core::Rgba`] per pixel.
//!
//! This is not a step any of the three decoders need internally — every
//! format composites and blits at the palette-index level. It exists for a
//! caller (a renderer, or this crate's own differential tests) that wants
//! plain pixels, and for pixel-level comparison against a reference decoder
//! that also expands to RGBA before diffing.

use vaco_core::{Error, Result, Rgba};
use vaco_limits::Budget;

use crate::IndexedBitmap;

/// `bitmap.rect().width * height * 4` bytes. An index past the palette's own
/// declared length paints as [`Rgba::TRANSPARENT`], matching
/// [`vaco_format_subtitle_bitmap::Palette::get`]'s own documented
/// convention.
///
/// # Errors
/// [`Error::InvalidData`] if the pixel count overflows a `u64` or does not
/// fit `usize` on this platform; otherwise whatever [`Budget::alloc`]
/// reports.
pub fn to_rgba(budget: &mut Budget, bitmap: &IndexedBitmap) -> Result<Vec<u8>> {
    let area = bitmap
        .rect()
        .area()
        .ok_or(Error::InvalidData("subtitle bitmap: rect area overflows"))?;
    let byte_len = area.checked_mul(4).ok_or(Error::InvalidData(
        "subtitle bitmap: rgba byte count overflows",
    ))?;
    let len = usize::try_from(byte_len)
        .map_err(|_| Error::InvalidData("subtitle bitmap: too large for this platform"))?;
    let mut out = budget.alloc::<u8>(len)?;
    for (i, &index) in bitmap.indices().iter().enumerate() {
        let colour = bitmap.palette().get(index).unwrap_or(Rgba::TRANSPARENT);
        let start = i.saturating_mul(4);
        let Some(slot) = out.get_mut(start..start.saturating_add(4)) else {
            continue;
        };
        slot.copy_from_slice(&[colour.r, colour.g, colour.b, colour.a]);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::{Palette, Rect};
    use vaco_limits::Limits;

    #[test]
    fn expands_two_indices_to_their_palette_colours() {
        let limits = Limits::strict();
        let rect = Rect::new(0, 0, 2, 1, &limits).unwrap();
        let palette = Palette::new(vec![Rgba::new(1, 2, 3, 4), Rgba::new(5, 6, 7, 8)]).unwrap();
        let bitmap = IndexedBitmap::new(rect, palette, vec![0, 1]).unwrap();
        let mut budget = Budget::new(limits);
        let rgba = to_rgba(&mut budget, &bitmap).unwrap();
        assert_eq!(rgba, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn an_index_past_the_palette_paints_transparent() {
        let limits = Limits::strict();
        let rect = Rect::new(0, 0, 1, 1, &limits).unwrap();
        let palette = Palette::new(vec![]).unwrap();
        let bitmap = IndexedBitmap::new(rect, palette, vec![7]).unwrap();
        let mut budget = Budget::new(limits);
        let rgba = to_rgba(&mut budget, &bitmap).unwrap();
        assert_eq!(rgba, vec![0, 0, 0, 0]);
    }
}
