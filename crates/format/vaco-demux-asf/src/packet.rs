//! ASF Data Packet parsing: error correction, payload parsing information,
//! and the four payload shapes [\[ASF\] §5.2](vaco_format_asf) defines
//! (single, single-compressed, multiple, multiple-compressed).
//!
//! This is deliberately the largest module in the crate. ASF is a
//! fixed-size-packet format: every Data Packet in a file is the same length
//! (declared once, in the File Properties Object), so a media object bigger
//! than one packet is split across several payloads (a *fragment*), and
//! several small objects are packed into one packet behind a
//! multiple-payload header. Getting the length-type-flag decoding and the
//! compressed-payload sub-payload loop right is most of what makes this
//! format's demuxer hard, and this module is where that lives.
//!
//! # What this module does not do
//!
//! It parses one packet's bytes into a flat list of [`ParsedPayload`]s. It
//! does **not** reassemble a fragmented media object across packets — that
//! is [`crate::demux::AsfDemuxer`]'s per-stream state, since it spans
//! packets and this module only ever sees one.

use vaco_core::{Error, Result};

/// How many bits of a `Length Type Flags`/`Property Flags` sub-field select a
/// field width, decoded once into a byte count (0 meaning "absent").
const fn width_for(bits: u8) -> usize {
    match bits & 0b11 {
        0b00 => 0,
        0b01 => 1,
        0b10 => 2,
        _ => 4,
    }
}

/// Decoded `Length Type Flags` (the first byte of the payload-parsing
/// information, [\[ASF\] §5.2.2](vaco_format_asf)).
#[derive(Debug, Clone, Copy)]
struct LengthTypeFlags {
    multiple_payloads: bool,
    sequence_width: usize,
    padding_width: usize,
    packet_length_width: usize,
}

impl LengthTypeFlags {
    fn parse(b: u8) -> Self {
        Self {
            multiple_payloads: b & 0x01 != 0,
            sequence_width: width_for(b >> 1),
            padding_width: width_for(b >> 3),
            packet_length_width: width_for(b >> 5),
        }
    }
}

/// Decoded `Property Flags` (the second byte, same section).
///
/// The Stream Number field's own width sub-field is deliberately not
/// modelled: unlike the other three, [\[ASF\] §5.2.2](vaco_format_asf) gives
/// the Stream Number field itself a fixed `BYTE` type in every payload
/// table, and states its own length-type sub-field "shall be set to 01" —
/// there is no legal value that changes how many bytes to read.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "every field genuinely is a field width from the spec's own Property Flags table; \
              naming them anything else would be less clear, not more"
)]
struct PropertyFlags {
    replicated_data_width: usize,
    offset_width: usize,
    media_object_number_width: usize,
}

impl PropertyFlags {
    fn parse(b: u8) -> Self {
        Self {
            replicated_data_width: width_for(b),
            offset_width: width_for(b >> 2),
            media_object_number_width: width_for(b >> 4),
        }
    }
}

/// A tiny forward-only cursor. Distinct from [`vaco_bitstream::ByteReader`]
/// only in that it reads variable-width (0/1/2/4-byte) little-endian
/// integers, which is the one operation this format's headers need over and
/// over and `ByteReader` has no primitive for.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn u8(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or(Error::InvalidData("asf: packet truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    /// A little-endian integer of `width` bytes (0, 1, 2 or 4), `0` for
    /// width 0 (field absent).
    fn width(&mut self, width: usize) -> Result<u32> {
        match width {
            0 => Ok(0),
            1 => Ok(u32::from(self.u8()?)),
            2 => {
                let b = self.bytes(2)?;
                let arr = b
                    .first_chunk::<2>()
                    .ok_or(Error::InvalidData("asf: packet truncated"))?;
                Ok(u32::from(u16::from_le_bytes(*arr)))
            }
            _ => {
                let b = self.bytes(4)?;
                let arr = b
                    .first_chunk::<4>()
                    .ok_or(Error::InvalidData("asf: packet truncated"))?;
                Ok(u32::from_le_bytes(*arr))
            }
        }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let b = self
            .data
            .get(self.pos..self.pos + n)
            .ok_or(Error::InvalidData("asf: packet truncated"))?;
        self.pos += n;
        Ok(b)
    }
}

/// One flattened payload out of a Data Packet.
///
/// A compressed sub-payload ([\[ASF\] §5.2.3.2/5.2.3.4](vaco_format_asf)) is
/// always a whole media object, so it carries no offset; an ordinary payload
/// may be a whole object or one fragment of one, which
/// [`crate::demux::AsfDemuxer`]'s reassembly state distinguishes using
/// `offset` and `total_len`.
#[derive(Debug, Clone, Copy)]
pub struct ParsedPayload<'a> {
    pub stream_number: u8,
    pub key_frame: bool,
    pub media_object_number: u32,
    /// Byte offset within the media object (ordinary payload), or `None` for
    /// a compressed sub-payload, which has no offset because it is always
    /// complete.
    pub offset: Option<u32>,
    /// The media object's total size in bytes, from Replicated Data, when
    /// present (Replicated Data Length >= 8). `None` when a payload carries
    /// no Replicated Data (Replicated Data Length == 0) — legal per spec,
    /// just less informative to a reassembler.
    pub total_len: Option<u32>,
    /// Presentation time in milliseconds, when known: from Replicated Data
    /// for an ordinary payload, or computed from Presentation Time +
    /// Presentation Time Delta * index for a compressed sub-payload.
    pub pts_ms: Option<u32>,
    pub data: &'a [u8],
}

/// Parse one Data Packet — exactly `packet.len()` bytes, already sliced to
/// the container's fixed packet size — into its flattened payload list.
///
/// # Errors
/// [`Error::InvalidData`] on any field that cannot be decoded within the
/// packet's own bytes (a truncated header, a padding length larger than the
/// packet, a payload count of zero). Never panics and never reads past
/// `packet`, however the length-type flags are set — every field width is
/// attacker-controlled.
pub fn parse_packet(packet: &[u8]) -> Result<Vec<ParsedPayload<'_>>> {
    let mut cur = Cursor::new(packet);
    let first = cur.u8()?;

    // Error correction data ([ASF] §5.2.1), if the high bit of the very
    // first packet byte says it is present. Never emitted by
    // `vaco-mux-asf`, but real-world files (including ffmpeg's own asf
    // muxer, measured) do carry it, so it must be skipped correctly.
    let length_flags_byte = if first & 0x80 != 0 {
        let ecc_len = usize::from(first & 0x0F);
        cur.bytes(ecc_len)?;
        cur.u8()?
    } else {
        first
    };

    let ltf = LengthTypeFlags::parse(length_flags_byte);
    let pf = PropertyFlags::parse(cur.u8()?);

    let _packet_length = cur.width(ltf.packet_length_width)?;
    let _sequence = cur.width(ltf.sequence_width)?;
    let padding_length = cur.width(ltf.padding_width)? as usize;
    let _send_time = cur.width(4)?; // Send Time: always a DWORD.
    let _duration = cur.width(2)?; // Duration: always a WORD.

    // The payload region ends `padding_length` bytes before the packet ends.
    // Clamped, not trusted: a padding length larger than what is left would
    // otherwise underflow.
    let region_end = packet.len().saturating_sub(padding_length).max(cur.pos);

    let mut out = Vec::new();
    if ltf.multiple_payloads {
        let flags = cur.u8()?;
        let count = usize::from(flags & 0x3F);
        let payload_length_width = width_for(flags >> 6);
        if count == 0 {
            return Err(Error::InvalidData(
                "asf: multiple-payload packet declares zero payloads",
            ));
        }
        for _ in 0..count {
            if cur.pos >= region_end {
                break;
            }
            parse_one_payload(
                &mut cur,
                pf,
                Some(payload_length_width),
                region_end,
                &mut out,
            )?;
        }
    } else {
        parse_one_payload(&mut cur, pf, None, region_end, &mut out)?;
    }
    Ok(out)
}

/// Parse one payload entry (§5.2.3.1/.2 for a single-payload packet, or one
/// element of §5.2.3.3/.4's array for a multiple-payload packet), appending
/// every logical payload it expands to (more than one for a compressed
/// payload's sub-payloads) to `out`.
fn parse_one_payload<'a>(
    cur: &mut Cursor<'a>,
    pf: PropertyFlags,
    payload_length_width: Option<usize>,
    region_end: usize,
    out: &mut Vec<ParsedPayload<'a>>,
) -> Result<()> {
    let stream_byte = cur.u8()?;
    let stream_number = stream_byte & 0x7F;
    let key_frame = stream_byte & 0x80 != 0;
    let media_object_number = cur.width(pf.media_object_number_width)?;
    let offset_or_time = cur.width(pf.offset_width)?;
    let replicated_len = cur.width(pf.replicated_data_width)? as usize;

    if replicated_len == 1 {
        // Compressed payload: the "replicated data" slot is instead a single
        // Presentation Time Delta byte, and `offset_or_time` is a
        // Presentation Time, not an offset (§5.2.3.2/.4).
        let delta = cur.u8()?;
        let region = compressed_region_len(cur, payload_length_width, region_end)?;
        let sub_end = cur.pos + region;
        let mut i: u32 = 0;
        while cur.pos < sub_end {
            let len = usize::from(cur.u8()?);
            let data = cur.bytes(len.min(sub_end.saturating_sub(cur.pos)))?;
            let pts_ms = offset_or_time.saturating_add(u32::from(delta).saturating_mul(i));
            out.push(ParsedPayload {
                stream_number,
                key_frame,
                media_object_number: media_object_number.saturating_add(i),
                offset: None,
                total_len: Some(len as u32),
                pts_ms: Some(pts_ms),
                data,
            });
            i = i.saturating_add(1);
        }
        // Sub-payloads are counted, not declared, so a region that ran out
        // mid-length-byte is simply where the loop stops — no error, per the
        // spec's own "detected by the end of the data" rule.
        return Ok(());
    }

    let replicated = if replicated_len > 0 {
        Some(cur.bytes(replicated_len.min(region_end.saturating_sub(cur.pos)))?)
    } else {
        None
    };
    let total_len = replicated
        .and_then(|r| r.first_chunk::<4>())
        .map(|b| u32::from_le_bytes(*b));
    let pts_ms = replicated
        .and_then(|r| r.get(4..8))
        .and_then(<[u8]>::first_chunk::<4>)
        .map(|b| u32::from_le_bytes(*b));

    let data_len = match payload_length_width {
        Some(w) => cur.width(w)? as usize,
        None => region_end.saturating_sub(cur.pos),
    };
    let data = cur.bytes(data_len.min(region_end.saturating_sub(cur.pos)))?;

    out.push(ParsedPayload {
        stream_number,
        key_frame,
        media_object_number,
        offset: Some(offset_or_time),
        total_len,
        pts_ms,
        data,
    });
    Ok(())
}

/// The byte length of a compressed payload's sub-payload region: the
/// explicit Payload Length field in a multiple-payload packet, or "the rest
/// of the packet up to padding" in a single-payload packet (§5.2.3.2's own
/// wording — the spec gives no explicit length there because there is only
/// one payload to bound it against).
fn compressed_region_len(
    cur: &mut Cursor<'_>,
    payload_length_width: Option<usize>,
    region_end: usize,
) -> Result<usize> {
    match payload_length_width {
        Some(w) => Ok((cur.width(w)? as usize).min(region_end.saturating_sub(cur.pos))),
        None => Ok(region_end.saturating_sub(cur.pos)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Build a single-stream, single-fragment, non-multiple-payload packet:
    /// Length Type Flags=0 (no ECC, no multiple payloads, no packet length,
    /// no sequence, no padding), Property Flags = the spec-recommended
    /// 0x5D (replicated=BYTE, offset=DWORD, media object number=BYTE, stream
    /// number=BYTE), Send Time/Duration = 0, then one ordinary payload.
    fn simple_packet(
        stream: u8,
        key: bool,
        mo_num: u8,
        offset: u32,
        replicated: Option<[u8; 8]>,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0x00u8, 0x5D]; // length type flags, property flags
        out.extend_from_slice(&0u32.to_le_bytes()); // send time
        out.extend_from_slice(&0u16.to_le_bytes()); // duration
        out.push(stream | if key { 0x80 } else { 0 });
        out.push(mo_num);
        out.extend_from_slice(&offset.to_le_bytes());
        match replicated {
            Some(r) => {
                out.push(8);
                out.extend_from_slice(&r);
            }
            None => out.push(0),
        }
        out.extend_from_slice(payload);
        out
    }

    fn replicated(total_len: u32, pts_ms: u32) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[0..4].copy_from_slice(&total_len.to_le_bytes());
        r[4..8].copy_from_slice(&pts_ms.to_le_bytes());
        r
    }

    #[test]
    fn single_payload_with_replicated_data_parses() {
        let pkt = simple_packet(1, true, 5, 0, Some(replicated(4, 1234)), b"data");
        let payloads = parse_packet(&pkt).unwrap();
        assert_eq!(payloads.len(), 1);
        let p = &payloads[0];
        assert_eq!(p.stream_number, 1);
        assert!(p.key_frame);
        assert_eq!(p.media_object_number, 5);
        assert_eq!(p.offset, Some(0));
        assert_eq!(p.total_len, Some(4));
        assert_eq!(p.pts_ms, Some(1234));
        assert_eq!(p.data, b"data");
    }

    #[test]
    fn single_payload_with_no_replicated_data_parses() {
        let pkt = simple_packet(2, false, 0, 0, None, b"xy");
        let payloads = parse_packet(&pkt).unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].total_len, None);
        assert_eq!(payloads[0].pts_ms, None);
        assert_eq!(payloads[0].data, b"xy");
    }

    #[test]
    fn multiple_payloads_in_one_packet_parse_in_order() {
        let mut out = vec![0x01u8, 0x5D]; // multiple payloads bit set
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(0b1000_0010); // payload flags: 2 payloads, length type=WORD
        for (stream, data) in [(1u8, b"aa".as_slice()), (2u8, b"bbb".as_slice())] {
            out.push(stream);
            out.push(0); // media object number
            out.extend_from_slice(&0u32.to_le_bytes()); // offset
            out.push(0); // no replicated data
            out.extend_from_slice(&(data.len() as u16).to_le_bytes());
            out.extend_from_slice(data);
        }
        let payloads = parse_packet(&out).unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].stream_number, 1);
        assert_eq!(payloads[0].data, b"aa");
        assert_eq!(payloads[1].stream_number, 2);
        assert_eq!(payloads[1].data, b"bbb");
    }

    #[test]
    fn error_correction_present_is_skipped_before_the_payload_header() {
        // Byte0 = 0x82: ECC present, ECC data length=2. Two ECC bytes follow,
        // then the ordinary Length Type Flags / Property Flags header.
        let mut out = vec![0x82u8, 0x00, 0x00, 0x00, 0x5D];
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(9); // stream number
        out.push(1); // media object number
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(0);
        out.extend_from_slice(b"z");
        let payloads = parse_packet(&out).unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].stream_number, 9);
        assert_eq!(payloads[0].data, b"z");
    }

    #[test]
    fn compressed_single_payload_splits_into_sub_payloads() {
        // Length type flags=0 (single payload). Replicated Data Length=1
        // marks this compressed; presentation time = offset field (500),
        // delta=100. Two sub-payloads: "ab" then "cde".
        let mut out = vec![0x00u8, 0x5D];
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(3); // stream number
        out.push(7); // media object number (first sub-payload's)
        out.extend_from_slice(&500u32.to_le_bytes()); // presentation time
        out.push(1); // replicated data length == 1 => compressed
        out.push(100); // presentation time delta
        out.push(2); // sub-payload 0 length
        out.extend_from_slice(b"ab");
        out.push(3); // sub-payload 1 length
        out.extend_from_slice(b"cde");
        let payloads = parse_packet(&out).unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].media_object_number, 7);
        assert_eq!(payloads[0].pts_ms, Some(500));
        assert_eq!(payloads[0].data, b"ab");
        assert_eq!(payloads[1].media_object_number, 8);
        assert_eq!(payloads[1].pts_ms, Some(600));
        assert_eq!(payloads[1].data, b"cde");
    }

    #[test]
    fn padding_shrinks_the_payload_region() {
        // Padding Length Type = 01 (BYTE): bits3-4 of 0x08 = 0b01000 -> 01.
        // Field order per [ASF] §5.2.2 is Packet Length, Sequence, Padding
        // Length, Send Time, Duration — padding length comes *before* send
        // time/duration, not after.
        let mut out = vec![0x08u8, 0x5D];
        out.push(5); // padding length = 5 bytes
        out.extend_from_slice(&0u32.to_le_bytes()); // send time
        out.extend_from_slice(&0u16.to_le_bytes()); // duration
        out.push(1); // stream number
        out.push(0);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(0);
        out.extend_from_slice(b"hello");
        out.extend_from_slice(&[0u8; 5]); // padding
        let payloads = parse_packet(&out).unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].data, b"hello");
    }

    #[test]
    fn a_zero_payload_count_is_rejected_not_a_panic() {
        let mut out = vec![0x01u8, 0x5D];
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(0b0000_0000); // 0 payloads
        assert!(parse_packet(&out).is_err());
    }

    #[test]
    fn a_truncated_packet_errors_rather_than_panics() {
        assert!(parse_packet(&[]).is_err());
        assert!(parse_packet(&[0x01, 0x5D]).is_err());
    }

    proptest::proptest! {
        #[test]
        fn parse_packet_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let _ = parse_packet(&bytes);
        }
    }
}
