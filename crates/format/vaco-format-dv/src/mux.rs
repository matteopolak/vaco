//! The DV "muxer": write already-framed DV frames back out.
//!
//! There is nothing to multiplex. A packet handed to
//! [`DvMuxer::write_packet`] is expected to already be one complete,
//! correctly-sized DV frame — exactly what [`crate::demux::DvDemuxer`]
//! produces — and this writes its bytes verbatim. No header, no trailer, no
//! index: the file *is* the frame sequence, so there is nowhere else for
//! structure to go.
//!
//! # What this does not do
//!
//! There is no DV video encoder anywhere in this workspace, so nothing
//! today produces a from-scratch DV video packet for this muxer to accept
//! beyond what the demuxer already emitted. Audio packets are accepted
//! ([`Muxer::add_stream`] does not reject an audio stream) but silently
//! dropped on write: interleaving PCM samples into a DV frame's AAUX/audio
//! blocks needs the same sample-deinterleaving this crate's demuxer defers
//! (see `demux.rs`'s module docs), and doing it one-sided — accepting an
//! audio track only to discard it — would be worse than refusing it
//! outright. This crate refuses instead: [`DvMuxer::write_packet`] returns
//! [`vaco_core::Error::Unsupported`] for a non-video stream.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// The DV muxer.
pub struct DvMuxer {
    sink: Box<dyn MediaSink>,
    video_stream: Option<u32>,
    frame_size: Option<usize>,
}

impl std::fmt::Debug for DvMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DvMuxer")
            .field("video_stream", &self.video_stream)
            .field("frame_size", &self.frame_size)
            .finish_non_exhaustive()
    }
}

impl DvMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            video_stream: None,
            frame_size: None,
        }
    }
}

impl Muxer for DvMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::FIXED_FRAMESIZE.union(FormatFlags::GENERIC_INDEX)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match params.media_type {
            Some(MediaType::Video) => {
                if self.video_stream.is_some() {
                    return Err(Error::Unsupported("dv: only one video stream is supported"));
                }
                self.video_stream = Some(0);
                Ok(0)
            }
            Some(MediaType::Audio) => {
                // Accepted so a caller enumerating streams from a DV source
                // and re-muxing them elsewhere does not fail solely because
                // this muxer cannot carry the audio — `write_packet` is
                // where an actual audio packet is refused. Index 1 mirrors
                // `DvDemuxer::open`'s stream order.
                Ok(1)
            }
            _ => Err(Error::Unsupported("dv: unsupported media type")),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        // Nothing to write: the file has no header, only frames.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if packet.stream_index != 0 {
            return Err(Error::Unsupported(
                "dv: audio packets cannot be written (no sample-interleaving support; see docs)",
            ));
        }
        let payload = packet.payload();
        if let Some(expected) = self.frame_size {
            if payload.len() != expected {
                return Err(Error::InvalidData(
                    "dv: frame size changed mid-stream, which DV cannot represent",
                ));
            }
        } else {
            self.frame_size = Some(payload.len());
        }
        self.sink.write(payload)
    }

    fn write_trailer(&mut self) -> Result<()> {
        // No trailer either.
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::DynBuf;
    use vaco_limits::{Budget, Limits};

    fn frame(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
        pkt.stream_index = 0;
        pkt
    }

    #[test]
    fn a_video_frame_is_written_verbatim() {
        let mut mux = DvMuxer::new(Box::new(DynBuf::new()));
        let idx = mux
            .add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        assert_eq!(idx, 0);
        mux.write_header().unwrap();
        mux.write_packet(&frame(&[0xAA; 32])).unwrap();
        mux.write_trailer().unwrap();
    }

    #[test]
    fn an_audio_packet_is_refused_at_write_time() {
        let mut mux = DvMuxer::new(Box::new(DynBuf::new()));
        mux.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Audio))
            .unwrap();
        mux.write_header().unwrap();
        let mut pkt = frame(&[0u8; 4]);
        pkt.stream_index = 1;
        assert!(mux.write_packet(&pkt).is_err());
    }

    #[test]
    fn a_changed_frame_size_mid_stream_is_refused() {
        let mut mux = DvMuxer::new(Box::new(DynBuf::new()));
        mux.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&frame(&[0u8; 32])).unwrap();
        assert!(mux.write_packet(&frame(&[0u8; 16])).is_err());
    }

    #[test]
    fn round_trips_bytes_through_a_dynbuf() {
        let sink = vaco_io::SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = DvMuxer::new(Box::new(sink));
        mux.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        mux.write_header().unwrap();
        mux.write_packet(&frame(b"hello-dv-frame")).unwrap();
        mux.write_trailer().unwrap();
        assert_eq!(mirror.take(), b"hello-dv-frame");
    }
}
