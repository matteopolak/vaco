//! The reference-frame slots motion compensation reads from.
//!
//! Only frame pictures are decoded, so "reference field"
//! never means a genuinely separate field-coded picture here — §7.6.2's
//! field-prediction-within-a-frame-picture case, which real interlaced
//! `mpeg2video` encodes use heavily (`-flags +ilme+ildct`), reads the top
//! or bottom rows of a whole decoded frame instead.

use vaco_frame::Frame;

/// One decoded frame kept around only to be read from, plus the sample
/// values a border read needs when a motion vector points outside it —
/// §7.6.3.8 forbids a conforming bitstream from doing this, but nothing
/// stops an adversarial one, and §7.6.3.9's concealment-motion-vector note
/// says a decoder should expect it in practice for the row below a
/// bottom-row macroblock.
#[derive(Debug, Clone)]
pub(crate) struct RefPicture {
    frame: Frame,
}

impl RefPicture {
    #[must_use]
    pub(crate) const fn new(frame: Frame) -> Self {
        Self { frame }
    }

    /// One luma or chroma sample at `(x, y)` in plane `plane`, clamped to
    /// the plane's own border on any out-of-range coordinate (including a
    /// negative one — `x`/`y` are `i32` because a motion vector's
    /// destination routinely is, right up to the point a sample is
    /// actually fetched).
    #[must_use]
    pub(crate) fn sample(&self, plane: usize, x: i32, y: i32) -> u8 {
        let Some(p) = self.frame.plane(plane) else {
            return 0;
        };
        let rows = p.rows().max(1);
        let yy = y.clamp(0, i32::try_from(rows).unwrap_or(i32::MAX) - 1);
        let Some(row) = p.row(usize::try_from(yy).unwrap_or(0)) else {
            return 0;
        };
        let cols = row.len().max(1);
        let xx = x.clamp(0, i32::try_from(cols).unwrap_or(i32::MAX) - 1);
        row.get(usize::try_from(xx).unwrap_or(0)).copied().unwrap_or(0)
    }
}
