//! Side data: typed variants, not opaque blobs.
//!
//! The D1 dividend — a consumer matches on a variant instead of casting a byte
//! pointer. The inventory's frame side-data types land here incrementally, as
//! the codecs that produce them arrive; the four in the freeze plus [`Crop`] are
//! what v0.1 needs.

use vaco_core::{Error, Result};
use vaco_pixfmt::PixFmt;

use crate::{Frame, FrameData, FrameSideData};

/// A crop rectangle, in pixels off each edge of the coded picture.
///
/// Cropping is offset metadata, never a copy: a coded 1920x1088 HEVC picture
/// presents as 1920x1080 by carrying `bottom: 8`, and no plane is touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Crop {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl Crop {
    /// The all-zero rectangle.
    pub const NONE: Self = Self {
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
    };

    /// Whether this rectangle crops anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.top == 0 && self.bottom == 0 && self.left == 0 && self.right == 0
    }

    /// The visible size of a `width` x `height` picture under this crop.
    ///
    /// Saturating: an over-large crop yields a zero dimension rather than
    /// wrapping into a huge one, which matters because these numbers come from
    /// a bitstream.
    #[must_use]
    pub const fn apply(&self, width: u32, height: u32) -> (u32, u32) {
        let w = width.saturating_sub(self.left).saturating_sub(self.right);
        let h = height.saturating_sub(self.top).saturating_sub(self.bottom);
        (w, h)
    }

    /// Whether the offsets land on a chroma sample boundary for `format`.
    ///
    /// A 4:2:0 picture cannot be cropped by an odd number of pixels: there is no
    /// half chroma sample to start at. Rejecting it here is what stops a
    /// bitstream-supplied crop from producing a misaligned chroma plane.
    #[must_use]
    pub fn is_valid_for(&self, format: PixFmt, width: u32, height: u32) -> bool {
        let (log2_w, log2_h) = format.log2_chroma();
        let mask_w = (1u32 << log2_w) - 1;
        let mask_h = (1u32 << log2_h) - 1;
        if self.left & mask_w != 0 || self.right & mask_w != 0 {
            return false;
        }
        if self.top & mask_h != 0 || self.bottom & mask_h != 0 {
            return false;
        }
        self.left.saturating_add(self.right) < width
            && self.top.saturating_add(self.bottom) < height
    }
}

/// Discriminant of [`FrameSideData`], for lookup and removal.
///
/// Separate from the enum itself so a caller can ask "is there a crop?" without
/// constructing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FrameSideDataKind {
    DisplayMatrix,
    ClosedCaptions,
    MasteringDisplay,
    ContentLightLevel,
    Cropping,
}

impl FrameSideData {
    /// Which kind of side data this is.
    #[must_use]
    pub const fn kind(&self) -> FrameSideDataKind {
        match self {
            Self::DisplayMatrix(_) => FrameSideDataKind::DisplayMatrix,
            Self::ClosedCaptions(_) => FrameSideDataKind::ClosedCaptions,
            Self::MasteringDisplay(_) => FrameSideDataKind::MasteringDisplay,
            Self::ContentLightLevel { .. } => FrameSideDataKind::ContentLightLevel,
            Self::Cropping(_) => FrameSideDataKind::Cropping,
        }
    }
}

impl Frame {
    /// The entry of `kind`, if the frame carries one.
    ///
    /// Linear scan: frames carry 0-3 entries, so a map would cost more than it
    /// saves.
    #[must_use]
    pub fn side_data(&self, kind: FrameSideDataKind) -> Option<&FrameSideData> {
        self.side_data.iter().find(|d| d.kind() == kind)
    }

    /// Attach `data`, replacing any existing entry of the same kind.
    pub fn set_side_data(&mut self, data: FrameSideData) {
        let kind = data.kind();
        if let Some(slot) = self.side_data.iter_mut().find(|d| d.kind() == kind) {
            *slot = data;
        } else {
            self.side_data.push(data);
        }
    }

    /// Detach and return the entry of `kind`.
    pub fn remove_side_data(&mut self, kind: FrameSideDataKind) -> Option<FrameSideData> {
        let at = self.side_data.iter().position(|d| d.kind() == kind)?;
        Some(self.side_data.remove(at))
    }

    /// The crop rectangle, if one is attached and it crops something.
    #[must_use]
    pub fn crop(&self) -> Option<Crop> {
        match self.side_data(FrameSideDataKind::Cropping)? {
            FrameSideData::Cropping(c) if !c.is_empty() => Some(*c),
            _ => None,
        }
    }

    /// Attach a crop rectangle, after checking it against the frame's chroma
    /// subsampling and dimensions.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the frame is audio, or if the rectangle is not
    /// on a chroma sample boundary or leaves nothing visible.
    pub fn set_crop(&mut self, crop: Crop) -> Result<()> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = &self.data
        else {
            return Err(Error::InvalidData("crop on an audio frame"));
        };
        if !crop.is_valid_for(*format, *width, *height) {
            return Err(Error::InvalidData("crop rectangle invalid for this format"));
        }
        self.set_side_data(FrameSideData::Cropping(crop));
        Ok(())
    }
}
