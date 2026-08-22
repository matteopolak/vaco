//! Construction and plane access.
//!
//! Until this module existed no crate outside `vaco-pool` could build a
//! [`Frame`] at all, because [`Buffer`] had no public constructor. Everything
//! here is deliberately additive to the frozen struct definitions: the fields
//! stay public and a struct literal still works, but nobody should have to write
//! one to get a correctly strided, correctly aligned picture.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;
use vaco_pool::{ALIGN, Buffer};
use vaco_sampfmt::SampleFmt;

use crate::plane::{PlaneMut, PlaneRef};
use crate::{Frame, FrameData, FrameFlags, Plane};

/// Geometry of one video plane: `(stride, rows, row_bytes)`.
fn video_geometry(format: PixFmt, width: u32, height: u32, index: usize) -> (usize, usize, usize) {
    let i = u8::try_from(index).unwrap_or(u8::MAX);
    let rows = format.plane_height(height, i) as usize;
    let row_bytes = format.min_stride(width, i);
    (row_bytes.next_multiple_of(ALIGN), rows, row_bytes)
}

impl Frame {
    /// A frame carrying `data` and nothing else: no timestamps, no colour
    /// signalling, no flags.
    ///
    /// The building block the more specific constructors are written in terms
    /// of, and the right entry point for a decoder that fills the metadata in
    /// from its own bitstream.
    #[must_use]
    pub fn from_data(data: FrameData) -> Self {
        Self {
            data,
            pts: Timestamp::NONE,
            duration: Duration::ZERO,
            time_base: Rational::ONE,
            color: ColorInfo::default(),
            sample_aspect_ratio: Rational::ONE,
            flags: FrameFlags::empty(),
            side_data: SmallVec::new(),
        }
    }

    /// Allocate a video frame, one buffer per plane, each row stride rounded up
    /// to [`ALIGN`].
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the dimensions or the implied size exceed the
    /// budget's caps, and [`Error::Unsupported`] for a hardware pixel format,
    /// whose planes live on a device rather than in this address space.
    pub fn alloc_video(
        budget: &mut Budget,
        format: PixFmt,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        if format.is_hw() {
            return Err(Error::Unsupported(
                "cannot allocate a hardware pixel format",
            ));
        }
        // Reject absurd dimensions before a byte is touched. `bits_per_pixel`
        // covers all planes, so this is the whole picture, not one plane.
        let bpp = u32::from(format.bits_per_pixel()).div_ceil(8).max(1);
        budget.check_frame(width, height, bpp)?;

        let layout = format.plane_layout(width, height, ALIGN)?;
        let mut planes: SmallVec<[Plane; 4]> = SmallVec::new();
        for i in 0..layout.planes {
            let size = layout.sizes.get(i).copied().unwrap_or(0);
            let stride = layout.strides.get(i).copied().unwrap_or(0);
            planes.push(Plane {
                data: Buffer::alloc(budget, size)?,
                stride,
            });
        }
        Ok(Self::from_data(FrameData::Video {
            format,
            width,
            height,
            planes,
        }))
    }

    /// Wrap planes that were filled somewhere else — decoder output, or a
    /// hardware surface that has been mapped.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane count does not match the format, or a
    /// plane is too short for its declared stride and height.
    pub fn video_from_planes(
        format: PixFmt,
        width: u32,
        height: u32,
        planes: SmallVec<[Plane; 4]>,
    ) -> Result<Self> {
        if planes.len() != format.plane_count() {
            return Err(Error::InvalidData(
                "plane count does not match pixel format",
            ));
        }
        for (i, plane) in planes.iter().enumerate() {
            let (_, rows, row_bytes) = video_geometry(format, width, height, i);
            if plane.stride < row_bytes {
                return Err(Error::InvalidData("plane stride is narrower than a row"));
            }
            let need = plane.stride.saturating_mul(rows);
            if rows > 0 && plane.data.len() < need {
                return Err(Error::InvalidData("plane buffer is shorter than its rows"));
            }
        }
        Ok(Self::from_data(FrameData::Video {
            format,
            width,
            height,
            planes,
        }))
    }

    /// Allocate an audio frame: one buffer per channel for a planar format,
    /// exactly one for an interleaved one.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the channel count or the implied size exceed
    /// the budget's caps.
    pub fn alloc_audio(
        budget: &mut Budget,
        format: SampleFmt,
        layout: ChannelLayout,
        samples: u32,
        sample_rate: u32,
    ) -> Result<Self> {
        budget.check_channels(u64::from(layout.channels))?;
        let channels = layout.channels.max(1) as usize;
        let (count, per_plane_samples) = if format.is_planar() {
            (channels, 1usize)
        } else {
            (1usize, channels)
        };
        let bytes = (samples as usize)
            .checked_mul(format.bytes_per_sample())
            .and_then(|b| b.checked_mul(per_plane_samples))
            .ok_or(Error::LimitExceeded {
                limit: "audio_frame_bytes",
                requested: u64::MAX,
                cap: usize::MAX as u64,
            })?;

        let mut planes: SmallVec<[Plane; 8]> = SmallVec::new();
        for _ in 0..count {
            planes.push(Plane {
                data: Buffer::alloc(budget, bytes)?,
                stride: bytes,
            });
        }
        Ok(Self::from_data(FrameData::Audio {
            format,
            sample_rate,
            samples,
            layout,
            planes,
        }))
    }

    /// Whether this is a video frame.
    #[must_use]
    pub const fn is_video(&self) -> bool {
        matches!(self.data, FrameData::Video { .. })
    }

    /// Whether this is an audio frame.
    #[must_use]
    pub const fn is_audio(&self) -> bool {
        matches!(self.data, FrameData::Audio { .. })
    }

    /// The pixel format, for a video frame.
    #[must_use]
    pub const fn pixel_format(&self) -> Option<PixFmt> {
        match self.data {
            FrameData::Video { format, .. } => Some(format),
            FrameData::Audio { .. } => None,
        }
    }

    /// The coded dimensions, before any crop.
    #[must_use]
    pub const fn dimensions(&self) -> Option<(u32, u32)> {
        match self.data {
            FrameData::Video { width, height, .. } => Some((width, height)),
            FrameData::Audio { .. } => None,
        }
    }

    /// How many planes the frame holds.
    #[must_use]
    pub fn plane_count(&self) -> usize {
        self.planes_slice().len()
    }

    pub(crate) fn planes_slice(&self) -> &[Plane] {
        match &self.data {
            FrameData::Video { planes, .. } => planes,
            FrameData::Audio { planes, .. } => planes,
        }
    }

    pub(crate) fn planes_slice_mut(&mut self) -> &mut [Plane] {
        match &mut self.data {
            FrameData::Video { planes, .. } => planes,
            FrameData::Audio { planes, .. } => planes,
        }
    }

    /// Read-only view of plane `index`, with its geometry attached.
    #[must_use]
    pub fn plane(&self, index: usize) -> Option<PlaneRef<'_>> {
        match &self.data {
            FrameData::Video {
                format,
                width,
                height,
                planes,
            } => {
                let plane = planes.get(index)?;
                let (_, rows, row_bytes) = video_geometry(*format, *width, *height, index);
                Some(PlaneRef::new(
                    plane.data.as_slice(),
                    plane.stride,
                    rows,
                    row_bytes.min(plane.stride),
                ))
            }
            FrameData::Audio { planes, .. } => {
                let plane = planes.get(index)?;
                let len = plane.data.len();
                Some(PlaneRef::new(plane.data.as_slice(), len, 1, len))
            }
        }
    }

    /// Exclusive view of plane `index`, copying it first if it is shared.
    ///
    /// Only this plane is made writable; the others keep sharing whatever they
    /// were sharing, which is the whole point of one `Arc` per plane.
    pub fn plane_mut(&mut self, index: usize) -> Option<PlaneMut<'_>> {
        match &mut self.data {
            FrameData::Video {
                format,
                width,
                height,
                planes,
            } => {
                let (format, width, height) = (*format, *width, *height);
                let plane = planes.get_mut(index)?;
                let (_, rows, row_bytes) = video_geometry(format, width, height, index);
                let stride = plane.stride;
                Some(PlaneMut::new(
                    plane.data.make_mut(),
                    stride,
                    rows,
                    row_bytes.min(stride),
                ))
            }
            FrameData::Audio { planes, .. } => {
                let plane = planes.get_mut(index)?;
                let data = plane.data.make_mut();
                let len = data.len();
                Some(PlaneMut::new(data, len, 1, len))
            }
        }
    }

    /// Every plane at once, mutably.
    ///
    /// Disjointness is structural rather than checked: `iter_mut` yields one
    /// `&mut Plane` per element with distinct provenance, and each plane's
    /// [`Buffer::make_mut`] acts on a *different* `Arc`. Two threads can
    /// therefore take two of these and the compiler proves they cannot alias —
    /// there is no runtime mechanism involved at all.
    ///
    /// See the crate-level example for the thread-scoped form.
    pub fn planes_mut(&mut self) -> SmallVec<[PlaneMut<'_>; 4]> {
        match &mut self.data {
            FrameData::Video {
                format,
                width,
                height,
                planes,
            } => {
                let (format, width, height) = (*format, *width, *height);
                planes
                    .iter_mut()
                    .enumerate()
                    .map(|(i, plane)| {
                        let (_, rows, row_bytes) = video_geometry(format, width, height, i);
                        let stride = plane.stride;
                        PlaneMut::new(plane.data.make_mut(), stride, rows, row_bytes.min(stride))
                    })
                    .collect()
            }
            FrameData::Audio { planes, .. } => planes
                .iter_mut()
                .map(|plane| {
                    let data = plane.data.make_mut();
                    let len = data.len();
                    PlaneMut::new(data, len, 1, len)
                })
                .collect(),
        }
    }
}
