//! Decoded frames: video pictures and audio sample blocks.
//!
//! # The ownership model, in one paragraph
//!
//! A [`Frame`] is a bag of metadata plus a list of [`Plane`]s, and **each plane
//! owns its own [`Buffer`]** — one `Arc` per plane, not one per frame (plan 11
//! F11). That single decision buys three things: two threads can hold `&mut` to
//! different planes with the borrow checker proving disjointness, a filter that
//! rewrites chroma and passes luma through copies nothing, and copy-on-write
//! granularity is the plane rather than the whole picture. Cloning a `Frame` is
//! a handful of refcount bumps and never touches a pixel.
//!
//! # Reading and writing planes
//!
//! Go through [`PlaneRef`] and [`PlaneMut`], not through `plane.data` directly.
//! They carry the geometry (`stride`, `rows`, `row_bytes`) that a bare `&[u8]`
//! loses, and they are the forward-compatibility seam for banded frame
//! threading (plan 11 §13.6) — a kernel written against `PlaneRef` keeps
//! compiling if planes ever become segmented.
//!
//! ```
//! use vaco_frame::Frame;
//! use vaco_limits::{Budget, Limits};
//! use vaco_pixfmt::PixFmt;
//!
//! let mut budget = Budget::new(Limits::strict());
//! let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 64, 64)?;
//!
//! // Four independent `&mut`, disjoint by construction: no runtime mechanism.
//! let mut planes = frame.planes_mut();
//! let (luma, chroma) = planes.split_at_mut(1);
//! std::thread::scope(|s| {
//!     s.spawn(|| luma[0].fill(16));            // exclusive access to plane 0
//!     s.spawn(|| { let _ = chroma[0].row(0); }); // and to plane 1
//! });
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod alloc;
mod plane;
mod pool;
mod sidedata;

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{Duration, Rational, Timestamp};
use vaco_pixfmt::PixFmt;
use vaco_pool::Buffer;
use vaco_sampfmt::SampleFmt;

pub use plane::{PlaneMut, PlaneRef};
pub use pool::FramePool;
pub use sidedata::{Crop, FrameMetadata, FrameSideDataKind};

/// One plane of a video frame, or one channel of planar audio.
///
/// Each plane owns its own [`Buffer`], rather than the frame owning one buffer
/// carrying all planes. That is deliberate: it lets two threads hold `&mut` to
/// different planes of the same frame with the borrow checker proving they are
/// disjoint — which is how chroma and luma get processed in parallel without
/// `unsafe` (plan 11 F11).
#[derive(Debug, Clone)]
pub struct Plane {
    pub data: Buffer,
    /// Bytes between the start of consecutive rows. May exceed the row's
    /// meaningful width, for alignment.
    pub stride: usize,
}

#[derive(Debug, Clone)]
pub enum FrameData {
    Video {
        format: PixFmt,
        width: u32,
        height: u32,
        planes: SmallVec<[Plane; 4]>,
    },
    Audio {
        format: SampleFmt,
        sample_rate: u32,
        samples: u32,
        layout: ChannelLayout,
        /// One entry for planar formats, exactly one for interleaved.
        planes: SmallVec<[Plane; 8]>,
    },
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub data: FrameData,
    pub pts: Timestamp,
    pub duration: Duration,
    pub time_base: Rational,
    pub color: ColorInfo,
    pub sample_aspect_ratio: Rational,
    pub flags: FrameFlags,
    pub side_data: SmallVec<[FrameSideData; 2]>,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct FrameFlags: u8 {
        const KEY         = 1 << 0;
        const CORRUPT     = 1 << 1;
        const INTERLACED  = 1 << 2;
        const TOP_FIELD_FIRST = 1 << 3;
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum FrameSideData {
    DisplayMatrix([i32; 9]),
    ClosedCaptions(Buffer),
    MasteringDisplay(Box<MasteringDisplay>),
    ContentLightLevel {
        max_cll: u32,
        max_fall: u32,
    },
    /// Crop rectangle to apply on presentation, as signalled by the codec.
    ///
    /// Not in the original freeze: [`Frame::cropped_dimensions`] was, and it has
    /// to read the rectangle from somewhere. Cropping is metadata rather than a
    /// plane rewrite, which is what makes it free.
    Cropping(Crop),
    /// The frame's string-keyed metadata dictionary — `AVFrame::metadata`'s
    /// counterpart, and the `lavfi.<filter>.<key>` export channel a whole
    /// family of measurement filters (`signalstats`, `freezedetect`, the rest
    /// of interface gap 11) has no other way to publish through.
    ///
    /// Not in the original freeze either, for the same reason `Cropping`
    /// wasn't: nothing needed it until a filter that only measures, and
    /// writes nothing else, showed up. Reach it through [`Frame::metadata`],
    /// [`Frame::set_metadata`] and [`Frame::metadata_get`] rather than
    /// matching this variant directly — they create the entry on first write
    /// and return `&[]`/`None` rather than requiring a caller to check for
    /// the variant's absence first.
    Metadata(FrameMetadata),
    // ... generated from the side-data table
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasteringDisplay {
    pub primaries: [[Rational; 2]; 3],
    pub white_point: [Rational; 2],
    pub max_luminance: Rational,
    pub min_luminance: Rational,
}

impl Frame {
    /// Crop rectangle applied on presentation, if the codec signalled one.
    ///
    /// `None` when there is no crop, when the crop is empty, or for audio.
    /// Cropping is presentation metadata: no plane is touched and no byte moves,
    /// which is why the visible size and the allocated size can differ.
    #[must_use]
    pub fn cropped_dimensions(&self) -> Option<(u32, u32)> {
        let FrameData::Video { width, height, .. } = self.data else {
            return None;
        };
        let crop = self.crop()?;
        Some(crop.apply(width, height))
    }

    /// Make every plane uniquely owned so it can be written.
    ///
    /// The `av_frame_make_writable` equivalent: pay the copy-on-write cost up
    /// front, at a point of the caller's choosing, rather than inside a loop.
    pub fn make_writable(&mut self) {
        for plane in self.planes_slice_mut() {
            plane.data.make_writable();
        }
    }

    /// Whether every plane is uniquely owned, so writing copies nothing.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.planes_slice().iter().all(|p| p.data.is_unique())
    }
}
