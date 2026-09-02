//! The RIST-specific RTCP messages, `VSF TR-06-1:2020` §5.2.6/§5.3.2,
//! quoted/paraphrased from the fetched document. Plain SR/RR/SDES/BYE need
//! no new types — they are `vaco_rtp::rtcp::RtcpPacket` used with the field
//! *values* §5.2.2-§5.2.5 pin down (`RC=0`, exactly one report block, one
//! CNAME chunk); only the messages below have a shape RFC 3550 does not
//! already define.
//!
//! Every message here rides inside an `RtcpPacket::Other { payload_type,
//! count_or_fmt, data }` as parsed by `vaco_rtp::rtcp::parse_one`/
//! `iter_compound` — `data` is everything after the 4-byte common header,
//! `count_or_fmt` is that header's reused 5-bit field (`APP`'s `Subtype`,
//! or the feedback `FMT`).
//!
//! # `RIST` name field
//!
//! §5.2.6 and §5.3.2.2 both carry a 4-byte ASCII `"RIST"` field
//! identifying the application. `0x52495354` is that string's big-endian
//! byte value (`R`=0x52, `I`=0x49, `S`=0x53, `T`=0x54), stated in the
//! spec's own worked example (Appendix A) as well as its prose.

use vaco_core::{Error, Result};

/// `"RIST"`, big-endian, §5.2.6/§5.3.2.2's `Name (ASCII)` field.
pub const RIST_NAME: u32 = 0x5249_5354;

/// RTCP payload type 204, `APP` (RFC 3550 §6.7) — both the RTT-echo
/// message and the range-based NACK ride on this payload type, told apart
/// by `Subtype`.
pub const PT_APP: u8 = 204;

/// RTCP payload type 205, Transport-Layer Feedback (RFC 4585 §6.2) — the
/// bitmask-based (Generic) NACK's payload type.
pub const PT_TRANSPORT_FB: u8 = 205;

/// §5.2.6's `APP` `Subtype` for an RTT Echo Request.
pub const RTT_ECHO_REQUEST_SUBTYPE: u8 = 2;
/// §5.2.6's `APP` `Subtype` for an RTT Echo Response.
pub const RTT_ECHO_RESPONSE_SUBTYPE: u8 = 3;
/// §5.3.2.2's `APP` `Subtype` for a range-based retransmission request.
pub const RANGE_NACK_SUBTYPE: u8 = 0;
/// RFC 4585 §6.2.1's Generic NACK `FMT` value, reused unchanged by
/// §5.3.2.1.
pub const GENERIC_NACK_FMT: u8 = 1;

fn malformed(detail: &'static str) -> Error {
    Error::InvalidData(detail)
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32> {
    let s = buf
        .get(at..at + 4)
        .ok_or_else(|| malformed("RIST RTCP field runs past the buffer"))?;
    let arr: [u8; 4] = s
        .try_into()
        .map_err(|_| malformed("RIST RTCP field is not 4 bytes"))?;
    Ok(u32::from_be_bytes(arr))
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16> {
    let s = buf
        .get(at..at + 2)
        .ok_or_else(|| malformed("RIST RTCP field runs past the buffer"))?;
    let arr: [u8; 2] = s
        .try_into()
        .map_err(|_| malformed("RIST RTCP field is not 2 bytes"))?;
    Ok(u16::from_be_bytes(arr))
}

/// §5.2.6's RTT Echo Request/Response `APP` message.
///
/// The wrapped-key-shaped opacity SRT's `km` module uses does not apply
/// here — every field's meaning is fixed by the spec, nothing is deferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RttEcho {
    pub is_response: bool,
    pub ssrc_media_source: u32,
    /// Arbitrary on request; echoed back verbatim on response.
    pub timestamp: u64,
    /// Zero on request (message sender fills zero, receiver ignores it);
    /// the responder's own processing delay in microseconds on response.
    pub processing_delay_us: u32,
    /// Optional padding so the RTT measurement covers a packet close to
    /// stream-packet size; content is arbitrary, length a multiple of 4.
    pub padding: Vec<u8>,
}

impl RttEcho {
    /// # Errors
    /// [`Error::InvalidData`] if `count_or_fmt` is not the request or
    /// response subtype, the name field is not `"RIST"`, or `data` is
    /// shorter than the fixed 20-byte body.
    pub fn parse(count_or_fmt: u8, data: &[u8]) -> Result<Self> {
        let is_response = match count_or_fmt {
            RTT_ECHO_REQUEST_SUBTYPE => false,
            RTT_ECHO_RESPONSE_SUBTYPE => true,
            _ => {
                return Err(malformed(
                    "RTT echo subtype is neither request (2) nor response (3)",
                ));
            }
        };
        let ssrc_media_source = u32_at(data, 0)?;
        let name = u32_at(data, 4)?;
        if name != RIST_NAME {
            return Err(malformed("RTT echo name field is not \"RIST\""));
        }
        let timestamp = (u64::from(u32_at(data, 8)?) << 32) | u64::from(u32_at(data, 12)?);
        let processing_delay_us = u32_at(data, 16)?;
        let padding = data.get(20..).unwrap_or(&[]).to_vec();
        Ok(Self {
            is_response,
            ssrc_media_source,
            timestamp,
            processing_delay_us,
            padding,
        })
    }

    /// Returns `(count_or_fmt, data)` — the caller wraps `data` in the
    /// common 4-byte RTCP header (`PT` = [`PT_APP`]) itself, matching how
    /// `vaco_rtp::rtcp::parse_one` hands unrecognised payload types back.
    #[must_use]
    pub fn serialize(&self) -> (u8, Vec<u8>) {
        let count_or_fmt = if self.is_response {
            RTT_ECHO_RESPONSE_SUBTYPE
        } else {
            RTT_ECHO_REQUEST_SUBTYPE
        };
        let mut data = Vec::new();
        data.extend_from_slice(&self.ssrc_media_source.to_be_bytes());
        data.extend_from_slice(&RIST_NAME.to_be_bytes());
        data.extend_from_slice(&((self.timestamp >> 32) as u32).to_be_bytes());
        data.extend_from_slice(&(self.timestamp as u32).to_be_bytes());
        data.extend_from_slice(&self.processing_delay_us.to_be_bytes());
        data.extend_from_slice(&self.padding);
        (count_or_fmt, data)
    }
}

/// One Generic NACK FCI entry, RFC 4585 §6.2.1: a lost packet's sequence
/// number (`pid`) plus a bitmask of up to 16 further, possibly-lost
/// packets immediately following it (`blp`, bit `i` set means `pid + i`,
/// `i` counted from 1, is also lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NackEntry {
    pub pid: u16,
    pub blp: u16,
}

/// §5.3.2.1's bitmask-based retransmission request: RFC 4585's Generic
/// NACK, `FMT` = [`GENERIC_NACK_FMT`], `PT` = [`PT_TRANSPORT_FB`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericNack {
    /// Ignored by a RIST sender per the spec's own footnote — RFC 3550
    /// calls this the packet sender's SSRC, kept only because the wire
    /// format has the field.
    pub ssrc_packet_sender: u32,
    pub ssrc_media_source: u32,
    pub entries: Vec<NackEntry>,
}

impl GenericNack {
    /// # Errors
    /// [`Error::InvalidData`] if `count_or_fmt` is not [`GENERIC_NACK_FMT`]
    /// or `data` is shorter than the fixed 8-byte header plus a whole
    /// number of 4-byte FCI entries.
    pub fn parse(count_or_fmt: u8, data: &[u8]) -> Result<Self> {
        if count_or_fmt != GENERIC_NACK_FMT {
            return Err(malformed("Generic NACK FMT is not 1"));
        }
        let ssrc_packet_sender = u32_at(data, 0)?;
        let ssrc_media_source = u32_at(data, 4)?;
        let fci = data.get(8..).unwrap_or(&[]);
        if !fci.len().is_multiple_of(4) {
            return Err(malformed(
                "Generic NACK FCI is not a whole number of 4-byte entries",
            ));
        }
        let mut entries = Vec::new();
        let mut pos = 0usize;
        while pos < fci.len() {
            let pid = u16_at(fci, pos)?;
            let blp = u16_at(fci, pos + 2)?;
            entries.push(NackEntry { pid, blp });
            pos += 4;
        }
        Ok(Self {
            ssrc_packet_sender,
            ssrc_media_source,
            entries,
        })
    }

    /// Returns `(count_or_fmt, data)` — see [`RttEcho::serialize`] for why.
    #[must_use]
    pub fn serialize(&self) -> (u8, Vec<u8>) {
        let mut data = Vec::new();
        data.extend_from_slice(&self.ssrc_packet_sender.to_be_bytes());
        data.extend_from_slice(&self.ssrc_media_source.to_be_bytes());
        for entry in &self.entries {
            data.extend_from_slice(&entry.pid.to_be_bytes());
            data.extend_from_slice(&entry.blp.to_be_bytes());
        }
        (GENERIC_NACK_FMT, data)
    }
}

/// One range-request FCI entry, §5.3.2.2: `start` is the first lost
/// packet's sequence number, `additional` is how many further consecutive
/// packets after it are also being requested (so `additional = 0`
/// requests exactly one packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeEntry {
    pub start: u16,
    pub additional: u16,
}

/// §5.3.2.2's range-based retransmission request: a RIST-specific `APP`
/// message, `Subtype` = [`RANGE_NACK_SUBTYPE`], `PT` = [`PT_APP`]. The
/// spec calls this an interim measure pending an IANA-allocated
/// Transport-Layer Feedback `FMT`, "in order to expedite the initial
/// implementations of the protocol" — quoted because it is the reason
/// this shares `APP`'s payload type with [`RttEcho`] rather than
/// [`GenericNack`]'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeNack {
    pub ssrc_media_source: u32,
    pub ranges: Vec<RangeEntry>,
}

impl RangeNack {
    /// # Errors
    /// [`Error::InvalidData`] if `count_or_fmt` is not
    /// [`RANGE_NACK_SUBTYPE`], the name field is not `"RIST"`, or `data`
    /// is shorter than the fixed 8-byte header plus a whole number of
    /// 4-byte range entries.
    pub fn parse(count_or_fmt: u8, data: &[u8]) -> Result<Self> {
        if count_or_fmt != RANGE_NACK_SUBTYPE {
            return Err(malformed("range NACK subtype is not 0"));
        }
        let ssrc_media_source = u32_at(data, 0)?;
        let name = u32_at(data, 4)?;
        if name != RIST_NAME {
            return Err(malformed("range NACK name field is not \"RIST\""));
        }
        let ranges_buf = data.get(8..).unwrap_or(&[]);
        if !ranges_buf.len().is_multiple_of(4) {
            return Err(malformed(
                "range NACK ranges are not a whole number of 4-byte entries",
            ));
        }
        let mut ranges = Vec::new();
        let mut pos = 0usize;
        while pos < ranges_buf.len() {
            let start = u16_at(ranges_buf, pos)?;
            let additional = u16_at(ranges_buf, pos + 2)?;
            ranges.push(RangeEntry { start, additional });
            pos += 4;
        }
        Ok(Self {
            ssrc_media_source,
            ranges,
        })
    }

    /// Returns `(count_or_fmt, data)` — see [`RttEcho::serialize`] for why.
    #[must_use]
    pub fn serialize(&self) -> (u8, Vec<u8>) {
        let mut data = Vec::new();
        data.extend_from_slice(&self.ssrc_media_source.to_be_bytes());
        data.extend_from_slice(&RIST_NAME.to_be_bytes());
        for range in &self.ranges {
            data.extend_from_slice(&range.start.to_be_bytes());
            data.extend_from_slice(&range.additional.to_be_bytes());
        }
        (RANGE_NACK_SUBTYPE, data)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    // --- draft-derived: TR-06-1 §5.2.6 Figure, field-by-field ---

    #[test]
    fn rtt_echo_request_round_trips() {
        let echo = RttEcho {
            is_response: false,
            ssrc_media_source: 0xAABB_CC00,
            timestamp: 0x1122_3344_5566_7788,
            processing_delay_us: 0, // spec: request sender fills zero
            padding: vec![],
        };
        let (count_or_fmt, data) = echo.serialize();
        assert_eq!(count_or_fmt, RTT_ECHO_REQUEST_SUBTYPE);
        let parsed = RttEcho::parse(count_or_fmt, &data).unwrap();
        assert_eq!(parsed, echo);
    }

    #[test]
    fn rtt_echo_response_carries_processing_delay_and_padding() {
        let echo = RttEcho {
            is_response: true,
            ssrc_media_source: 42,
            timestamp: 0,
            processing_delay_us: 1500,
            padding: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let (count_or_fmt, data) = echo.serialize();
        assert_eq!(count_or_fmt, RTT_ECHO_RESPONSE_SUBTYPE);
        // 20-byte fixed body + 4 padding bytes.
        assert_eq!(data.len(), 24);
        let parsed = RttEcho::parse(count_or_fmt, &data).unwrap();
        assert_eq!(parsed, echo);
    }

    #[test]
    fn rtt_echo_rejects_wrong_name() {
        let mut data = vec![0u8; 20];
        data[4..8].copy_from_slice(&0x4141_4141u32.to_be_bytes()); // not "RIST"
        assert!(RttEcho::parse(RTT_ECHO_REQUEST_SUBTYPE, &data).is_err());
    }

    #[test]
    fn rtt_echo_rejects_bad_subtype() {
        let data = vec![0u8; 20];
        assert!(RttEcho::parse(1, &data).is_err());
    }

    // --- draft-derived: Appendix A's worked bitmask-NACK scenario, but
    // the expected PID/BLP values are independently computed from the
    // §5.3.2.1 prose rule ("bit i set iff PID+i, i from 1, is not
    // received"), not transcribed from the appendix's rendered figure —
    // see rtcp.rs's module docs and the crate-level note on why a
    // low-resolution scan of a bitmask is the wrong thing to trust
    // byte-for-byte. The *scenario* (packet 99 received, 100 lost, 101-102
    // received, 103-122 lost, 123+ received) and the two-entries-of-16
    // shape are draft-derived; the specific bit pattern below is this
    // crate's own application of the stated rule to that scenario.

    fn expected_bitmask_appendix_a_scenario() -> Vec<NackEntry> {
        // Lost: exactly {100} ∪ {103..=122}.
        let lost = |seq: u32| seq == 100 || (103..=122).contains(&seq);
        let mut entries = Vec::new();
        let mut next_pid: Option<u16> = Some(100);
        while let Some(pid) = next_pid {
            let mut blp = 0u16;
            for i in 1..=16u32 {
                if lost(u32::from(pid) + i) {
                    blp |= 1 << (i - 1);
                }
            }
            entries.push(NackEntry { pid, blp });
            // Find the next lost packet strictly after this entry's own
            // 17-packet span (pid, pid+1, .., pid+16) that this entry did
            // not already cover, mirroring how a real sender would keep
            // issuing entries until every lost packet is named by some
            // entry's PID or BLP bit.
            let span_end = u32::from(pid) + 16;
            next_pid = ((span_end + 1)..=122).find(|&s| lost(s)).map(|s| s as u16);
        }
        entries
    }

    #[test]
    fn generic_nack_matches_appendix_a_scenario_rule() {
        let entries = expected_bitmask_appendix_a_scenario();
        // Appendix A names exactly two entries, PID=100 and PID=117.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pid, 100);
        assert_eq!(entries[1].pid, 117);
        // PID=100: packets 101,102 received (bits 1,2 clear), 103..116
        // lost (bits 3..16 set) -> 0b1111_1111_1111_1100.
        assert_eq!(entries[0].blp, 0b1111_1111_1111_1100);
        // PID=117: packets 118..122 lost (bits 1..5 set), 123.. received
        // (bits 6..16 clear) -> 0b0000_0000_0001_1111.
        assert_eq!(entries[1].blp, 0b0000_0000_0001_1111);

        let nack = GenericNack {
            ssrc_packet_sender: 0,
            ssrc_media_source: 0xAABB_CC00,
            entries,
        };
        let (count_or_fmt, data) = nack.serialize();
        let parsed = GenericNack::parse(count_or_fmt, &data).unwrap();
        assert_eq!(parsed, nack);
    }

    #[test]
    fn generic_nack_rejects_wrong_fmt() {
        let data = vec![0u8; 8];
        assert!(GenericNack::parse(2, &data).is_err());
    }

    #[test]
    fn generic_nack_rejects_truncated_fci() {
        let mut data = vec![0u8; 8];
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // 3 bytes, not 4
        assert!(GenericNack::parse(GENERIC_NACK_FMT, &data).is_err());
    }

    // --- draft-derived: TR-06-1 Appendix A's own range-request example
    // (Start=100/Additional=0, Start=103/Additional=19), matching the
    // same 100+103..122 scenario ---

    #[test]
    fn range_nack_matches_appendix_a_example() {
        let nack = RangeNack {
            ssrc_media_source: 0xAABB_CC00,
            ranges: vec![
                RangeEntry {
                    start: 100,
                    additional: 0,
                },
                RangeEntry {
                    start: 103,
                    additional: 19,
                },
            ],
        };
        let (count_or_fmt, data) = nack.serialize();
        assert_eq!(count_or_fmt, RANGE_NACK_SUBTYPE);
        let parsed = RangeNack::parse(count_or_fmt, &data).unwrap();
        assert_eq!(parsed, nack);
        // Additional=19 alongside Start=103 requests 103..=122 inclusive
        // (20 packets) -- exactly Appendix A's stated loss run.
        let last_requested = u32::from(nack.ranges[1].start) + u32::from(nack.ranges[1].additional);
        assert_eq!(last_requested, 122);
    }

    #[test]
    fn range_nack_rejects_wrong_name() {
        let mut data = vec![0u8; 8];
        data[4..8].copy_from_slice(&0x4141_4141u32.to_be_bytes());
        assert!(RangeNack::parse(RANGE_NACK_SUBTYPE, &data).is_err());
    }

    // --- self-consistency: round trip through vaco_rtp's own compound
    // iterator, proving these types integrate with the crate they build
    // on rather than only with their own serialize()/parse() pair ---

    #[test]
    fn rtt_echo_round_trips_through_vaco_rtp_compound_packet() {
        let echo = RttEcho {
            is_response: false,
            ssrc_media_source: 7,
            timestamp: 99,
            processing_delay_us: 0,
            padding: vec![],
        };
        let (count_or_fmt, body) = echo.serialize();
        let mut packet = Vec::new();
        #[allow(
            clippy::integer_division,
            reason = "RTCP length is defined in 32-bit words; body.len() is a multiple of 4 by construction here"
        )]
        let length_words = u16::try_from((4 + body.len()) / 4 - 1).unwrap();
        packet.push((2 << 6) | count_or_fmt); // V=2, P=0, Subtype
        packet.push(PT_APP);
        packet.extend_from_slice(&length_words.to_be_bytes());
        packet.extend_from_slice(&body);

        let parsed: Vec<_> = vaco_rtp::rtcp::iter_compound(&packet)
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            vaco_rtp::rtcp::RtcpPacket::Other {
                payload_type,
                count_or_fmt: cof,
                data,
            } => {
                assert_eq!(*payload_type, PT_APP);
                let round_tripped = RttEcho::parse(*cof, data).unwrap();
                assert_eq!(round_tripped, echo);
            }
            other => unreachable!("expected Other, got {other:?}"),
        }
    }
}
