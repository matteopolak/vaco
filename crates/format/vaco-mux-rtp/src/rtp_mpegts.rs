//! `rtp_mpegts`: RFC 2250 §2's `MP2T` RTP framing over already-muxed MPEG-2
//! TS packets.
//!
//! See the crate's top-level docs for why this does **not** run a nested
//! MPEG-TS muxer the way `ffmpeg -f rtp_mpegts` does: `vaco_format_core::Muxer`
//! has no seam for one muxer to own another, so [`RtpMpegtsMuxer`] instead
//! expects its stream's packets to already be complete, `188`-byte-aligned
//! TS output — a caller runs `vaco-mux-mpegts` (or any other MPEG-TS muxer)
//! itself and feeds this muxer its output bytes as this muxer's one
//! stream's packet payloads.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Result};
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// Static payload type 33 (`MP2T`, RFC 3551).
const MP2T_PAYLOAD_TYPE: u8 = 33;
const MP2T_CLOCK_RATE: u32 = 90_000;
const TS_PACKET_LEN: usize = 188;

/// Split a run of `188`-byte-aligned MPEG-TS packets into RTP payloads,
/// each holding as many whole TS packets as fit under `mtu` — RFC 2250 §2
/// requires RTP/MP2T payloads to hold a whole number of TS packets, never a
/// partial one.
///
/// # Errors
/// [`Error::InvalidData`] if `ts_bytes.len()` is not a multiple of 188.
pub fn pack(ts_bytes: &[u8], mtu: usize) -> Result<Vec<Vec<u8>>> {
    if !ts_bytes.len().is_multiple_of(TS_PACKET_LEN) {
        return Err(Error::InvalidData(
            "rtp_mpegts input is not a whole number of 188-byte TS packets",
        ));
    }
    #[allow(
        clippy::integer_division,
        reason = "counting whole TS packets that fit; remainder is intentionally discarded"
    )]
    let packets_per_payload = (mtu / TS_PACKET_LEN).max(1);
    let chunk_len = packets_per_payload * TS_PACKET_LEN;
    Ok(ts_bytes.chunks(chunk_len).map(<[u8]>::to_vec).collect())
}

/// The `rtp_mpegts` muxer — see the module docs for the scope decision.
pub struct RtpMpegtsMuxer {
    sink: Box<dyn MediaSink>,
    ssrc: u32,
    sequence_number: u16,
    mtu: usize,
    have_stream: bool,
}

impl std::fmt::Debug for RtpMpegtsMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtpMpegtsMuxer").finish_non_exhaustive()
    }
}

impl RtpMpegtsMuxer {
    #[must_use]
    pub fn new(sink: Box<dyn MediaSink>) -> Self {
        Self {
            sink,
            ssrc: 0x5254_5054, // "RTPT" - a fixed, documented default; see `crate::muxer` for why no RNG is used
            sequence_number: 0,
            mtu: crate::muxer::DEFAULT_MTU,
            have_stream: false,
        }
    }
}

impl Muxer for RtpMpegtsMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::TS_DISCONT
    }

    fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
        if self.have_stream {
            return Err(Error::Unsupported(
                "the rtp_mpegts muxer carries exactly one stream",
            ));
        }
        self.have_stream = true;
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
        let payloads = pack(packet.payload(), self.mtu)?;
        let Some(pts) = packet.pts.ticks() else {
            return Err(Error::InvalidData(
                "rtp_mpegts packets need a pts to derive an RTP timestamp",
            ));
        };
        let rtp_timestamp = u32::try_from(pts & 0xFFFF_FFFF).unwrap_or(0);
        for payload in &payloads {
            let header = vaco_format_rtp::RtpHeader {
                version: vaco_format_rtp::RTP_VERSION,
                padding: false,
                extension: false,
                marker: false, // RFC 2250 §2: the marker bit is not used for MP2T
                payload_type: MP2T_PAYLOAD_TYPE,
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

    fn stream_time_base(&self, stream_index: u32) -> Option<vaco_core::Rational> {
        (stream_index == 0)
            .then(|| vaco_core::Rational::new(1, i32::try_from(MP2T_CLOCK_RATE).unwrap_or(90_000)))
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match MuxerDesc::open's fn-pointer signature exactly"
)]
fn open_rtp_mpegts_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(RtpMpegtsMuxer::new(sink)))
}

pub const MUXER_RTP_MPEGTS: MuxerDesc = MuxerDesc {
    name: "rtp_mpegts",
    long_name: "RTP/mpegts output format",
    extensions: &[],
    default_video: Some(vaco_codec_core::CodecId::Mpeg4),
    default_audio: Some(vaco_codec_core::CodecId::Aac),
    open: open_rtp_mpegts_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn packs_whole_ts_packets_per_payload() {
        let ts = vec![0x47u8; TS_PACKET_LEN * 5];
        let out = pack(&ts, 400).unwrap(); // 400 / 188 = 2 packets per payload
        assert_eq!(out[0].len(), TS_PACKET_LEN * 2);
        assert_eq!(out.iter().map(Vec::len).sum::<usize>(), ts.len());
    }

    #[test]
    fn rejects_misaligned_input() {
        assert!(pack(&[0u8; 100], 400).is_err());
    }

    proptest::proptest! {
        #[test]
        fn pack_never_panics(n_packets in 0usize..20, mtu in 1usize..2000) {
            let ts = vec![0x47u8; TS_PACKET_LEN * n_packets];
            let _ = pack(&ts, mtu);
        }
    }
}
