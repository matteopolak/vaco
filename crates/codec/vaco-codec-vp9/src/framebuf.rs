//! A private, plain reconstruction buffer — same reasoning as
//! `vaco-codec-vp8::framebuf`'s identical `Plane`: intra prediction and
//! reconstruction need to read already-written pixels of the very buffer
//! being written, which does not fit `vaco_frame::Plane`'s borrow shape.
//! Copied into a real `vaco_frame::Frame` once, at emission. See
//! `xtask/src/dup_check.rs`'s `DISTINCT` entry for `vaco-codec-vp8`'s
//! `Plane`, which this one duplicates the reasoning of rather than the code
//! (VP9 samples are up to 12-bit, so this one is `u16`-backed, not `u8`).

use vaco_limits::Budget;

/// A single reconstruction plane, `u16` per sample (holds 8/10/12-bit
/// values equally).
#[derive(Debug)]
pub struct Plane {
    width: usize,
    height: usize,
    data: Vec<u16>,
}

impl Plane {
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub fn new(budget: &mut Budget, width: usize, height: usize) -> vaco_core::Result<Self> {
        let len = width.saturating_mul(height);
        let data = budget.alloc(len)?;
        Ok(Self { width, height, data })
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Reads `(x, y)`, clamping both coordinates into range — the "edge
    /// extension" every VP9 process that reads `CurrFrame` past its own
    /// bounds implicitly relies on (`Min(maxX, ...)`/`Min(maxY, ...)` in
    /// §8.5.1, for instance).
    #[must_use]
    pub fn get_clamped(&self, x: i32, y: i32) -> u16 {
        let cx = x.clamp(0, i32::try_from(self.width.saturating_sub(1)).unwrap_or(0));
        let cy = y.clamp(0, i32::try_from(self.height.saturating_sub(1)).unwrap_or(0));
        let (ux, uy) = (usize::try_from(cx).unwrap_or(0), usize::try_from(cy).unwrap_or(0));
        self.data.get(uy * self.width + ux).copied().unwrap_or(0)
    }

    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        if x < self.width
            && y < self.height
            && let Some(slot) = self.data.get_mut(y * self.width + x)
        {
            *slot = v;
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u16] {
        &self.data
    }
}

/// One decoded frame's three reconstruction planes.
#[derive(Debug)]
pub struct Picture {
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
}

impl Picture {
    /// # Errors
    /// [`vaco_core::Error`] if any plane's allocation exceeds `budget`.
    pub fn new(
        budget: &mut Budget,
        luma_width: usize,
        luma_height: usize,
        chroma_width: usize,
        chroma_height: usize,
    ) -> vaco_core::Result<Self> {
        Ok(Self {
            y: Plane::new(budget, luma_width, luma_height)?,
            u: Plane::new(budget, chroma_width, chroma_height)?,
            v: Plane::new(budget, chroma_width, chroma_height)?,
        })
    }
}
