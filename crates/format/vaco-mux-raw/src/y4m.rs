//! `yuv4mpegpipe` — the one muxer in this crate that is not a verbatim byte
//! dump. See `crate::raw`. `vaco-demux-raw` implements the matching reader
//! independently (this crate does not depend on it); both are derived from
//! the same de facto Y4M grammar rather than from each other.
//!
//! # The format
//!
//! One header line, then one `FRAME\n` marker plus raw planar bytes per
//! picture — see the crate docs for the exact grammar. This muxer writes:
//!
//! ```text
//! YUV4MPEG2 W<width> H<height> F<num>:<den> Ip A0:0 C<space>\n
//! FRAME\n<frame bytes>
//! FRAME\n<frame bytes>
//! ...
//! ```
//!
//! `width`/`height`/`framerate` come from the first (and only) stream's
//! [`vaco_codec_core::VideoParameters`], read at [`Muxer::write_header`] —
//! which is why, unlike `crate::raw::RawMuxer`, this muxer must buffer the
//! declared parameters from `add_stream` until then.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

fn colorspace_tag(format: Option<PixFmt>) -> &'static str {
    match format {
        Some(PixFmt::Yuv422p) => "422",
        Some(PixFmt::Yuv444p) => "444",
        Some(PixFmt::Gray8) => "mono",
        // Plain 4:2:0 (the spec's own default) covers everything else this
        // crate can currently express; see `vaco-demux-raw`'s equivalent note.
        _ => "420jpeg",
    }
}

/// The `yuv4mpegpipe` muxer.
#[derive(Debug)]
pub struct Yuv4MpegMuxer {
    out: IoWriter,
    params: Option<CodecParameters>,
    header_written: bool,
}

impl Yuv4MpegMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            params: None,
            header_written: false,
        })
    }
}

impl Muxer for Yuv4MpegMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.params.is_some() {
            return Err(Error::Unsupported(
                "yuv4mpegpipe carries exactly one video stream",
            ));
        }
        // Measured: `ffmpeg -c copy -f yuv4mpegpipe` on an H.264 source fails
        // at `write_header` with "Codec not supported" — Y4M has no
        // configuration record for an encoded bitstream, only a raw frame
        // per `FRAME` marker. `Rawvideo` is this crate's own tag for that;
        // `WrappedAvframe` is `MUXER_YUV4MPEGPIPE::default_video`, the
        // reference's own pseudo-codec for "whatever the encoder decoded".
        // A `None` codec id is let through rather than rejected, matching
        // this crate's other muxers' tolerance for metadata nobody filled in.
        if let Some(codec_id) = params.codec_id
            && !matches!(codec_id, CodecId::Rawvideo | CodecId::WrappedAvframe)
        {
            return Err(Error::Unsupported("yuv4mpegpipe carries raw video only"));
        }
        self.params = Some(params.clone());
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        let Some(video) = self.params.as_ref().and_then(|p| p.video.as_ref()) else {
            return Err(Error::InvalidData(
                "yuv4mpegpipe needs a video stream with known geometry",
            ));
        };
        if video.width == 0 || video.height == 0 {
            return Err(Error::InvalidData(
                "yuv4mpegpipe needs a nonzero picture size",
            ));
        }
        let rate = if video.frame_rate.is_defined() && !video.frame_rate.is_zero() {
            video.frame_rate
        } else {
            vaco_core::Rational::new(25, 1)
        };
        let line = format!(
            "YUV4MPEG2 W{} H{} F{}:{} Ip A0:0 C{}\n",
            video.width,
            video.height,
            rate.num,
            rate.den,
            colorspace_tag(video.format)
        );
        self.out.write(line.as_bytes())?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        self.out.write(b"FRAME\n")?;
        self.out.write(packet.payload())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.out.flush()
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(Yuv4MpegMuxer::new(sink)?))
}

/// Measured: `ffmpeg -h muxer=yuv4mpegpipe` -> `wrapped_avframe`, the
/// reference's passthrough pseudo-codec, and no audio default.
pub const MUXER_YUV4MPEGPIPE: MuxerDesc = MuxerDesc {
    name: "yuv4mpegpipe",
    long_name: "YUV4MPEG pipe",
    extensions: &["y4m"],
    default_video: Some(vaco_codec_core::CodecId::WrappedAvframe),
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;
    use vaco_core::MediaType;
    use vaco_format_core::vacoraw::MemorySink;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn an_encoded_codec_is_rejected() {
        // Measured: `ffmpeg -c copy -f yuv4mpegpipe` on an H.264 source
        // fails with "Codec not supported" rather than dumping the
        // bitstream bytes as if they were a raw frame.
        let sink = Box::new(MemorySink::new());
        let mut m = Yuv4MpegMuxer::new(sink).unwrap();
        let mut params = CodecParameters::new(MediaType::Video);
        params.codec_id = Some(CodecId::H264);
        params.video = Some(VideoParameters::default());
        assert!(m.add_stream(&params).is_err());
    }

    #[test]
    fn header_and_frames_round_trip() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = Yuv4MpegMuxer::new(Box::new(sink)).unwrap();
        let mut params = CodecParameters::new(MediaType::Video);
        params.video = Some(VideoParameters {
            width: 4,
            height: 4,
            frame_rate: vaco_core::Rational::new(25, 1),
            format: Some(PixFmt::Yuv420p),
            ..VideoParameters::default()
        });
        m.add_stream(&params).unwrap();
        m.write_header().unwrap();
        let mut budget = Budget::new(Limits::strict());
        let payload = vec![7u8; 24];
        let p = Packet::from_slice(&mut budget, &payload).unwrap();
        m.write_packet(&p).unwrap();
        m.write_trailer().unwrap();
        let out = shared.snapshot();
        assert!(out.starts_with(b"YUV4MPEG2 W4 H4 F25:1"));
        let frame_at = out.windows(6).position(|w| w == b"FRAME\n").unwrap();
        assert_eq!(&out[frame_at + 6..], payload.as_slice());
    }

    #[test]
    fn a_zero_size_stream_is_rejected_at_the_header() {
        let sink = MemorySink::new();
        let mut m = Yuv4MpegMuxer::new(Box::new(sink)).unwrap();
        m.add_stream(&CodecParameters::video()).unwrap();
        assert!(m.write_header().is_err());
    }

    #[test]
    fn a_second_stream_is_rejected() {
        let sink = MemorySink::new();
        let mut m = Yuv4MpegMuxer::new(Box::new(sink)).unwrap();
        assert!(m.add_stream(&CodecParameters::video()).is_ok());
        assert!(m.add_stream(&CodecParameters::video()).is_err());
    }
}
