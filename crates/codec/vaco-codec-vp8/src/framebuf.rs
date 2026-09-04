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
//!
//! # Reference slots hold handles, not bytes
//!
//! [`RefFrames`] used to own a `Picture` outright per slot, so
//! `RefFrames::update`'s "copy to golden/altref" rule (RFC 6386 §9.7/§9.8)
//! was a real byte-for-byte clone of a whole reconstructed picture. Frame
//! threading needs the opposite: the *decision* of what last/golden/altref
//! point to next has to be made the instant a frame's header is parsed —
//! before that frame's own pixels exist, on a worker thread, possibly
//! seconds later. [`vaco_codec_core::picture::PictureRef`] is exactly a
//! handle to a picture that may still be in production, and cloning one is a
//! refcount bump, so "copy to golden" is now free instead of a full-frame
//! `memcpy`, and it no longer needs the pixels to exist at all.
//!
//! [`materialize`] is the one place those handles turn back into bytes: a
//! [`Plane`]'s `get_clamped` is what every inter-prediction sample read in
//! [`crate::decode::mc_block`] already calls, so a task waits for the whole
//! referenced picture once (RFC 6386 has no partial-picture reference —
//! motion compensation can land anywhere in a reference frame) and copies it
//! into the same owned buffer shape reconstruction has always used. This
//! costs one reference-sized copy per distinct reference a frame actually
//! uses, at every thread count including one — see the module doc of
//! [`crate::frame_task`] for why that price was paid deliberately rather
//! than threaded around.

use vaco_codec_core::picture::PictureRef;
use vaco_core::{Error, Result};
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

/// The encoder's own last/golden/altref bookkeeping: plain owned pictures.
///
/// The encoder builds every reference synchronously on one thread (there is
/// no worker to hand a still-producing picture to), so it has no use for
/// [`RefFrames`]'s handle-based design below. Owned pictures are the simpler
/// fit for this synchronous caller.
#[derive(Debug, Clone, Default)]
pub struct EncRefFrames {
    pub last: Option<Picture>,
    pub golden: Option<Picture>,
    pub altref: Option<Picture>,
}

impl EncRefFrames {
    #[must_use]
    pub fn get(&self, which: u8) -> Option<&Picture> {
        match which {
            1 => self.last.as_ref(),
            2 => self.golden.as_ref(),
            3 => self.altref.as_ref(),
            _ => None,
        }
    }

    /// Apply RFC 6386 §9.7/§9.8's refresh/copy rules after a frame encodes.
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

/// The decoder's three reference slots RFC 6386 keeps: last, golden, altref.
///
/// Each slot is a handle, not a picture — see this module's doc. `Clone` is
/// three `Option<Arc<_>>` clones. See [`EncRefFrames`] for the encoder's own,
/// simpler equivalent.
#[derive(Debug, Clone, Default)]
pub struct RefFrames {
    pub last: Option<PictureRef>,
    pub golden: Option<PictureRef>,
    pub altref: Option<PictureRef>,
}

impl RefFrames {
    #[must_use]
    pub fn get(&self, which: u8) -> Option<&PictureRef> {
        match which {
            1 => self.last.as_ref(),
            2 => self.golden.as_ref(),
            3 => self.altref.as_ref(),
            _ => None,
        }
    }

    /// Apply RFC 6386 §9.7/§9.8's refresh/copy rules the instant a frame's
    /// header is known — `current` is a handle to a picture whose pixels may
    /// not exist yet, which is what lets this run in the serial split stage
    /// rather than waiting for reconstruction. `copy_to_golden`/
    /// `copy_to_altref` are the 2-bit codes: 0 = no copy, 1 = copy from LAST,
    /// 2 = copy from ALTREF (for golden) / GOLDEN (for altref).
    pub fn update(
        &mut self,
        current: &PictureRef,
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
            self.last = Some(current.clone());
        }
    }
}

/// Wait for `reference` to finish and copy its three planes into an owned
/// [`Picture`] — the bridge from a handle that may still be producing to the
/// plain `get_clamped`-addressable buffer [`crate::decode::mc_block`] reads.
///
/// Every plane is a single band (`PictureSpec::single_band`, see
/// [`crate::frame_task`]), so `contiguous_all` always succeeds once the wait
/// itself succeeds — RFC 6386 has no notion of a partially available
/// reference frame, so there is no coarser or finer grain to wait for.
///
/// # Errors
///
/// Whatever [`vaco_codec_core::picture::PictureRef::wait_rows_for`] reports:
/// the producing task failed, or (checked in debug builds) this call would
/// wait on a picture that is not earlier in decode order.
pub fn materialize(
    reference: &PictureRef,
    waiter_decode_index: u64,
    mb_cols: usize,
    mb_rows: usize,
    budget: &mut Budget,
) -> Result<Picture> {
    let mut out = Picture::new(budget, mb_cols, mb_rows)?;
    for (plane_idx, dst) in [&mut out.y, &mut out.u, &mut out.v].into_iter().enumerate() {
        let height = u32::try_from(dst.height).unwrap_or(u32::MAX);
        let view =
            reference.wait_rows_for(waiter_decode_index, plane_idx, height.saturating_sub(1))?;
        let src = view.contiguous_all().ok_or(Error::InvalidData(
            "vp8: a single-band reference plane was not one contiguous borrow",
        ))?;
        for y in 0..dst.height {
            let row_start = y.saturating_mul(src.stride);
            let row = src
                .data
                .get(row_start..row_start.saturating_add(dst.width))
                .unwrap_or(&[]);
            for (x, &v) in row.iter().enumerate() {
                dst.set(x, y, v);
            }
        }
    }
    Ok(out)
}
