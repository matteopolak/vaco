//! The single reference-frame slot both formats' motion compensation reads
//! from. Neither H.261 nor baseline H.263 has B-pictures, so there is only
//! ever one reference (the most recently decoded picture) — no
//! `previous`/`recent`/`held` triple like `vaco-codec-mpeg12`'s B-picture
//! reordering needs.

use vaco_frame::Frame;

/// One decoded frame kept around only to be read from, plus border
/// clamping for a motion vector that points outside it (never valid in a
/// conforming bitstream, but nothing stops an adversarial one).
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
    /// the plane's own border on any out-of-range coordinate.
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
        row.get(usize::try_from(xx).unwrap_or(0))
            .copied()
            .unwrap_or(0)
    }
}
