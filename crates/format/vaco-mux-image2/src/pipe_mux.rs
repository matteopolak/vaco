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
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

use crate::writer::{Image2MuxOptions, Image2MuxWriter};

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

/// The real per-file writer, reached through the registry once
/// [`Muxer::bind_url`] supplies the pattern. Wraps [`Image2MuxWriter`],
/// which already does the real, correct per-frame filesystem work — it was
/// simply unreachable from the registry path before `bind_url` existed.
#[derive(Debug)]
struct PatternWriterMuxer {
    writer: Image2MuxWriter,
    has_stream: bool,
}

impl Muxer for PatternWriterMuxer {
    fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
        if self.has_stream {
            return Err(Error::Unsupported(
                "image2 carries exactly one stream; use vaco_mux_image2::Image2MuxWriter \
                 directly for multiple inputs",
            ));
        }
        self.has_stream = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        // Nothing to write: image2 has no container-level header, only files.
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.writer.write_frame(packet.payload(), packet.pts.ticks())
    }

    fn write_trailer(&mut self) -> Result<()> {
        // Every frame is already a complete, flushed file by the time
        // `write_packet` returns; there is nothing left to finalise.
        Ok(())
    }
}

/// The `image2` muxer as reached through the registry.
///
/// Starts as [`Image2SinkMuxer`], because that is all [`MuxerDesc::open`]'s
/// frozen signature can construct against one already-open sink, and
/// becomes the real [`PatternWriterMuxer`] the moment a caller supplies the
/// destination pattern through [`Muxer::bind_url`] — a file literally named
/// `out_%03d.png` was the symptom of this seam having no way to reach
/// [`Image2MuxWriter`] at all.
#[derive(Debug)]
enum RegistryMuxer {
    Sink(Image2SinkMuxer),
    Pattern(PatternWriterMuxer),
}

impl Muxer for RegistryMuxer {
    // `NEEDNUMBER` is what tells `vaco-cli`'s `open_output` (mirroring the
    // read side's `DemuxerDesc.flags` check) to skip creating a real
    // destination file for the literal pattern string and call `bind_url`
    // on this throwaway-sink-backed instance instead.
    //
    // `NOTIMESTAMPS`: a still has no inherent timestamp, and a decode leg
    // that produces one genuinely has none to give (`Timestamp::NONE` all
    // the way through, not a stand-in for zero). Without this,
    // `vaco-format-core`'s generic mux-timestamp stage refused the packet
    // outright ("this container needs timestamps and the packet has none"),
    // which made even a single still fail to mux.
    fn flags(&self) -> FormatFlags {
        FormatFlags::NEEDNUMBER.union(FormatFlags::NOTIMESTAMPS)
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        match self {
            Self::Sink(m) => m.add_stream(params),
            Self::Pattern(m) => m.add_stream(params),
        }
    }

    fn write_header(&mut self) -> Result<()> {
        match self {
            Self::Sink(m) => m.write_header(),
            Self::Pattern(m) => m.write_header(),
        }
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        match self {
            Self::Sink(m) => m.write_packet(packet),
            Self::Pattern(m) => m.write_packet(packet),
        }
    }

    fn write_trailer(&mut self) -> Result<()> {
        match self {
            Self::Sink(m) => m.write_trailer(),
            Self::Pattern(m) => m.write_trailer(),
        }
    }

    /// Replace the placeholder sink-backed state with the real per-file
    /// writer, preserving whether a stream was already declared (the order
    /// [`Muxer::bind_url`]'s own doc comment recommends — right after `open`
    /// — is not enforced here defensively, the same way the read-side
    /// `RegistryDemuxer::bind_url` is not).
    ///
    /// # Errors
    /// [`Error::Unsupported`] if already bound. Otherwise whatever
    /// [`Image2MuxWriter::create`] finds wrong with `url` (more than one
    /// `%d` placeholder; a bare filename is legal).
    fn bind_url(&mut self, url: &str) -> Result<()> {
        let has_stream = match self {
            Self::Sink(m) => m.has_stream,
            Self::Pattern(_) => {
                return Err(Error::Unsupported(
                    "this image2 muxer is already bound to a pattern",
                ));
            }
        };
        let writer = Image2MuxWriter::create(url, Image2MuxOptions::default())?;
        *self = Self::Pattern(PatternWriterMuxer { writer, has_stream });
        Ok(())
    }
}

/// The `image2` muxer's registry entry.
pub const MUXER_IMAGE2: MuxerDesc = MuxerDesc {
    name: "image2",
    long_name: "image2 sequence",
    // The reference's list, verbatim and in its order — held once, in
    // `vaco-codec-core`, because the `image2` demuxer's probe and the CLI's
    // output-codec guess read the same list and had drifted from this copy of
    // it. Claiming an extension is independent of whether an encoder for it
    // exists; with an empty list every image extension fell through to a
    // demuxer-only row and the CLI refused an output path it can in fact write.
    extensions: vaco_codec_core::IMAGE_EXTENSIONS,
    // Measured: `ffmpeg -h muxer=image2` -> `Default video codec: mjpeg.`
    // (`CodecId::Jpeg` is spelled `mjpeg` in the reference's own listing.)
    //
    // This is the *fallback*, not the answer: the reference overrides it from
    // the output filename's extension, so `out.png` writes PNG and not JPEG.
    // That override is `vaco_codec_core::image_codec_for_extension`, consulted
    // by the CLI before this field — see `vaco-cli`'s `default_encoder_for`.
    default_video: Some(vaco_codec_core::CodecId::Jpeg),
    default_audio: None,
    open: |sink: Box<dyn MediaSink>| {
        Ok(Box::new(RegistryMuxer::Sink(Image2SinkMuxer::new(sink)?)) as Box<dyn Muxer>)
    },
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

    #[test]
    fn declares_neednumber_and_notimestamps() {
        let sink = SharedDynBuf::new();
        let m = (MUXER_IMAGE2.open)(Box::new(sink)).unwrap();
        assert_eq!(
            m.flags(),
            FormatFlags::NEEDNUMBER.union(FormatFlags::NOTIMESTAMPS)
        );
    }

    /// The registry's frozen `open` can only ever produce the degenerate
    /// one-sink shape, but `Muxer::bind_url` — called with the real
    /// destination pattern right after `open`, exactly as `vaco-cli`'s
    /// `open_output` now does for a `NEEDNUMBER` muxer — rebinds to the
    /// real per-file writer.
    #[test]
    fn open_then_bind_url_writes_one_real_file_per_frame() {
        let dir =
            std::env::temp_dir().join(format!("vaco-mux-image2-test-bind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pattern = dir.join("out%03d.png");
        let pattern = pattern.to_str().unwrap();

        // The registry path's only option: a throwaway placeholder sink,
        // exactly as a caller unable to write the literal pattern string
        // as a real file would pass.
        let placeholder = SharedDynBuf::new();
        let mut m = (MUXER_IMAGE2.open)(Box::new(placeholder)).unwrap();
        m.add_stream(&CodecParameters::new(MediaType::Video))
            .unwrap();
        m.bind_url(pattern).unwrap();
        m.write_header().unwrap();

        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let p1 = Packet::from_slice(&mut budget, b"one").unwrap();
        let p2 = Packet::from_slice(&mut budget, b"two").unwrap();
        m.write_packet(&p1).unwrap();
        m.write_packet(&p2).unwrap();
        m.write_trailer().unwrap();

        assert_eq!(std::fs::read(dir.join("out001.png")).unwrap(), b"one");
        assert_eq!(std::fs::read(dir.join("out002.png")).unwrap(), b"two");

        // A second bind is refused rather than silently re-resolving.
        assert!(matches!(m.bind_url(pattern), Err(Error::Unsupported(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
