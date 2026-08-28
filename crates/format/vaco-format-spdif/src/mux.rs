//! The `spdif` muxer: one fixed-size IEC 61937 burst per AC-3 frame.
//!
//! Measured against `ffmpeg -f spdif` (see `iec61937.rs`'s and `demux.rs`'s
//! module docs for the exact numbers): every burst is
//! [`crate::demux::AC3_BURST_BYTES`] (6144) bytes — `Pa Pb Pc Pd` (8 bytes,
//! little-endian `u16` words by default) then the AC-3 frame's bytes with
//! every adjacent pair swapped, then zero padding out to 6144.

use crate::ac3;
use crate::iec61937::{BurstHeader, DATA_TYPE_AC3, swap_payload};
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Result};
use vaco_format_core::{FormatFlags, Muxer};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use crate::demux::AC3_BURST_BYTES;

/// A fixed, small (< 6144 byte) zero buffer for burst padding — not sized
/// from any input, so no `Budget` involvement is warranted. Module-level so
/// clippy's `items_after_statements` does not flag a `static` declared
/// partway through `write_packet`.
static ZEROS: [u8; AC3_BURST_BYTES] = [0u8; AC3_BURST_BYTES];

/// The `spdif` muxer.
pub struct SpdifMuxer {
    sink: Box<dyn MediaSink>,
    /// Mirrors `-spdif_flags be`. See `iec61937.rs`'s module docs for why
    /// this is a plain `bool` and not a shared `Endian` type.
    big_endian: bool,
    audio_stream: Option<u32>,
}

impl std::fmt::Debug for SpdifMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpdifMuxer")
            .field("big_endian", &self.big_endian)
            .field("audio_stream", &self.audio_stream)
            .finish_non_exhaustive()
    }
}

impl SpdifMuxer {
    /// A muxer with the reference's default byte order (little-endian
    /// bursts — `-spdif_flags be` is not set).
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            big_endian: false,
            audio_stream: None,
        }
    }

    /// Mirrors `-spdif_flags be`. Measured (see `iec61937.rs`): a burst
    /// written this way was **not** readable back by `ffmpeg -f spdif` in
    /// this crate's own testing, so this is offered for producers that want
    /// big-endian S/PDIF output specifically, not as a round-trippable mode
    /// of this crate's own demuxer.
    #[must_use]
    pub const fn big_endian(mut self) -> Self {
        self.big_endian = true;
        self
    }
}

impl Muxer for SpdifMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if params.media_type != Some(MediaType::Audio) {
            return Err(Error::Unsupported("spdif: audio-only container"));
        }
        if let Some(codec) = params.codec_id
            && codec != vaco_codec_core::CodecId::Ac3
        {
            return Err(Error::Unsupported(
                "spdif: only AC-3 (data type 1) is supported",
            ));
        }
        if self.audio_stream.is_some() {
            return Err(Error::Unsupported(
                "spdif: only one audio stream is supported",
            ));
        }
        self.audio_stream = Some(0);
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if packet.stream_index != 0 {
            return Err(Error::Unsupported("spdif: unknown stream index"));
        }
        let payload = packet.payload();
        // A real AC-3 frame decodes with `ac3::parse`; this is not used for
        // muxing logic, only as the same range check the demuxer would
        // apply on the way back in, so a caller handing this muxer
        // non-AC-3 bytes under an AC-3-tagged stream fails loudly here
        // rather than producing a burst nothing can read.
        if ac3::parse(payload).is_none() {
            return Err(Error::InvalidData(
                "spdif: packet does not start with a valid AC-3 sync frame header",
            ));
        }
        let pd_bits = u16::try_from(payload.len().saturating_mul(8))
            .map_err(|_| Error::InvalidData("spdif: AC-3 frame too large for a Pd length field"))?;
        let header = BurstHeader {
            pc: DATA_TYPE_AC3,
            pd: pd_bits,
        };
        let header_bytes = header.write(self.big_endian);
        let total = header_bytes.len().saturating_add(payload.len());
        if total > AC3_BURST_BYTES {
            return Err(Error::InvalidData(
                "spdif: AC-3 frame does not fit in a 6144-byte burst",
            ));
        }
        self.sink.write(&header_bytes)?;
        self.sink.write(&swap_payload(payload))?;
        let padding = AC3_BURST_BYTES.saturating_sub(total);
        let Some(pad) = ZEROS.get(..padding) else {
            return Err(Error::InvalidData("spdif: padding computation overflowed"));
        };
        self.sink.write(pad)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::SharedDynBuf;
    use vaco_limits::{Budget, Limits};

    /// A real (truncated but header-valid) AC-3 frame: the exact bytes
    /// measured in `ac3.rs`'s module docs, padded to a plausible frame
    /// length. Truncated content past the header does not matter to the
    /// muxer, which never looks past what `ac3::parse` reads.
    fn ac3_frame(len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        v[0] = 0x0B;
        v[1] = 0x77;
        v[4] = 0x14; // fscod=0 (48kHz), frmsizecod bits irrelevant here
        v[6] = 0x40; // acmod=2 (stereo)
        v
    }

    fn packet(bytes: &[u8]) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
        pkt.stream_index = 0;
        pkt
    }

    #[test]
    fn a_frame_is_wrapped_in_a_6144_byte_burst() {
        let sink = SharedDynBuf::new();
        let mirror = sink.clone();
        let mut mux = SpdifMuxer::new(Box::new(sink));
        let idx = mux
            .add_stream(&CodecParameters::new(MediaType::Audio).with_codec(
                vaco_codec_core::CodecId::Ac3,
            ))
            .unwrap();
        assert_eq!(idx, 0);
        mux.write_header().unwrap();
        mux.write_packet(&packet(&ac3_frame(768))).unwrap();
        mux.write_trailer().unwrap();
        let bytes = mirror.take();
        assert_eq!(bytes.len(), AC3_BURST_BYTES);
        assert_eq!(&bytes[0..8], &[0x72, 0xF8, 0x1F, 0x4E, 0x01, 0x00, 0x00, 0x18]);
        // The AC-3 sync word 0x0B77 appears byte-swapped in the burst.
        assert_eq!(&bytes[8..10], &[0x77, 0x0B]);
        assert!(bytes[8 + 768..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_video_stream_is_refused() {
        let mut mux = SpdifMuxer::new(Box::new(vaco_io::DynBuf::new()));
        assert!(
            mux.add_stream(&CodecParameters::new(MediaType::Video))
                .is_err()
        );
    }

    #[test]
    fn a_non_ac3_payload_is_refused_at_write_time() {
        let mut mux = SpdifMuxer::new(Box::new(vaco_io::DynBuf::new()));
        mux.add_stream(&CodecParameters::new(MediaType::Audio))
            .unwrap();
        mux.write_header().unwrap();
        assert!(mux.write_packet(&packet(&[0u8; 16])).is_err());
    }
}
