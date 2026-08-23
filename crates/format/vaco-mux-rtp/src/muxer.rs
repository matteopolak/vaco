//! [`RtpMuxer`] — the registered `rtp` muxer.
//!
//! One stream only (RFC 3550 §5.2: one SSRC, one clock — carrying more than
//! one media in one RTP session needs a session-level SDP this trait has no
//! room for, which is exactly why RTSP's `SETUP` allocates a *separate*
//! transport per track rather than multiplexing several codecs onto one
//! `rtp:` sink). A second [`Muxer::add_stream`] call is refused.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use crate::registry::{Packetizer, packetizer_for};

/// This muxer's own MTU cap on a packetiser's output — `ffmpeg -h
/// muxer=rtp` has no such option (the reference reads the underlying
/// protocol's `pkt_size`), so this is this crate's own choice: 1200 bytes,
/// comfortably under Ethernet's 1500-byte MTU once IP/UDP/RTP headers are
/// accounted for, which is the same conservative value several real-world
/// RTP stacks default to.
pub(crate) const DEFAULT_MTU: usize = 1200;

/// A time-derived, not cryptographically random, per-process seed — good
/// enough for an SSRC/initial sequence number (RFC 3550 places no security
/// requirement on either), and this workspace declares no RNG crate (D10).
fn time_seed() -> u32 {
    let now = vaco_time::Instant::now();
    let bits = format!("{now:?}");
    let mut h: u32 = 0x811C_9DC5;
    for b in bits.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The `rtp` muxer.
pub struct RtpMuxer {
    sink: Box<dyn MediaSink>,
    packetizer: Option<Box<dyn Packetizer>>,
    payload_type: u8,
    clock_rate: u32,
    ssrc: u32,
    sequence_number: u16,
    mtu: usize,
    have_stream: bool,
}

impl std::fmt::Debug for RtpMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtpMuxer")
            .field("payload_type", &self.payload_type)
            .field("clock_rate", &self.clock_rate)
            .field("ssrc", &self.ssrc)
            .finish_non_exhaustive()
    }
}

impl RtpMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        let seed = time_seed();
        Self {
            sink,
            packetizer: None,
            payload_type: 96,
            clock_rate: 90_000,
            ssrc: seed,
            sequence_number: (seed >> 16) as u16,
            mtu: DEFAULT_MTU,
            have_stream: false,
        }
    }
}

fn static_payload_type_for(codec: vaco_codec_core::CodecId) -> Option<(u8, u32)> {
    vaco_format_rtp::payload::STATIC_PAYLOADS
        .iter()
        .find(|row| row.codec == Some(codec))
        .map(|row| (row.payload_type, row.clock_rate))
}

impl Muxer for RtpMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::TS_DISCONT
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.have_stream {
            return Err(Error::Unsupported(
                "the rtp muxer carries exactly one stream",
            ));
        }
        let Some(codec) = params.codec_id else {
            return Err(Error::Unsupported("the rtp muxer needs a known codec id"));
        };
        let Some((_name, factory)) = packetizer_for(codec) else {
            return Err(Error::Unsupported(
                "no RTP packetiser is implemented for this codec",
            ));
        };
        let (pt, clock_rate) = static_payload_type_for(codec).unwrap_or((96, 90_000));
        self.payload_type = pt;
        self.clock_rate = clock_rate;
        self.packetizer = Some(factory());
        self.have_stream = true;
        let _ = params.effective_media_type().unwrap_or(MediaType::Data);
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if !self.have_stream {
            return Err(Error::Unsupported(
                "write_header called with no stream added",
            ));
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let Some(packetizer) = self.packetizer.as_mut() else {
            return Err(Error::Unsupported("write_packet called before add_stream"));
        };
        let payloads = packetizer.packetize(packet.payload(), self.mtu);
        let Some(pts) = packet.pts.ticks() else {
            return Err(Error::InvalidData(
                "RTP packets need a pts to derive an RTP timestamp",
            ));
        };
        // `pts` is in the stream's time base (RFC 3550's own clock); this
        // muxer does not itself rescale — `vaco-sched`'s interleave stage is
        // where that belongs, per `Muxer::stream_time_base`'s own docs.
        let rtp_timestamp = u32::try_from(pts & 0xFFFF_FFFF).unwrap_or(0);
        let last = payloads.len().saturating_sub(1);
        for (i, payload) in payloads.iter().enumerate() {
            let header = vaco_format_rtp::RtpHeader {
                version: vaco_format_rtp::RTP_VERSION,
                padding: false,
                extension: false,
                marker: i == last,
                payload_type: self.payload_type,
                sequence_number: self.sequence_number,
                timestamp: rtp_timestamp,
                ssrc: self.ssrc,
                csrc_count: 0,
            };
            self.sequence_number = self.sequence_number.wrapping_add(1);
            let bytes = vaco_format_rtp::rtp::build_basic(&header, payload);
            self.sink.write(&bytes)?;
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.flush()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index == 0 {
            Some(Rational::new(
                1,
                i32::try_from(self.clock_rate).unwrap_or(90_000),
            ))
        } else {
            None
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn-pointer signature exactly"
)]
fn open_rtp_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(RtpMuxer::new(sink)))
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "rtp",
    long_name: "RTP output",
    extensions: &[],
    default_video: Some(vaco_codec_core::CodecId::Mpeg4),
    default_audio: Some(vaco_codec_core::CodecId::PcmMulaw),
    open: open_rtp_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use vaco_core::Timestamp;

    #[derive(Default)]
    struct RecordingSink {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl MediaSink for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> Result<()> {
            self.writes.lock().unwrap().push(buf.to_vec());
            Ok(())
        }
        fn seek(&mut self, _pos: u64) -> Result<u64> {
            Err(Error::NotSeekable)
        }
        fn position(&self) -> u64 {
            0
        }
        fn is_seekable(&self) -> bool {
            false
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_one_rtp_packet_per_write_call() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = RecordingSink {
            writes: writes.clone(),
        };
        let mut mux = RtpMuxer::new(Box::new(sink));
        let params = CodecParameters::audio().with_codec(vaco_codec_core::CodecId::PcmMulaw);
        mux.add_stream(&params).unwrap();
        mux.write_header().unwrap();

        let mut limits_budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let mut pkt = Packet::from_slice(&mut limits_budget, b"01234567890123").unwrap();
        pkt.pts = Timestamp::new(160);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();

        let out = writes.lock().unwrap();
        assert_eq!(out.len(), 1);
        let parsed = vaco_format_rtp::RtpPacket::parse(&out[0]).unwrap();
        assert_eq!(parsed.header.payload_type, 0); // PCMU static PT
        assert_eq!(parsed.payload, b"01234567890123");
        assert!(parsed.header.marker);
    }

    #[test]
    fn rejects_a_second_stream() {
        let sink = RecordingSink::default();
        let mut mux = RtpMuxer::new(Box::new(sink));
        let params = CodecParameters::audio().with_codec(vaco_codec_core::CodecId::PcmMulaw);
        mux.add_stream(&params).unwrap();
        assert!(mux.add_stream(&params).is_err());
    }
}
