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

/// Every sample of `plane`, packed little-endian, 2 bytes each — the shape
/// [`vaco_codec_core::picture::PictureWriter`]'s byte-oriented bands need
/// (issue #328). `u16::to_le_bytes` throughout; [`plane_from_bytes`] is the
/// exact inverse, so the round trip is internal to this crate and the
/// choice of endianness is not otherwise observable.
///
/// # Errors
/// [`vaco_core::Error`] if the allocation exceeds `budget`.
pub fn plane_to_bytes(plane: &Plane, budget: &mut Budget) -> vaco_core::Result<Vec<u8>> {
    let mut out: Vec<u8> = budget.alloc(plane.data.len() * 2)?;
    for (chunk, &v) in out.chunks_exact_mut(2).zip(&plane.data) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
    Ok(out)
}

/// The inverse of [`plane_to_bytes`]: reinterpret `width * height * 2`
/// little-endian bytes back into a [`Plane`]'s samples.
fn plane_from_bytes(bytes: &[u8], width: usize, height: usize, budget: &mut Budget) -> vaco_core::Result<Plane> {
    let mut plane = Plane::new(budget, width, height)?;
    for (i, slot) in plane.data.iter_mut().enumerate() {
        let off = i * 2;
        *slot = bytes.get(off..off + 2).and_then(|b| b.try_into().ok()).map_or(0, u16::from_le_bytes);
    }
    Ok(plane)
}

/// Wait for `reference` to finish and copy its three planes into an owned
/// [`Picture`] — the bridge from a handle that may still be producing to
/// the plain, directly-addressable buffer every reconstruction call site
/// reads (see `crate::refframe`'s module doc for why the handle exists at
/// all). Every plane is a single band (`PictureSpec::single_band`, see
/// `crate::frame_task`), so `contiguous_all` always succeeds once the wait
/// itself does — VP9 has no notion of a partially available reference
/// frame either: inter prediction can land anywhere in it.
///
/// # Errors
///
/// Whatever [`vaco_codec_core::picture::PictureRef::wait_rows_for`] reports.
pub fn materialize(
    reference: &vaco_codec_core::picture::PictureRef,
    waiter_decode_index: u64,
    luma_w: usize,
    luma_h: usize,
    chroma_w: usize,
    chroma_h: usize,
    budget: &mut Budget,
) -> vaco_core::Result<Picture> {
    let dims = [(0usize, luma_w, luma_h), (1, chroma_w, chroma_h), (2, chroma_w, chroma_h)];
    let mut planes = Vec::new();
    for (plane_idx, w, h) in dims {
        let height = u32::try_from(h).unwrap_or(u32::MAX);
        let view = reference.wait_rows_for(waiter_decode_index, plane_idx, height.saturating_sub(1))?;
        let src = view.contiguous_all().ok_or(vaco_core::Error::InvalidData(
            "vp9: a single-band reference plane was not one contiguous borrow",
        ))?;
        // `src.stride` is bytes per row as published; `w * 2` is bytes per
        // row this plane actually needs, which may be less when the
        // publisher rounded a band's width up -- take exactly the bytes
        // that matter, row by row, rather than assume the strides match.
        let mut bytes: Vec<u8> = budget.alloc(w * h * 2)?;
        bytes.clear();
        for y in 0..h {
            let row_start = y * src.stride;
            let row = src.data.get(row_start..row_start + (w * 2)).unwrap_or(&[]);
            bytes.extend_from_slice(row);
            if row.len() < w * 2 {
                bytes.resize(bytes.len() + (w * 2 - row.len()), 0);
            }
        }
        planes.push(plane_from_bytes(&bytes, w, h, budget)?);
    }
    let mut it = planes.into_iter();
    let (Some(y), Some(u), Some(v)) = (it.next(), it.next(), it.next()) else {
        return Err(vaco_core::Error::InvalidData("vp9: materialize produced fewer than three planes"));
    };
    Ok(Picture { y, u, v })
}
