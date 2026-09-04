#![allow(clippy::expect_used, reason = "test fixtures")]

//! Fixtures shared between the unit tests in this crate.
//!
//! Kept out of the public API — the *public* worked examples live in
//! [`crate::mock`], which is what an external test or another crate should
//! reach for.

use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{Rational, Timestamp};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

use crate::LinkFormat;

/// A `gray8` video link at 25 fps.
pub(crate) fn video_link_format(width: u32, height: u32) -> LinkFormat {
    LinkFormat::Video {
        format: PixFmt::Gray8,
        width,
        height,
        time_base: Rational::new(1, 25),
        frame_rate: Rational::new(25, 1),
        sample_aspect_ratio: Rational::ONE,
        color: ColorInfo::default(),
    }
}

/// A `gray8` frame of the given geometry, timestamped in 1/25.
pub(crate) fn video_frame(width: u32, height: u32, pts: i64) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool
        .acquire_video(PixFmt::Gray8, width, height)
        .expect("a small gray8 frame is within every default cap");
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 25);
    frame.set_duration_ticks(1);
    frame
}

/// An `s16` stereo frame at 48 kHz.
pub(crate) fn audio_frame(samples: u32, pts: i64) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool
        .acquire_audio(SampleFmt::S16, ChannelLayout::STEREO, samples, 48_000)
        .expect("a small s16 frame is within every default cap");
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 48_000);
    frame.set_duration_ticks(i64::from(samples));
    frame
}
