//! Serialise a [`Frame`] as a YUV4MPEG2 stream.
//!
//! `x264 --demuxer y4m` and `x265 --y4m` both read this format directly off
//! stdin, self-describing width, height, frame rate, interlacing and pixel
//! aspect ratio in one header line rather than needing them repeated on the
//! command line — one less place for this crate's arguments and the actual
//! frame data to disagree.
//!
//! # The colour-space tag, measured rather than guessed
//!
//! The Y4M spec (`mjpeg-tools`) lets the header's `C` field name a colour
//! space; a reader that does not recognise the tag is supposed to fall back
//! to 4:2:0. This crate always emits `yuv420p` frames, so the only question
//! is which of Y4M's several 4:2:0 spellings (`420jpeg`, `420mpeg2`,
//! `420paldv`) to write. Measured directly rather than assumed (D17):
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 \
//!     -pix_fmt yuv420p test.y4m
//! $ head -c 32 test.y4m
//! YUV4MPEG2 W64 H64 F5:1 Ip A1:1 C420jpeg
//! ```
//!
//! `ffmpeg 8.1`'s own Y4M muxer writes `C420jpeg` for `yuv420p`, so that is
//! what this module writes too. `x264`/`x265` both ignore chroma siting for
//! decode purposes (H.264/H.265 have no siting field of their own), so the
//! three 4:2:0 spellings are interchangeable for encoding — this only
//! matters for round-tripping through another Y4M-reading tool that does
//! care, which is out of scope here.

use std::io::{self, Write};

use vaco_core::{Rational, Result};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

/// The one pixel format this module knows how to write.
pub const SUPPORTED_FORMAT: PixFmt = PixFmt::Yuv420p;

/// `Y4mGeometry` pulled from the first frame of a stream — fixed for the whole
/// encode, since Y4M's header line is written once.
#[derive(Debug, Clone, Copy)]
pub struct Y4mGeometry {
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    pub sar: Rational,
}

impl Y4mGeometry {
    /// Read geometry off `frame`, and the frame rate implied by its
    /// `duration`/`time_base` (`fps = 1 / (duration * time_base)`).
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] for anything but a
    /// [`SUPPORTED_FORMAT`] video frame.
    pub fn from_frame(frame: &Frame) -> Result<Self> {
        let FrameData::Video { format, width, height, .. } = &frame.data else {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-exec: not a video frame",
            ));
        };
        if *format != SUPPORTED_FORMAT {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-exec: only yuv420p input is implemented",
            ));
        }
        // fps = 1 / (duration_ticks * time_base) = time_base.den /
        // (duration_ticks * time_base.num). Falls back to a nominal 25fps
        // when the duration is unknown (zero) or the arithmetic would
        // divide by zero — a wrong tag in the Y4M header changes only the
        // rate-control heuristics `x264`/`x265` apply, not the geometry or
        // pixel content, so this is a quality knob, not a correctness one.
        let tb = frame.time_base;
        let ticks = frame.duration.0;
        let fps = if ticks > 0 && tb.num > 0 {
            i64::from(tb.den)
                .checked_div(ticks.saturating_mul(i64::from(tb.num)))
                .and_then(|v| i32::try_from(v).ok())
                .map_or(Rational { num: 25, den: 1 }, |num| Rational { num: num.max(1), den: 1 })
        } else {
            Rational { num: 25, den: 1 }
        };
        let sar = if frame.sample_aspect_ratio.den > 0 {
            frame.sample_aspect_ratio
        } else {
            Rational::ONE
        };
        Ok(Self { width: *width, height: *height, fps, sar })
    }

    /// The one Y4M header line this stream will ever write.
    #[must_use]
    pub fn header_line(&self) -> String {
        format!(
            "YUV4MPEG2 W{} H{} F{}:{} Ip A{}:{} C420jpeg\n",
            self.width, self.height, self.fps.num, self.fps.den, self.sar.num, self.sar.den
        )
    }
}

/// Write the stream header once, before the first frame.
///
/// # Errors
/// Whatever `out.write_all` returns.
pub fn write_header(out: &mut impl Write, geometry: &Y4mGeometry) -> io::Result<()> {
    out.write_all(geometry.header_line().as_bytes())
}

/// Write one `FRAME` marker plus its three planes, trimmed to each plane's
/// logical row width (dropping any alignment padding `stride` carries).
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] if `frame` is not a [`SUPPORTED_FORMAT`]
/// video frame of the geometry `write_header` was given; otherwise whatever
/// `out.write_all` returns, wrapped in [`vaco_core::Error::Io`].
pub fn write_frame(out: &mut impl Write, frame: &Frame) -> Result<()> {
    let FrameData::Video { format, width, height, planes } = &frame.data else {
        return Err(vaco_core::Error::Unsupported(
            "vaco-codec-exec: not a video frame",
        ));
    };
    if *format != SUPPORTED_FORMAT {
        return Err(vaco_core::Error::Unsupported(
            "vaco-codec-exec: only yuv420p input is implemented",
        ));
    }
    out.write_all(b"FRAME\n").map_err(vaco_core::Error::Io)?;
    for (index, plane) in planes.iter().enumerate() {
        let plane_index = u8::try_from(index).unwrap_or(u8::MAX);
        let row_bytes = format.min_stride(*width, plane_index);
        let rows = format.plane_height(*height, plane_index) as usize;
        let data = plane.data.as_slice();
        for row in 0..rows {
            let start = row.saturating_mul(plane.stride);
            let end = start.saturating_add(row_bytes).min(data.len());
            let start = start.min(end);
            out.write_all(data.get(start..end).unwrap_or(&[])).map_err(vaco_core::Error::Io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the crate, not the untrusted-input surface"
)]
mod tests {
    use vaco_limits::{Budget, Limits};

    use super::*;

    #[test]
    fn header_line_matches_the_measured_ffmpeg_shape() {
        let geom = Y4mGeometry { width: 64, height: 64, fps: Rational { num: 5, den: 1 }, sar: Rational::ONE };
        assert_eq!(geom.header_line(), "YUV4MPEG2 W64 H64 F5:1 Ip A1:1 C420jpeg\n");
    }

    #[test]
    fn write_frame_emits_exactly_the_logical_plane_bytes() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 4, 2).unwrap();
        if let FrameData::Video { planes, .. } = &mut frame.data {
            for plane in planes.iter_mut() {
                plane.data.make_mut().fill(0x42);
            }
        }
        let mut out = Vec::new();
        write_frame(&mut out, &frame).unwrap();
        // "FRAME\n" + 4*2 luma + 2*1 + 2*1 chroma (4:2:0 at 4x2 rounds each
        // chroma plane to 2x1).
        assert_eq!(out.len(), 6 + 8 + 2 + 2);
        assert!(out[6..].iter().all(|&b| b == 0x42));
    }
}
