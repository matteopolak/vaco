//! A private, plain reconstruction buffer.
//!
//! Same reasoning as `vaco-codec-vp8`/`vaco-codec-vp9`'s identically-named
//! types (`xtask/src/dup_check.rs`'s `DISTINCT` list covers `Plane`/`Picture`
//! generically for exactly this shape): intra prediction and reconstruction
//! need to read already-written pixels of the very buffer being written,
//! which does not fit `vaco_frame::Plane`'s borrow shape. Copied into a real
//! `vaco_frame::Frame` once, at emission. `u16`-backed since AV1 samples run
//! up to 12 bits.

use vaco_core::Result;
use vaco_limits::Budget;

/// One reconstruction plane.
#[derive(Debug)]
pub struct Plane {
    width: usize,
    height: usize,
    data: Vec<u16>,
}

impl Plane {
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub fn new(budget: &mut Budget, width: usize, height: usize) -> Result<Self> {
        let len = width.saturating_mul(height);
        let data = budget.alloc(len)?;
        Ok(Self {
            width,
            height,
            data,
        })
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Reads `(x, y)`, clamping both coordinates into range — the edge
    /// extension every out-of-bounds neighbour read in intra prediction
    /// (`AvailU`/`AvailL` already gate the *decision* to read; this handles
    /// reads at the frame's own physical edge) relies on.
    #[must_use]
    pub fn get_clamped(&self, x: i32, y: i32) -> u16 {
        let max_x = i32::try_from(self.width.saturating_sub(1)).unwrap_or(0);
        let max_y = i32::try_from(self.height.saturating_sub(1)).unwrap_or(0);
        let (cx, cy) = (x.clamp(0, max_x), y.clamp(0, max_y));
        let (ux, uy) = (
            usize::try_from(cx).unwrap_or(0),
            usize::try_from(cy).unwrap_or(0),
        );
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

/// One decoded frame's reconstruction planes: luma plus, unless the sequence
/// is monochrome, two chroma planes at the sequence's subsampling.
#[derive(Debug)]
pub struct Picture {
    pub y: Plane,
    pub u: Option<Plane>,
    pub v: Option<Plane>,
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
        monochrome: bool,
    ) -> Result<Self> {
        let y = Plane::new(budget, luma_width, luma_height)?;
        let (u, v) = if monochrome {
            (None, None)
        } else {
            (
                Some(Plane::new(budget, chroma_width, chroma_height)?),
                Some(Plane::new(budget, chroma_width, chroma_height)?),
            )
        };
        Ok(Self { y, u, v })
    }

    /// The plane for `plane_index` (0 = Y, 1 = U, 2 = V), or `None` for a
    /// chroma index on a monochrome picture.
    #[must_use]
    pub const fn plane(&self, plane_index: usize) -> Option<&Plane> {
        match plane_index {
            0 => Some(&self.y),
            1 => self.u.as_ref(),
            2 => self.v.as_ref(),
            _ => None,
        }
    }

    #[must_use]
    pub const fn plane_mut(&mut self, plane_index: usize) -> Option<&mut Plane> {
        match plane_index {
            0 => Some(&mut self.y),
            1 => self.u.as_mut(),
            2 => self.v.as_mut(),
            _ => None,
        }
    }

    /// A disjoint borrow of the luma plane (read-only) alongside one
    /// chroma plane (mutable) — exactly what CFL prediction needs (read
    /// already-reconstructed luma, write the chroma plane it predicts),
    /// without the two borrows aliasing since `y` and `u`/`v` are always
    /// different fields.
    #[must_use]
    pub const fn luma_and_chroma_mut(
        &mut self,
        plane_index: usize,
    ) -> (&Plane, Option<&mut Plane>) {
        let chroma = match plane_index {
            1 => self.u.as_mut(),
            2 => self.v.as_mut(),
            _ => None,
        };
        (&self.y, chroma)
    }
}
