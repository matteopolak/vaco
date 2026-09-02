//! Shared 8-bit plane helpers for this crate's filters — the same small
//! per-crate fork every T2/T3 filter crate in this family carries (D19
//! governs shared *types*, not these tiny predicates; see
//! `vaco-filter-convolve::common`'s doc for the same call).

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

use crate::font8x8::{self, GLYPH_H, GLYPH_W};

/// Blit one ASCII byte's glyph into `plane`, foreground `255`, background
/// left untouched (the caller's canvas is expected to already be a solid
/// colour, matching every filter in this family's "fresh canvas per frame"
/// rule — see `datascope`'s module doc).
///
/// Moved here from `datascope` (D19) when `graphmonitor`/`agraphmonitor`
/// needed the identical byte-per-pixel blit for their own, variable-width
/// text lines rather than `datascope`'s fixed value grid.
pub(crate) fn draw_glyph(rows: &mut [&mut [u8]], top: u32, left: u32, ch: u8) {
    for row in 0..GLYPH_H {
        let Some(y) = top
            .checked_add(row as u32)
            .and_then(|v| usize::try_from(v).ok())
        else {
            continue;
        };
        let Some(dst) = rows.get_mut(y) else { continue };
        for col in 0..GLYPH_W {
            if !font8x8::glyph_pixel(ch, row, col) {
                continue;
            }
            let Some(x) = left
                .checked_add(col as u32)
                .and_then(|v| usize::try_from(v).ok())
            else {
                continue;
            };
            if let Some(px) = dst.get_mut(x) {
                *px = 255;
            }
        }
    }
}

/// Draw an already-formatted ASCII byte string starting at `(left, top)`,
/// one glyph every [`GLYPH_W`] pixels — `datascope`'s fixed-width number
/// grid and `graphmonitor`'s variable-width label/counter lines are both
/// this, just fed different byte strings.
pub(crate) fn draw_text(rows: &mut [&mut [u8]], top: u32, left: u32, text: &[u8]) {
    for (i, &ch) in text.iter().enumerate() {
        let x = left + u32::try_from(i).unwrap_or(0) * GLYPH_W as u32;
        draw_glyph(rows, top, x, ch);
    }
}

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
