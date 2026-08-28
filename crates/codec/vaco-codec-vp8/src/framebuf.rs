//! An internal reconstruction buffer per plane, and the three reference
//! frame slots (last/golden/altref) RFC 6386 §9.7/§9.8 describes.
//!
//! Kept as a private, plain `Vec<u8>` rather than a [`vaco_frame::Plane`]
//! because intra prediction and the loop filter both need to read
//! already-written pixels of the *same* plane while a macroblock further
//! along is still being written — sequential `&mut self` calls on an
//! owned buffer make that trivial, where the pool's `PlaneRef`/`PlaneMut`
//! split would need one exclusive borrow per plane per macroblock. The
//! final picture is copied into a real [`vaco_frame::Frame`] once, in
//! [`crate::decode`], which is where the pool/budget-backed allocation this
//! project requires actually happens for the emitted frame.

use vaco_core::Result;
use vaco_limits::Budget;

/// One reconstructed plane, padded to a whole number of macroblocks (16px
/// luma / 8px chroma) so every predictor and filter can address a full
/// macroblock grid without special-casing the frame's true (unpadded) edge.
#[derive(Debug, Clone)]
pub struct Plane {
    data: Vec<u8>,
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl Plane {
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] if `width * height` exceeds the
    /// budget's caps.
    pub fn new(budget: &mut Budget, width: usize, height: usize) -> Result<Self> {
        let stride = width;
        let len = stride.saturating_mul(height);
        Ok(Self {
            data: budget.alloc(len)?,
            stride,
            width,
            height,
        })
    }

    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 {
            return 0;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data.get(y * self.stride + x).copied().unwrap_or(0)
    }

    /// Same as [`Self::get`] but clamps out-of-range coordinates to the
    /// plane's edge instead of returning 0 — the edge-extension a
    /// reference frame needs under motion compensation (RFC 6386 §18.1).
    #[must_use]
    pub fn get_clamped(&self, x: i32, y: i32) -> u8 {
        let max_x = i32::try_from(self.width.saturating_sub(1)).unwrap_or(i32::MAX);
        let max_y = i32::try_from(self.height.saturating_sub(1)).unwrap_or(i32::MAX);
        let cx = usize::try_from(x.clamp(0, max_x)).unwrap_or(0);
        let cy = usize::try_from(y.clamp(0, max_y)).unwrap_or(0);
        self.data.get(cy * self.stride + cx).copied().unwrap_or(0)
    }

    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        if let Some(slot) = self.data.get_mut(y * self.stride + x) {
            *slot = v;
        }
    }

    #[must_use]
    pub fn row(&self, y: usize) -> &[u8] {
        let start = y.saturating_mul(self.stride);
        self.data.get(start..start + self.width).unwrap_or(&[])
    }

    /// The whole backing buffer, for building a borrowed
    /// [`vaco_codec_dsp_mecmp::Plane`] over it — `crate::encode`'s motion
    /// search and distortion measurement both need one, and duplicating
    /// this plane type rather than adapting to the shared one is exactly
    /// what D19 exists to prevent.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

/// One decoded picture's three planes, held by a reference slot.
#[derive(Debug, Clone)]
pub struct Picture {
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
}

impl Picture {
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] if the implied plane sizes exceed
    /// the budget's caps.
    pub fn new(budget: &mut Budget, mb_cols: usize, mb_rows: usize) -> Result<Self> {
        Ok(Self {
            y: Plane::new(budget, mb_cols * 16, mb_rows * 16)?,
            u: Plane::new(budget, mb_cols * 8, mb_rows * 8)?,
            v: Plane::new(budget, mb_cols * 8, mb_rows * 8)?,
        })
    }
}

/// The three reference slots RFC 6386 keeps: last, golden, altref.
#[derive(Debug, Clone, Default)]
pub struct RefFrames {
    pub last: Option<Picture>,
    pub golden: Option<Picture>,
    pub altref: Option<Picture>,
}

impl RefFrames {
    #[must_use]
    pub fn get(&self, which: u8) -> Option<&Picture> {
        match which {
            1 => self.last.as_ref(),
            2 => self.golden.as_ref(),
            3 => self.altref.as_ref(),
            _ => None,
        }
    }

    /// Apply RFC 6386 §9.7/§9.8's refresh/copy rules after a frame decodes.
    /// `copy_to_golden`/`copy_to_altref` are the 2-bit codes: 0 = no copy,
    /// 1 = copy from LAST, 2 = copy from ALTREF (for golden) / GOLDEN (for
    /// altref).
    pub fn update(
        &mut self,
        current: Picture,
        refresh_last: bool,
        refresh_golden: bool,
        refresh_altref: bool,
        copy_to_golden: u32,
        copy_to_altref: u32,
    ) {
        if !refresh_golden {
            match copy_to_golden {
                1 => self.golden = self.last.clone(),
                2 => self.golden = self.altref.clone(),
                _ => {}
            }
        }
        if !refresh_altref {
            match copy_to_altref {
                1 => self.altref = self.last.clone(),
                2 => self.altref = self.golden.clone(),
                _ => {}
            }
        }
        if refresh_golden {
            self.golden = Some(current.clone());
        }
        if refresh_altref {
            self.altref = Some(current.clone());
        }
        if refresh_last {
            self.last = Some(current);
        }
    }
}
