//! The registry-reachable `image2` [`Muxer`]: every frame's payload, written
//! consecutively into the one sink the frozen `MuxerDesc::open` seam
//! provides.
//!
//! This is deliberately `image2pipe`'s shape, not the real multi-file
//! `image2`'s — see `crate::writer`'s module docs for why the registry path
//! cannot express the real thing at all, and use
//! [`crate::writer::Image2MuxWriter`] directly when a path pattern is
//! available. What this type *is* correct for: it is the read-side mirror of
//! `vaco-demux-image2::pipe`'s splitters, which already expect exactly this
//! shape of input (`cat *.png | ffmpeg -f image2pipe ... | ffmpeg -f
//! png_pipe -i -` is a real, supported reference pipeline).

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Result};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

/// Writes every packet's payload back to back into one sink. Mirrors
/// `vaco-mux-raw::raw::RawMuxer` almost exactly, for the same reason: no
/// header, no trailer, one stream.
#[derive(Debug)]
pub struct Image2SinkMuxer {
    out: IoWriter,
    has_stream: bool,
}

impl Image2SinkMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            has_stream: false,
        })
    }
}

impl Muxer for Image2SinkMuxer {
    fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
        if self.has_stream {
            return Err(Error::Unsupported(
                "the registry-path image2 muxer carries exactly one stream; \
                 use vaco_mux_image2::Image2MuxWriter directly for multi-file output",
            ));
        }
        self.has_stream = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.out.write(packet.payload())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.out.flush()
    }
}

/// The `image2` muxer's registry entry.
pub const MUXER_IMAGE2: MuxerDesc = MuxerDesc {
    name: "image2",
    long_name: "image2 sequence",
    extensions: &[],
    // Measured: `ffmpeg -h muxer=image2` -> `Default video codec: mjpeg.`
    // (`CodecId::Jpeg` is spelled `mjpeg` in the reference's own listing.)
    default_video: Some(vaco_codec_core::CodecId::Jpeg),
    default_audio: None,
    open: |sink: Box<dyn MediaSink>| Ok(Box::new(Image2SinkMuxer::new(sink)?) as Box<dyn Muxer>),
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::MediaType;
    use vaco_io::SharedDynBuf;

    #[test]
    fn writes_every_packet_payload_back_to_back() {
        let sink = SharedDynBuf::new();
        let readback = sink.clone();
        let mut m = Image2SinkMuxer::new(Box::new(sink)).unwrap();
        m.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        m.write_header().unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let p1 = Packet::from_slice(&mut budget, b"one").unwrap();
        let p2 = Packet::from_slice(&mut budget, b"two").unwrap();
        m.write_packet(&p1).unwrap();
        m.write_packet(&p2).unwrap();
        m.write_trailer().unwrap();
        assert_eq!(readback.snapshot(), b"onetwo");
    }

    #[test]
    fn a_second_stream_is_rejected() {
        let sink = SharedDynBuf::new();
        let mut m = Image2SinkMuxer::new(Box::new(sink)).unwrap();
        m.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        assert!(
            m.add_stream(&CodecParameters::new(MediaType::Video))
                .is_err()
        );
    }

    #[test]
    fn can_be_opened_via_its_own_descriptor() {
        let sink = SharedDynBuf::new();
        let mut m = (MUXER_IMAGE2.open)(Box::new(sink)).unwrap();
        assert!(
            m.add_stream(&CodecParameters::new(MediaType::Video))
                .is_ok()
        );
    }
}
