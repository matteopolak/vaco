//! Raw `.flac`: `"fLaC"` followed by metadata blocks, then a back-to-back
//! sequence of self-delimiting FLAC frames.
//!
//! Specification: RFC 9639 §8 (stream structure).
//!
//! # Why this is a muxer only, and a thin one
//!
//! There is no demuxer here — decoding FLAC already goes through
//! `vaco-codec-flac`'s own `Decoder`, and reading a bare `.flac` file's
//! frames back out is `vaco-demux-raw`'s job (the same "self-delimiting
//! elementary stream, no container index" shape as `.h264`/`.aac`), not a
//! second implementation of that walk here.
//!
//! # Why the header is just `params.extradata`, verbatim
//!
//! `vaco-codec-flac::FlacEncoder::extradata()` already returns exactly
//! `"fLaC"` followed by the `STREAMINFO` metadata block, marked as the last
//! one — the complete raw-FLAC file header, byte for byte, because that is
//! also what a container's own `CodecPrivate`/`extradata` channel wants
//! (E2E-GAPS #2's `Encoder::extradata`/`Encoder::prime_audio` pair is what
//! makes it reach here before `write_header` needs it: `Muxer::add_stream`
//! runs once, before a single frame is encoded, and there is no later hook
//! this trait offers to patch the header in afterwards). So this muxer does
//! not reconstruct `STREAMINFO` from `CodecParameters`' scattered fields —
//! it writes the encoder's own header unmodified, which is also, by
//! construction, exactly what `vaco-mux-matroska`'s `CodecPrivate` for the
//! same encoder carries.
//!
//! Every packet after that is one already-framed FLAC frame
//! (`vaco-codec-flac::encoder`'s own sync code, header, subframes and
//! footer CRC) — this muxer never touches frame payloads, only concatenates
//! them.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Rational, Result};
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

#[derive(Debug)]
pub struct FlacMuxer {
    out: IoWriter,
    sample_rate: Option<u32>,
    header_written: bool,
    added: bool,
    /// `params.extradata` from `add_stream`, taken by `write_header` — see
    /// the module doc for why this is the whole header, unmodified.
    pending_extradata: Option<Vec<u8>>,
}

impl FlacMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            sample_rate: None,
            header_written: false,
            added: false,
            pending_extradata: None,
        })
    }
}

impl Muxer for FlacMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.added {
            return Err(Error::Unsupported("flac: only one stream is supported"));
        }
        if params.codec_id != Some(CodecId::Flac) {
            return Err(Error::Unsupported(
                "flac: this container only carries the flac codec",
            ));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("flac: not an audio stream"))?;
        self.sample_rate = Some(audio.sample_rate.max(1));
        self.pending_extradata.clone_from(&params.extradata);
        self.added = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if !self.added {
            return Err(Error::InvalidData("flac: no stream added"));
        }
        // `Encoder::extradata`'s doc: `None` by default, `Some` only from an
        // encoder that overrides it (`FlacEncoder` does, once primed or
        // once it has seen a frame) — a copied stream from a source that
        // never carried one (or an encoder this build has not wired the
        // same way) has nothing valid to write here, and guessing a
        // `STREAMINFO` this crate did not itself measure is exactly the
        // kind of synthesis this whole batch's other fixes replaced with a
        // real answer or a refusal.
        let Some(extradata) = self.pending_extradata.take() else {
            return Err(Error::Unsupported(
                "flac: no STREAMINFO available for this stream (fLaC extradata missing)",
            ));
        };
        self.out.write(&extradata)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("flac: packet written before the header"));
        }
        self.out.write(packet.payload())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index != 0 {
            return None;
        }
        self.sample_rate.map(|r| Rational::new(1, r.cast_signed()))
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("flac: trailer written before the header"));
        }
        self.out.flush()
    }
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "flac",
    long_name: "raw FLAC",
    extensions: &["flac"],
    default_video: None,
    default_audio: Some(CodecId::Flac),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(FlacMuxer::new(sink)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::AudioParameters;
    use vaco_core::MediaType;
    use vaco_format_core::vacoraw::MemorySink;

    fn params_with_extradata(extradata: &[u8]) -> CodecParameters {
        let mut p = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Flac);
        p.audio = Some(AudioParameters {
            sample_rate: 44_100,
            ..AudioParameters::default()
        });
        p.extradata = Some(extradata.to_vec());
        p
    }

    #[test]
    fn the_header_is_the_encoders_own_extradata_verbatim() {
        let sink = MemorySink::new();
        let buf = sink.shared();
        let mut m = FlacMuxer::new(Box::new(sink)).unwrap();
        let extradata = b"fLaC\x80\x00\x00\x22whatever-streaminfo-bytes-go-here".to_vec();
        m.add_stream(&params_with_extradata(&extradata)).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        assert_eq!(buf.snapshot(), extradata);
    }

    #[test]
    fn packets_are_concatenated_verbatim_after_the_header() {
        let sink = MemorySink::new();
        let buf = sink.shared();
        let mut m = FlacMuxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params_with_extradata(b"fLaC-header")).unwrap();
        m.write_header().unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let p1 = Packet::from_slice(&mut budget, &[0xFF, 0xF8, 1, 2, 3]).unwrap();
        let p2 = Packet::from_slice(&mut budget, &[0xFF, 0xF8, 4, 5]).unwrap();
        m.write_packet(&p1).unwrap();
        m.write_packet(&p2).unwrap();
        m.write_trailer().unwrap();
        let mut want = b"fLaC-header".to_vec();
        want.extend_from_slice(&[0xFF, 0xF8, 1, 2, 3]);
        want.extend_from_slice(&[0xFF, 0xF8, 4, 5]);
        assert_eq!(buf.snapshot(), want);
    }

    #[test]
    fn a_second_stream_is_refused() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        m.add_stream(&params_with_extradata(b"fLaC-header")).unwrap();
        assert!(m.add_stream(&params_with_extradata(b"fLaC-header")).is_err());
    }

    #[test]
    fn a_non_flac_codec_is_refused() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        let mut p = params_with_extradata(b"irrelevant");
        p.codec_id = Some(CodecId::PcmS16le);
        assert!(m.add_stream(&p).is_err());
    }

    #[test]
    fn writing_the_header_without_extradata_is_refused_not_guessed() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        let mut p = params_with_extradata(b"unused");
        p.extradata = None;
        m.add_stream(&p).unwrap();
        assert!(m.write_header().is_err());
    }
}
