//! A worked two-input filter, and the helpers the tests build graphs from.
//!
//! `Stamp` is deliberately the smallest filter that makes the *choice* the
//! synchroniser made observable: it copies the secondary frame's first byte
//! into the main frame. That is exactly the experiment the reference was probed
//! with — overlay a solid colour whose value identifies the frame, then read
//! the output back — so the same vectors can be asserted against both.

use vaco_core::{MediaType, Rational, Result, TimeBase, Timestamp};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;

use crate::adapt::{FrameSyncFilter, Synced};
use crate::opts::{FrameSyncOpts, FsInput};
use crate::sync::FrameSyncEvent;

/// Two video input pads.
pub const DUAL_VIDEO_PADS: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Video,
    },
    Pad {
        name: "second",
        media_type: MediaType::Video,
    },
];

/// One video output pad.
pub const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// A `gray8` link of the given geometry and time base.
#[must_use]
pub fn gray_link(width: u32, height: u32, time_base: TimeBase) -> LinkFormat {
    let mut format = LinkFormat::unconfigured(MediaType::Video);
    if let LinkFormat::Video {
        format: f,
        width: w,
        height: h,
        time_base: tb,
        frame_rate,
        sample_aspect_ratio,
        ..
    } = &mut format
    {
        *f = PixFmt::Gray8;
        *w = width;
        *h = height;
        *tb = time_base;
        *frame_rate = time_base.inverse();
        *sample_aspect_ratio = Rational::ONE;
    }
    format
}

/// A 1x1 `gray8` frame carrying `value`, timestamped `pts` in `time_base`.
#[must_use]
pub fn gray_frame(pool: &FramePool, pts: i64, time_base: TimeBase, value: u8) -> Option<Frame> {
    let mut frame = pool.acquire_video(PixFmt::Gray8, 1, 1).ok()?;
    if let Some(mut plane) = frame.plane_mut(0) {
        for row in plane.rows_mut() {
            for byte in row.iter_mut() {
                *byte = value;
            }
        }
    }
    frame.pts = Timestamp::new(pts);
    frame.time_base = time_base;
    Some(frame)
}

/// The first byte of a frame's first plane.
#[must_use]
pub fn first_byte(frame: &Frame) -> Option<u8> {
    frame
        .plane(0)
        .and_then(|p| p.row(0))
        .and_then(|r| r.first())
        .copied()
}

/// Copies the secondary frame's first byte into the main frame.
///
/// Two inputs, one output, and nothing else — so an output byte names exactly
/// which secondary frame the synchroniser chose, and a zero says it chose none.
#[derive(Debug, Clone, Copy)]
pub struct Stamp {
    opts: FrameSyncOpts,
    roles: Roles,
}

/// Which family of per-input roles a [`Stamp`] declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Roles {
    /// Input 0 drives; the rest are sampled and may be absent early.
    /// `overlay`, `blend`, `lut2`.
    Dual,
    /// Every input drives and every input must have started. `hstack`,
    /// `vstack`, `maskedmerge`.
    Uniform,
}

impl Stamp {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "stamp",
        description: "write the secondary frame's value into the main frame",
        inputs: DUAL_VIDEO_PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    /// A filter with the dual-input roles and these options.
    #[must_use]
    pub const fn new(opts: FrameSyncOpts) -> Self {
        Self {
            opts,
            roles: Roles::Dual,
        }
    }

    /// A filter with the uniform roles.
    #[must_use]
    pub const fn uniform(opts: FrameSyncOpts) -> Self {
        Self {
            opts,
            roles: Roles::Uniform,
        }
    }

    /// What this filter's pads accept: `gray8` everywhere, all tied.
    #[must_use]
    pub fn formats(label: &str) -> NodeFormats {
        NodeFormats::uniform(
            2,
            1,
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Gray8),
            label,
        )
    }

    /// Wrap it in the adapter, ready for `Graph::add`.
    #[must_use]
    pub fn boxed(self) -> Box<Synced<Self>> {
        Box::new(Synced::new(self))
    }
}

impl FrameSyncFilter for Stamp {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        match self.roles {
            Roles::Dual => FsInput::dual(n),
            Roles::Uniform => FsInput::uniform(n),
        }
    }

    fn opts(&self) -> FrameSyncOpts {
        self.opts
    }

    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let value = event.get(1).and_then(first_byte).unwrap_or(0);
        let Some(mut main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        main.pts = event.timestamp();
        main.time_base = event.time_base();
        if let Some(mut plane) = main.plane_mut(0) {
            for row in plane.rows_mut() {
                for byte in row.iter_mut() {
                    *byte = value;
                }
            }
        }
        Ok(FrameOut::One(main))
    }
}
