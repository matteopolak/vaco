//! RFC 3550 §6 RTCP: sender/receiver reports, source descriptions, `BYE`.
//!
//! RTCP packets are always sent as a **compound packet** (§6.1): one or more
//! individual packets back-to-back in a single UDP datagram (or interleaved
//! TCP frame), each carrying its own length so the next one can be found.
//! [`iter_compound`] is the entry point; it never trusts a packet's declared
//! length past what the buffer actually holds.
//!
//! `vaco-demux-rtsp`'s keepalive and quality reporting need [`build_rr`] (a
//! receiver report — this crate never sends media, so it never needs
//! [`build_sr`]'s sender-side fields for its own traffic, but a full sender
//! report *builder* is included for `vaco-mux-rtp`, which does). Both use
//! `vaco-time` for the NTP timestamp — never `std::time` — because a sender
//! report is exactly the "encode wall-clock time" case D18 exists for.

use vaco_core::{Error, Result};

const HEADER_LEN: usize = 4;
const REPORT_BLOCK_LEN: usize = 24;
/// `REPORT_BLOCK_LEN` in 32-bit words — a compile-time constant, spelled
/// without `/` so `clippy::integer_division` (deny, workspace-wide) has
/// nothing to flag.
const REPORT_BLOCK_LEN_WORDS: usize = 6;

/// One report block, RFC 3550 §6.4.1 (identical layout in SR and RR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportBlock {
    pub ssrc: u32,
    pub fraction_lost: u8,
    /// Signed 24-bit cumulative count, sign-extended into an `i32`.
    pub cumulative_lost: i32,
    pub extended_highest_seq: u32,
    pub jitter: u32,
    /// Last SR timestamp (middle 32 bits of the NTP timestamp), 0 if none received.
    pub last_sr: u32,
    /// Delay since last SR, in units of 1/65536 seconds, 0 if `last_sr` is 0.
    pub delay_since_last_sr: u32,
}

/// RFC 3550 §6.4.1 sender info, the fixed part of a Sender Report ahead of
/// its report blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderInfo {
    /// Full 64-bit NTP timestamp.
    pub ntp: u64,
    pub rtp_timestamp: u32,
    pub packet_count: u32,
    pub octet_count: u32,
}

/// One SDES item, RFC 3550 §6.5. `kind` 1 = CNAME, the only one RTSP's
/// keepalive path needs to emit; others are parsed and reported verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdesItem {
    pub kind: u8,
    pub text: Vec<u8>,
}

/// One parsed RTCP packet from a compound packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcpPacket {
    SenderReport {
        ssrc: u32,
        info: SenderInfo,
        reports: Vec<ReportBlock>,
    },
    ReceiverReport {
        ssrc: u32,
        reports: Vec<ReportBlock>,
    },
    SourceDescription(Vec<(u32, Vec<SdesItem>)>),
    Bye {
        sources: Vec<u32>,
        reason: Option<Vec<u8>>,
    },
    /// RFC 3550 §6.7 `APP`, or any payload type this module does not
    /// interpret. `payload_type` is the raw wire value so a caller can tell
    /// an `APP` (204) from something genuinely unrecognised.
    Other {
        payload_type: u8,
        data: Vec<u8>,
    },
}

fn get(buf: &[u8], range: std::ops::Range<usize>) -> Result<&[u8]> {
    buf.get(range)
        .ok_or(Error::InvalidData("RTCP packet runs past the buffer"))
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32> {
    let s = get(buf, at..at + 4)?;
    let arr: [u8; 4] = s
        .try_into()
        .map_err(|_| Error::InvalidData("RTCP field runs past the buffer"))?;
    Ok(u32::from_be_bytes(arr))
}

fn parse_report_blocks(buf: &[u8], count: u8) -> Result<Vec<ReportBlock>> {
    let mut out = Vec::new();
    for i in 0..usize::from(count) {
        let base = i
            .checked_mul(REPORT_BLOCK_LEN)
            .ok_or(Error::InvalidData("RTCP report block count overflows"))?;
        let ssrc = u32_at(buf, base)?;
        let word = u32_at(buf, base + 4)?;
        let fraction_lost = (word >> 24) as u8;
        let raw24 = word & 0x00FF_FFFF;
        // Sign-extend a 24-bit two's complement value.
        let cumulative_lost = if raw24 & 0x0080_0000 != 0 {
            (raw24 | 0xFF00_0000).cast_signed()
        } else {
            raw24.cast_signed()
        };
        let extended_highest_seq = u32_at(buf, base + 8)?;
        let jitter = u32_at(buf, base + 12)?;
        let last_sr = u32_at(buf, base + 16)?;
        let delay_since_last_sr = u32_at(buf, base + 20)?;
        out.push(ReportBlock {
            ssrc,
            fraction_lost,
            cumulative_lost,
            extended_highest_seq,
            jitter,
            last_sr,
            delay_since_last_sr,
        });
    }
    Ok(out)
}

fn parse_sdes_chunk(buf: &[u8]) -> Result<(u32, Vec<SdesItem>, usize)> {
    let ssrc = u32_at(buf, 0)?;
    let mut pos = 4usize;
    let mut items = Vec::new();
    loop {
        let kind = *buf
            .get(pos)
            .ok_or(Error::InvalidData("SDES chunk runs past the buffer"))?;
        if kind == 0 {
            pos += 1;
            break;
        }
        let len = usize::from(
            *buf.get(pos + 1)
                .ok_or(Error::InvalidData("SDES item has no length byte"))?,
        );
        let text = get(buf, pos + 2..pos + 2 + len)?.to_vec();
        items.push(SdesItem { kind, text });
        pos += 2 + len;
    }
    // Chunks are padded to a 32-bit boundary.
    let padded = pos.div_ceil(4) * 4;
    Ok((ssrc, items, padded))
}

/// Parse one RTCP packet starting at `buf[0]`, per the header in RFC 3550
/// §6.1, returning it together with the byte length actually consumed
/// (header + `length` field's 32-bit words, exactly as the wire states).
///
/// # Errors
/// [`Error::InvalidData`] if the header, its declared length, or any nested
/// field (report block, SDES item, `BYE` reason) runs past `buf`.
pub fn parse_one(buf: &[u8]) -> Result<(RtcpPacket, usize)> {
    if buf.len() < HEADER_LEN {
        return Err(Error::InvalidData("RTCP header shorter than 4 bytes"));
    }
    let head: [u8; 4] = get(buf, 0..4)?
        .try_into()
        .map_err(|_| Error::InvalidData("RTCP header runs past the buffer"))?;
    let b0 = head[0];
    let version = b0 >> 6;
    if version != 2 {
        return Err(Error::InvalidData("RTCP header version is not 2"));
    }
    let count = b0 & 0x1F;
    let payload_type = head[1];
    let length_words = u16::from_be_bytes([head[2], head[3]]);
    let total_len = (usize::from(length_words) + 1)
        .checked_mul(4)
        .ok_or(Error::InvalidData("RTCP length overflows"))?;
    let body = get(buf, HEADER_LEN..total_len)?;

    let packet = match payload_type {
        200 => {
            let ssrc = u32_at(body, 0)?;
            let ntp = (u64::from(u32_at(body, 4)?) << 32) | u64::from(u32_at(body, 8)?);
            let info = SenderInfo {
                ntp,
                rtp_timestamp: u32_at(body, 12)?,
                packet_count: u32_at(body, 16)?,
                octet_count: u32_at(body, 20)?,
            };
            let reports = parse_report_blocks(get(body, 24..body.len())?, count)?;
            RtcpPacket::SenderReport {
                ssrc,
                info,
                reports,
            }
        }
        201 => {
            let ssrc = u32_at(body, 0)?;
            let reports = parse_report_blocks(get(body, 4..body.len())?, count)?;
            RtcpPacket::ReceiverReport { ssrc, reports }
        }
        202 => {
            let mut chunks = Vec::new();
            let mut pos = 0usize;
            for _ in 0..count {
                let (ssrc, items, used) = parse_sdes_chunk(get(body, pos..body.len())?)?;
                chunks.push((ssrc, items));
                pos += used;
            }
            RtcpPacket::SourceDescription(chunks)
        }
        203 => {
            let mut sources = Vec::new();
            for i in 0..usize::from(count) {
                sources.push(u32_at(body, i * 4)?);
            }
            let reason_off = usize::from(count) * 4;
            let reason = if body.len() > reason_off {
                let len = usize::from(
                    *body
                        .get(reason_off)
                        .ok_or(Error::InvalidData("RTCP BYE reason has no length byte"))?,
                );
                Some(get(body, reason_off + 1..reason_off + 1 + len)?.to_vec())
            } else {
                None
            };
            RtcpPacket::Bye { sources, reason }
        }
        other => RtcpPacket::Other {
            payload_type: other,
            data: body.to_vec(),
        },
    };
    Ok((packet, total_len))
}

/// Iterate every packet in a compound RTCP packet (RFC 3550 §6.1: RTCP is
/// never sent as a single lone packet in practice, but this walks whatever
/// is actually there, one at a time, rather than assuming a fixed count).
pub fn iter_compound(buf: &[u8]) -> impl Iterator<Item = Result<RtcpPacket>> + '_ {
    let mut pos = 0usize;
    let mut done = false;
    std::iter::from_fn(move || {
        if done || pos >= buf.len() {
            return None;
        }
        let Some(rest) = buf.get(pos..) else {
            done = true;
            return Some(Err(Error::InvalidData(
                "RTCP compound packet offset overflow",
            )));
        };
        match parse_one(rest) {
            Ok((pkt, used)) => {
                if used == 0 {
                    done = true;
                    return Some(Err(Error::InvalidData("RTCP packet claimed zero length")));
                }
                pos += used;
                Some(Ok(pkt))
            }
            Err(e) => {
                done = true;
                Some(Err(e))
            }
        }
    })
}

fn header(payload_type: u8, count: u8, length_words: u16) -> [u8; 4] {
    let b0 = (2 << 6) | (count & 0x1F);
    [
        b0,
        payload_type,
        (length_words >> 8) as u8,
        length_words as u8,
    ]
}

fn push_report_block(out: &mut Vec<u8>, rb: &ReportBlock) {
    out.extend_from_slice(&rb.ssrc.to_be_bytes());
    let word = (u32::from(rb.fraction_lost) << 24) | (rb.cumulative_lost as u32 & 0x00FF_FFFF);
    out.extend_from_slice(&word.to_be_bytes());
    out.extend_from_slice(&rb.extended_highest_seq.to_be_bytes());
    out.extend_from_slice(&rb.jitter.to_be_bytes());
    out.extend_from_slice(&rb.last_sr.to_be_bytes());
    out.extend_from_slice(&rb.delay_since_last_sr.to_be_bytes());
}

/// Build a Receiver Report (RFC 3550 §6.4.2). `reports.len()` must fit in 5
/// bits (`<= 31`); callers in this workspace never negotiate more than one
/// source per session, so this truncates silently rather than erroring on a
/// case that cannot arise from any RTSP negotiation this crate performs.
#[must_use]
pub fn build_rr(ssrc: u32, reports: &[ReportBlock]) -> Vec<u8> {
    let count = reports.len().min(31);
    let length_words = u16::try_from(1 + count * REPORT_BLOCK_LEN_WORDS).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    out.extend_from_slice(&header(201, count as u8, length_words));
    out.extend_from_slice(&ssrc.to_be_bytes());
    for rb in reports.iter().take(count) {
        push_report_block(&mut out, rb);
    }
    out
}

/// Build a Sender Report (RFC 3550 §6.4.1), for `vaco-mux-rtp`.
#[must_use]
pub fn build_sr(ssrc: u32, info: &SenderInfo, reports: &[ReportBlock]) -> Vec<u8> {
    let count = reports.len().min(31);
    let length_words = u16::try_from(6 + count * REPORT_BLOCK_LEN_WORDS).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    out.extend_from_slice(&header(200, count as u8, length_words));
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&((info.ntp >> 32) as u32).to_be_bytes());
    out.extend_from_slice(&(info.ntp as u32).to_be_bytes());
    out.extend_from_slice(&info.rtp_timestamp.to_be_bytes());
    out.extend_from_slice(&info.packet_count.to_be_bytes());
    out.extend_from_slice(&info.octet_count.to_be_bytes());
    for rb in reports.iter().take(count) {
        push_report_block(&mut out, rb);
    }
    out
}

/// Build a `BYE` (RFC 3550 §6.6), for session teardown.
#[must_use]
pub fn build_bye(sources: &[u32]) -> Vec<u8> {
    let count = sources.len().min(31);
    let length_words = u16::try_from(count).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    out.extend_from_slice(&header(203, count as u8, length_words));
    for s in sources.iter().take(count) {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    fn sample_block() -> ReportBlock {
        ReportBlock {
            ssrc: 0x1111_2222,
            fraction_lost: 3,
            cumulative_lost: -5,
            extended_highest_seq: 1000,
            jitter: 42,
            last_sr: 7,
            delay_since_last_sr: 9,
        }
    }

    #[test]
    fn rr_round_trips() {
        let built = build_rr(0xAAAA_BBBB, &[sample_block()]);
        let (pkt, used) = parse_one(&built).unwrap();
        assert_eq!(used, built.len());
        match pkt {
            RtcpPacket::ReceiverReport { ssrc, reports } => {
                assert_eq!(ssrc, 0xAAAA_BBBB);
                assert_eq!(reports, vec![sample_block()]);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn sr_round_trips() {
        let info = SenderInfo {
            ntp: 0x0102_0304_0506_0708,
            rtp_timestamp: 90000,
            packet_count: 10,
            octet_count: 12345,
        };
        let built = build_sr(0x1234, &info, &[sample_block()]);
        let (pkt, used) = parse_one(&built).unwrap();
        assert_eq!(used, built.len());
        match pkt {
            RtcpPacket::SenderReport {
                ssrc,
                info: got,
                reports,
            } => {
                assert_eq!(ssrc, 0x1234);
                assert_eq!(got, info);
                assert_eq!(reports, vec![sample_block()]);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bye_round_trips_without_reason() {
        let built = build_bye(&[1, 2, 3]);
        let (pkt, _) = parse_one(&built).unwrap();
        assert_eq!(
            pkt,
            RtcpPacket::Bye {
                sources: vec![1, 2, 3],
                reason: None
            }
        );
    }

    #[test]
    fn compound_iterates_every_packet() {
        let mut buf = build_rr(1, &[]);
        buf.extend_from_slice(&build_bye(&[1]));
        let parsed: Vec<_> = iter_compound(&buf).collect::<Result<_>>().unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn rejects_length_overrunning_buffer() {
        let mut built = build_rr(1, &[sample_block()]);
        // Claim a much larger length than actually present.
        built[2] = 0xFF;
        built[3] = 0xFF;
        assert!(parse_one(&built).is_err());
    }

    #[test]
    fn rejects_report_block_count_overrunning_buffer() {
        // count = 31 report blocks declared, no body at all.
        let buf = [0xFFu8, 201, 0, 1];
        assert!(parse_one(&buf).is_err());
    }

    proptest::proptest! {
        #[test]
        fn parse_one_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let _ = parse_one(&bytes);
        }

        #[test]
        fn iter_compound_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..1024)) {
            let _: Vec<_> = iter_compound(&bytes).take(64).collect();
        }
    }
}
