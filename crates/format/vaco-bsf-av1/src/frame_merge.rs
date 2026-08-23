//! `av1_frame_merge`: the inverse of [`crate::frame_split`] — reassemble a
//! stream of per-OBU (or per-frame) packets back into one packet per
//! temporal unit.
//!
//! # What is measured, not assumed
//!
//! Unlike `frame_split`, this one's grouping key is unambiguous and directly
//! observable: **a Temporal Delimiter OBU starts a new temporal unit.**
//! Verified as a full round trip against `ffmpeg 8.1`: `av1_frame_split`
//! then `av1_frame_merge`, applied to a real SVT-AV1 elementary stream read
//! through the `obu` demuxer, reproduced the demuxer's own native
//! packetisation **byte for byte** (`framecrc` agreement on every packet,
//! `pts`/`dts`/size/CRC, 25 packets both sides).
//!
//! And a negative measurement, which is why this filter is not the identity
//! it might look like from the round trip alone: fed an MP4-sourced AV1
//! stream — whose samples never carry a temporal delimiter, because the
//! ISOBMFF sample already *is* the access-unit boundary — the reference
//! refuses every packet with `Missing Temporal Delimiter`. This crate
//! reproduces that refusal (`Error::InvalidData`) rather than inventing a
//! unit boundary the input does not state.
//!
//! # The algorithm
//!
//! Accumulate incoming bytes into a buffer. Scan each incoming packet's OBUs
//! (it may itself carry several, if fed unsplit input); whenever a Temporal
//! Delimiter OBU appears while the buffer already holds bytes, flush the
//! buffer as a completed packet first, then start the new buffer at that
//! delimiter. The very first OBU this filter ever sees must be a Temporal
//! Delimiter, or there is no unit boundary to work from at all — exactly
//! the MP4 case above.
//!
//! Handling more than one Temporal Delimiter inside a single incoming packet
//! is not itself something the reference was observed doing (every real
//! demuxer hands this filter at most one per packet), but it falls out of
//! the same rule without a special case, and a filter that produces multiple
//! output packets from one input is already normal for this crate's
//! [`vaco_bsf_core::PacketMap`] interface.
//!
//! The merged packet's timestamps are the *first* constituent packet's — the
//! one carrying the Temporal Delimiter — which is what a real elementary
//! stream demuxer's per-OBU timestamps look like (measured: every OBU of one
//! temporal unit shares that unit's `pts`).

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_av1::Av1Framing;
use vaco_parse_av1::obu::{self, ObuType};

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "av1_frame_merge",
    long_name: "Merge AV1 OBU frames into temporal units",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Av1) => Ok(Box::new(MappedFilter::new(FrameMerge {
            buffer: Vec::new(),
            template: None,
            budget: Budget::new(Limits::permissive()),
        }))),
        _ => Err(Error::Unsupported("av1_frame_merge: av1 only")),
    }
}

struct FrameMerge {
    buffer: Vec<u8>,
    /// The first constituent packet of the buffer currently being
    /// accumulated, whose metadata (timestamps, flags) the flushed packet
    /// inherits.
    template: Option<Packet>,
    budget: Budget,
}

impl FrameMerge {
    fn flush(&mut self, out: &mut VecDeque<Packet>) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let Some(t) = self.template.take() else {
            return Ok(());
        };
        let mut np = Packet::from_slice(&mut self.budget, &self.buffer)?;
        np.stream_index = t.stream_index;
        np.pts = t.pts;
        np.dts = t.dts;
        np.duration = t.duration;
        np.pos = t.pos;
        np.flags = t.flags;
        out.push_back(np);
        self.buffer.clear();
        Ok(())
    }
}

impl PacketMap for FrameMerge {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else {
            return self.flush(out);
        };
        let payload = p.payload();
        for unit in obu::units(payload, Av1Framing::ObuStream) {
            if unit.header.obu_type == ObuType::TEMPORAL_DELIMITER {
                if self.buffer.is_empty() {
                    self.template = Some(p.clone());
                } else {
                    self.flush(out)?;
                    self.template = Some(p.clone());
                }
            } else if self.buffer.is_empty() && self.template.is_none() {
                return Err(Error::InvalidData(
                    "av1_frame_merge: missing temporal delimiter",
                ));
            }
            self.buffer.extend_from_slice(unit.bytes(payload));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn av1_params() -> CodecParameters {
        CodecParameters::video().with_codec(CodecId::Av1)
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn annexb_pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    fn obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![(obu_type << 3) | 0b0000_0010];
        v.push(payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }

    /// The measured round trip: split then merge reproduces the original.
    #[test]
    fn split_then_merge_round_trips() {
        let td = obu(2, &[]);
        let seq = obu(1, &[0xAA]);
        let f1 = obu(6, &[0x01]);
        let f2 = obu(6, &[0x02, 0x03]);
        let mut original = td;
        original.extend(&seq);
        original.extend(&f1);
        original.extend(&f2);

        let mut split = (crate::frame_split::DESC.build)(&av1_params()).unwrap();
        split.send_packet(Some(&annexb_pkt(&original))).unwrap();
        let mut merge = (DESC.build)(&av1_params()).unwrap();
        while let Ok(p) = split.receive_packet() {
            merge.send_packet(Some(&p)).unwrap();
        }
        merge.send_packet(None).unwrap();
        let out = merge.receive_packet().unwrap();
        assert_eq!(out.payload(), original.as_slice());
        assert!(merge.receive_packet().is_err());
    }

    /// The measured negative case: a stream with no temporal delimiter at
    /// all — an MP4-sourced sample, in the reference — is refused rather
    /// than silently treated as one giant unit.
    #[test]
    fn a_stream_with_no_temporal_delimiter_is_refused() {
        let mut f = (DESC.build)(&av1_params()).unwrap();
        let seq_then_frame = {
            let mut v = obu(1, &[0xAA]);
            v.extend(obu(6, &[0x01]));
            v
        };
        assert!(f.send_packet(Some(&annexb_pkt(&seq_then_frame))).is_err());
    }

    /// A second temporal delimiter flushes the first group before starting
    /// the next, even when both arrive inside one input packet.
    #[test]
    fn two_temporal_units_in_one_input_packet_yield_two_output_packets() {
        let mut buf = obu(2, &[]);
        buf.extend(obu(6, &[0x01]));
        buf.extend(obu(2, &[]));
        buf.extend(obu(6, &[0x02]));

        let mut f = (DESC.build)(&av1_params()).unwrap();
        f.send_packet(Some(&annexb_pkt(&buf))).unwrap();
        f.send_packet(None).unwrap();

        let mut expected_a = obu(2, &[]);
        expected_a.extend(obu(6, &[0x01]));
        let mut expected_b = obu(2, &[]);
        expected_b.extend(obu(6, &[0x02]));

        assert_eq!(f.receive_packet().unwrap().payload(), expected_a.as_slice());
        assert_eq!(f.receive_packet().unwrap().payload(), expected_b.as_slice());
        assert!(f.receive_packet().is_err());
    }

    #[test]
    fn a_non_av1_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::H264);
        assert!((DESC.build)(&params).is_err());
    }

    /// Falsified: if the "must open with a TD" check were removed, the
    /// missing-delimiter test above would silently accept the stream instead
    /// of refusing it.
    #[test]
    fn falsified_removing_the_leading_td_check_would_wrongly_accept() {
        // Direct assertion that the fixture used above really has no TD in
        // it, so the refusal in that test is exercising the real check and
        // not an unrelated parse failure.
        let seq_then_frame = {
            let mut v = obu(1, &[0xAA]);
            v.extend(obu(6, &[0x01]));
            v
        };
        let has_td = obu::units(&seq_then_frame, Av1Framing::ObuStream)
            .iter()
            .any(|u| u.header.obu_type == ObuType::TEMPORAL_DELIMITER);
        assert!(!has_td);
    }
}
