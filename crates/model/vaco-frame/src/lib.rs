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
mod subtitle;

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
pub use subtitle::{SubtitleContent, SubtitleRect};

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
    /// A subtitle event: zero or more positioned regions. See
    /// [`subtitle`] for the shape and why
    /// the display-time window is not a field here (it is `Frame::pts`/
    /// `Frame::duration`, which every variant already carries) and why this
    /// enum stays closed rather than gaining `#[non_exhaustive]` alongside
    /// this variant.
    ///
    /// No decoder in this workspace constructs one yet — this is the shape
    /// the three in-flight T2-13 subtitle-codec crates
    /// (`vaco-codec-subtitle-bitmap`/`-cc`/`-teletext`) are meant to be
    /// wired to, not a claim that any of them are wired today.
    Subtitle { rects: SmallVec<[SubtitleRect; 2]> },
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
    /// Human-readable lines a filter wants printed at info log level, one
    /// entry per line.
    ///
    /// `showinfo`'s whole output is a console log line, not a metadata
    /// write — measured (`ffprobe -show_frames` through it, `ffmpeg 8.1`) to
    /// touch `AVFrame::metadata` **not at all**. [`FrameSideData::Metadata`]
    /// is the `lavfi.<filter>.<key>` tag convention specifically; stuffing
    /// an unstructured line under a made-up key there would not reproduce
    /// anything the reference actually exports, since nothing does. This is
    /// a separate, narrower channel: no keys, no structure, just the lines
    /// a filter would otherwise have written straight to the log.
    Log(Vec<String>),
    /// Per-block motion vectors a decoder attached to this frame, for
    /// `codecview`'s `mv`/`mv_type` visualisation.
    ///
    /// No decoder in this workspace populates this yet (D5: motion vectors
    /// are decoder-internal state no codec crate currently surfaces), so
    /// this variant exists without a producer — recorded as such rather
    /// than silently reattempting `codecview` before the decoder-side half
    /// exists. See [`MotionVector`]'s own doc for the field shape and what
    /// it is (and is not) measured against.
    MotionVectors(Vec<MotionVector>),
    /// How many *extra* field periods this frame's presentation should be
    /// held for, beyond the one it normally gets — MPEG-2's
    /// `repeat_first_field`/`top_field_first` combination (H.262 §6.3.10),
    /// the `AVFrame::repeat_pict` concept `ffmpeg`'s own `repeatfields`
    /// filter reads (`vaco-filter-deinterlace`'s own `repeatfields.rs`
    /// documents needing exactly this, independently of any decoder).
    ///
    /// Always in units of one field period, always one of `0` (no repeat,
    /// the overwhelmingly common case — this variant is normally absent
    /// rather than present with `0`, see [`Frame::repeat_pict`]), `1`
    /// (one repeated field), `2` (one repeated frame) or `4` (two repeated
    /// frames) under a conforming bitstream: the codec that computes this
    /// must combine sequence-level and picture-level flags itself (H.262's
    /// combination rule depends on `progressive_sequence`, a
    /// sequence-level flag, as well as two picture-level ones), so a
    /// consumer reads one already-resolved number rather than re-deriving
    /// the combination.
    Pulldown(u8),
    // ... generated from the side-data table
}

/// One motion vector, as a decoder would attach it to the frame it predicts.
///
/// Fields mirror the concepts `ffmpeg -h filter=codecview`'s own option
/// descriptions name for `mv`/`mv_type`/`block`: which prediction direction
/// produced it, the block it covers, and where it points from and to. This
/// is a representative shape for a currently-unproduced side data variant
/// (see [`FrameSideData::MotionVectors`]), not a byte-for-byte transcription
/// of the reference's internal struct layout — D7 forbids consulting that,
/// and black-box probing (`ffprobe -export_side_data +mvs`) surfaces only a
/// side-data-type label, not a field breakdown, for want of a decoder in
/// this workspace to attach one in the first place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionVector {
    /// Which reference this vector predicts from: negative counts backward
    /// (B-frame back-reference), positive forward — the same sign
    /// convention `codecview`'s `mv=pf+bf+bb` options name (past/backward
    /// forward, backward-frame forward, backward-frame backward).
    pub source: i32,
    /// Width and height of the block this vector covers, in luma pixels.
    pub w: u16,
    pub h: u16,
    /// The block's top-left position in the *destination* (this) frame.
    pub dst_x: i32,
    pub dst_y: i32,
    /// Where the block's content came from in the source picture —
    /// `dst_x + motion_x/scale`, precomputed rather than left for a caller
    /// to rescale, since drawing an arrow is the only consumer.
    pub src_x: i32,
    pub src_y: i32,
}

/// SMPTE ST 2086 mastering display colour volume — the values H.264/HEVC's
/// `mastering_display_colour_volume()` SEI, AV1's `metadata_hdr_mdcv()` OBU
/// and MP4's `mdcv` box all carry, in three different fixed-point encodings
/// of the identical spec (D7 forbids reading which of the three came first;
/// each producer converts its own bitstream's raw units to this shared
/// shape).
///
/// `primaries[0]`/`[1]`/`[2]` are **red, green, blue**, matching the
/// reference's own `AVMasteringDisplayMetadata.display_primaries` layout —
/// measured with real `ffprobe -show_frames` on an HDR10 fixture
/// (`master-display=G(13250,34500)B(7500,3000)R(34000,16000)`, i.e.
/// green/blue/red in that order on the command line and in the bitstream
/// itself, per H.264/HEVC/AV1 Annex D's own `c == 0, 1, 2` semantics text)
/// printing `red_x=34000/50000 ... green_x=13250/50000 ... blue_x=7500/50000`
/// — red first. A producer reading a green/blue/red-ordered bitstream must
/// permute into this red/green/blue order, not copy positionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasteringDisplay {
    pub primaries: [[Rational; 2]; 3],
    pub white_point: [Rational; 2],
    pub max_luminance: Rational,
    pub min_luminance: Rational,
}

impl Frame {
    /// Read the native tick count used by frame producers and filters.
    ///
    /// Keep the frame's time base alongside this value; packet durations use
    /// a seconds-based representation and must not be reinterpreted here.
    #[must_use]
    pub const fn duration_ticks(&self) -> i64 {
        self.duration.0
    }

    /// Set a duration in the frame's native time base.
    pub const fn set_duration_ticks(&mut self, ticks: i64) {
        self.duration = Duration(ticks);
    }

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
