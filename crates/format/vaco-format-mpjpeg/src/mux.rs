//! The MPJPEG muxer: one MIME part per JPEG packet.
//!
//! Measured against `ffmpeg -f mpjpeg` (see `lib.rs` module docs for the
//! exact byte layout): `write_header` emits nothing, every packet becomes
//! `--<tag>\r\nContent-type: image/jpeg\r\nContent-length: N\r\n\r\n` followed
//! by the packet bytes and a bare `\r\n`, and `write_trailer` emits one more
//! `--<tag>\r\n` and nothing else.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// `ffmpeg -h muxer=mpjpeg` -> `-boundary_tag <string> ... (default "ffmpeg")`.
const DEFAULT_BOUNDARY_TAG: &str = "ffmpeg";

/// The MPJPEG muxer.
pub struct MpjpegMuxer {
    sink: Box<dyn MediaSink>,
    boundary_tag: String,
    video_stream: Option<u32>,
}

impl std::fmt::Debug for MpjpegMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpjpegMuxer")
            .field("boundary_tag", &self.boundary_tag)
            .field("video_stream", &self.video_stream)
            .finish_non_exhaustive()
    }
}

impl MpjpegMuxer {
    /// A muxer with the reference's default boundary tag, `"ffmpeg"`.
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            boundary_tag: DEFAULT_BOUNDARY_TAG.to_owned(),
            video_stream: None,
        }
    }

    /// Mirrors `ffmpeg -boundary_tag`.
    #[must_use]
    pub fn with_boundary_tag(mut self, tag: impl Into<String>) -> Self {
        self.boundary_tag = tag.into();
        self
    }
}

impl Muxer for MpjpegMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::NOTIMESTAMPS
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match params.media_type {
            Some(MediaType::Video) => {
                if self.video_stream.is_some() {
                    return Err(Error::Unsupported(
                        "mpjpeg: only one video stream is supported",
                    ));
                }
                self.video_stream = Some(0);
                Ok(0)
            }
            _ => Err(Error::Unsupported("mpjpeg: video-only container")),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        // Nothing precedes the first part.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if packet.stream_index != 0 {
            return Err(Error::Unsupported("mpjpeg: unknown stream index"));
        }
        let payload = packet.payload();
        self.sink.write(b"--")?;
        self.sink.write(self.boundary_tag.as_bytes())?;
        self.sink.write(b"\r\n")?;
        self.sink.write(b"Content-type: image/jpeg\r\n")?;
        self.sink
            .write(format!("Content-length: {}\r\n", payload.len()).as_bytes())?;
        self.sink.write(b"\r\n")?;
        self.sink.write(payload)?;
        self.sink.write(b"\r\n")
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.write(b"--")?;
        self.sink.write(self.boundary_tag.as_bytes())?;
        self.sink.write(b"\r\n")?;
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::SharedDynBuf;
    use vaco_limits::{Budget, Limits};

    fn frame(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
        pkt.stream_index = 0;
        pkt
    }

    #[test]
    fn a_video_frame_round_trips_through_the_wire_format() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpjpegMuxer::new(Box::new(sink));
        let idx = mux
            .add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        assert_eq!(idx, 0);
        mux.write_header().unwrap();
        mux.write_packet(&frame(b"\xff\xd8fakejpeg\xff\xd9")).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert_eq!(
            bytes,
            b"--ffmpeg\r\nContent-type: image/jpeg\r\nContent-length: 12\r\n\r\n\
              \xff\xd8fakejpeg\xff\xd9\r\n--ffmpeg\r\n"
        );
    }

    #[test]
    fn an_audio_packet_is_refused() {
        let mut mux = MpjpegMuxer::new(Box::new(vaco_io::DynBuf::new()));
        assert!(
            mux.add_stream(&CodecParameters::new(MediaType::Audio))
                .is_err()
        );
    }

    #[test]
    fn a_custom_boundary_tag_is_used_verbatim() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = MpjpegMuxer::new(Box::new(sink)).with_boundary_tag("mytag");
        mux.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&frame(b"x")).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert!(bytes.starts_with(b"--mytag\r\n"));
        assert!(bytes.ends_with(b"--mytag\r\n"));
    }
}
