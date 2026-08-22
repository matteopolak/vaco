//! Decoded frames: video pictures and audio sample blocks.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{Duration, Rational, Timestamp};
use vaco_pixfmt::PixFmt;
use vaco_pool::Buffer;
use vaco_sampfmt::SampleFmt;

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
    ContentLightLevel { max_cll: u32, max_fall: u32 },
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
    #[must_use]
    pub fn cropped_dimensions(&self) -> Option<(u32, u32)> {
        todo!("P0-03 freeze")
    }

    /// Make every plane uniquely owned so it can be written.
    pub fn make_writable(&mut self) {
        todo!("P0-03 freeze: Buffer::make_mut on each plane")
    }
}
