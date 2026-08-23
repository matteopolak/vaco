//! The Ogg page header and its segment table (RFC 3533 §6).
//!
//! This module is deliberately I/O-free: it turns a byte slice already in
//! memory into a header plus the byte ranges its lacing table describes, and
//! nothing here allocates more than the segment table itself (at most 255
//! bytes). [`crate::demux`] owns the buffered reading, resynchronisation and
//! budget accounting; [`vaco_mux_ogg`](../vaco_mux_ogg/index.html) (a
//! sibling crate, per the same D19 pattern `vaco-mux-flv` uses against
//! `vaco-demux-flv`) reuses [`OggPageHeader`] and [`packet_spans`] so the two
//! crates share one definition of what a page *is*, even though only the
//! demuxer parses one off the wire and only the muxer builds one from
//! scratch.
//!
//! # Layout
//!
//! ```text
//!  0   4   capture_pattern   "OggS"
//!  4   1   stream_structure_version   always 0
//!  5   1   header_type_flag   bit0 continued, bit1 bos, bit2 eos
//!  6   8   granule_position   i64 LE; -1 means no packet completes here
//! 14   4   bitstream_serial_number   u32 LE
//! 18   4   page_sequence_number   u32 LE
//! 22   4   checksum   u32 LE, computed with this field zeroed
//! 26   1   page_segments   N
//! 27   N   segment_table   N lacing values, one byte each
//! 27+N …   body, length = sum(segment_table)
//! ```
//!
//! # Lacing, in one paragraph
//!
//! Each lacing value is a byte 0..=255. A run of `255`s glues segments into
//! one packet; the run ends at the first value less than 255, and that
//! value's own bytes are still part of the packet (a value of `0` after a
//! `255` means "the packet is an exact multiple of 255 bytes; this segment
//! contributes nothing further"). If a page's segment table *ends* on a
//! `255` — nothing left to say whether the packet is done — the packet is
//! **continued**: the next page for this serial number must carry the
//! `continued` header flag and resume the same packet with its own first run.

use vaco_core::{Error, Result};

/// `"OggS"`, the four bytes every page begins with.
pub const CAPTURE_PATTERN: [u8; 4] = *b"OggS";

/// Bytes fixed before the segment table: capture pattern through
/// `page_segments`.
pub const FIXED_HEADER_LEN: usize = 27;

/// Offset of the four checksum bytes within the fixed header.
pub const CHECKSUM_OFFSET: usize = 22;

/// A page can declare at most this many segments (`page_segments` is a
/// single byte).
pub const MAX_SEGMENTS: usize = 255;

/// A lacing value this high means "more of this packet follows"; a full
/// segment.
pub const CONTINUATION_VALUE: u8 = 255;

/// Largest possible body: 255 segments of 255 bytes each.
pub const MAX_BODY_LEN: usize = MAX_SEGMENTS * CONTINUATION_VALUE as usize;

/// Largest possible whole page: fixed header + full segment table + full body.
pub const MAX_PAGE_LEN: usize = FIXED_HEADER_LEN + MAX_SEGMENTS + MAX_BODY_LEN;

/// The stream structure version this crate understands. RFC 3533 defines only
/// `0`; a higher value is a format we cannot parse, not corruption.
pub const SUPPORTED_VERSION: u8 = 0;

bitflags::bitflags! {
    /// `header_type_flag`, RFC 3533 §6.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct OggHeaderFlags: u8 {
        /// This page's first packet continues one from the previous page.
        const CONTINUED = 1 << 0;
        /// First page of a logical bitstream.
        const BOS = 1 << 1;
        /// Last page of a logical bitstream.
        const EOS = 1 << 2;
    }
}

/// The granule position field, signed and 64 bits. `-1` is the reserved
/// "no packet completes here" value (RFC 3533 §6), not an ordinary sample
/// count that happens to be negative — [`OggPageHeader::granule`] returns
/// `None` for it so a caller cannot accidentally treat it as a timestamp.
pub const GRANULE_UNSET: i64 = -1;

/// A parsed page header: everything before the body.
#[derive(Debug, Clone)]
pub struct OggPageHeader {
    pub version: u8,
    pub flags: OggHeaderFlags,
    /// Raw 64-bit field. `-1` means unset; read through [`Self::granule`].
    pub granule_position: i64,
    pub serial: u32,
    pub sequence: u32,
    pub checksum: u32,
    /// Lacing values, at most [`MAX_SEGMENTS`] of them.
    pub segments: Vec<u8>,
}

impl OggPageHeader {
    /// The granule position, or `None` for the reserved "unset" value.
    #[must_use]
    pub const fn granule(&self) -> Option<i64> {
        if self.granule_position == GRANULE_UNSET {
            None
        } else {
            Some(self.granule_position)
        }
    }

    /// Total body length the segment table describes.
    #[must_use]
    pub fn body_len(&self) -> usize {
        self.segments.iter().map(|&b| b as usize).sum()
    }

    /// Bytes this page occupies on the wire, header and body together.
    #[must_use]
    pub fn total_len(&self) -> usize {
        FIXED_HEADER_LEN + self.segments.len() + self.body_len()
    }
}

/// Parse the fixed header and segment table from the start of `data`.
///
/// `data` must already contain the whole header — the fixed 27 bytes plus
/// `page_segments` more — but need not contain the body. Returns the header
/// and the offset the body starts at (`FIXED_HEADER_LEN + page_segments`).
///
/// # Errors
///
/// [`Error::InvalidData`] when the capture pattern, version or bounds do not
/// hold; [`Error::UnexpectedEof`] when `data` is shorter than the header it
/// claims.
pub fn parse_header(data: &[u8]) -> Result<(OggPageHeader, usize)> {
    let fixed = data.get(..FIXED_HEADER_LEN).ok_or(Error::UnexpectedEof)?;
    let Some(capture) = fixed.get(0..4) else {
        return Err(Error::UnexpectedEof);
    };
    if capture != CAPTURE_PATTERN {
        return Err(Error::InvalidData("not an Ogg page: bad capture pattern"));
    }
    let version = *fixed.get(4).ok_or(Error::UnexpectedEof)?;
    if version != SUPPORTED_VERSION {
        return Err(Error::Unsupported("Ogg stream structure version"));
    }
    let flags = OggHeaderFlags::from_bits_truncate(*fixed.get(5).ok_or(Error::UnexpectedEof)?);
    let granule_position = i64::from_le_bytes(
        fixed
            .get(6..14)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?,
    );
    let serial = u32::from_le_bytes(
        fixed
            .get(14..18)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?,
    );
    let sequence = u32::from_le_bytes(
        fixed
            .get(18..22)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?,
    );
    let checksum = u32::from_le_bytes(
        fixed
            .get(CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?,
    );
    let page_segments = usize::from(*fixed.get(26).ok_or(Error::UnexpectedEof)?);
    let table_end = FIXED_HEADER_LEN.saturating_add(page_segments);
    let segments = data
        .get(FIXED_HEADER_LEN..table_end)
        .ok_or(Error::UnexpectedEof)?
        .to_vec();
    Ok((
        OggPageHeader {
            version,
            flags,
            granule_position,
            serial,
            sequence,
            checksum,
            segments,
        },
        table_end,
    ))
}

/// Verify `page` (the exact `FIXED_HEADER_LEN + segments.len() + body_len`
/// bytes read off the wire, checksum field included) against the checksum it
/// declares.
///
/// Computed as three [`crate::crc::crc32_update`] calls around the zeroed
/// checksum field rather than by cloning the buffer: `crc(before) ->
/// crc(four zero bytes) -> crc(after)`, which is what RFC 3533 §6 means by
/// "the CRC of the entire page with the checksum field replaced by zero".
///
/// # Errors
///
/// [`Error::UnexpectedEof`] if `page` is shorter than a page could be.
pub fn verify_checksum(page: &[u8]) -> Result<bool> {
    let before = page.get(..CHECKSUM_OFFSET).ok_or(Error::UnexpectedEof)?;
    let after = page
        .get(CHECKSUM_OFFSET + 4..)
        .ok_or(Error::UnexpectedEof)?;
    let stored = u32::from_le_bytes(
        page.get(CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?,
    );
    let mut crc = crate::crc::crc32(before);
    crc = crate::crc::crc32_update(crc, &[0, 0, 0, 0]);
    crc = crate::crc::crc32_update(crc, after);
    Ok(crc == stored)
}

/// One run the segment table describes: a byte range into the page body, and
/// whether a packet actually finishes there.
///
/// `complete = false` is only possible for the table's last run, and only
/// when its last lacing value is [`CONTINUATION_VALUE`] — the packet is not
/// finished by this page and must be resumed by a later page for the same
/// serial number, which is required to carry
/// [`OggHeaderFlags::CONTINUED`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketSpan {
    pub start: usize,
    pub end: usize,
    pub complete: bool,
}

/// Split a segment table into the runs it describes.
///
/// Pure arithmetic over lacing values — no bound on `segments.len()` is
/// assumed beyond what the caller already enforced by capping it at
/// [`MAX_SEGMENTS`], so this never allocates more than one [`PacketSpan`] per
/// table entry and terminates on any input, including an empty table (which
/// yields no spans: a page with `page_segments == 0` carries no payload).
#[must_use]
pub fn packet_spans(segments: &[u8]) -> Vec<PacketSpan> {
    let mut spans = Vec::new();
    let mut run_start = 0usize;
    let mut pos = 0usize;
    let last = segments.len().saturating_sub(1);
    for (i, &v) in segments.iter().enumerate() {
        pos = pos.saturating_add(usize::from(v));
        if v < CONTINUATION_VALUE {
            spans.push(PacketSpan {
                start: run_start,
                end: pos,
                complete: true,
            });
            run_start = pos;
        } else if i == last {
            // The table ran out while still inside a run: unfinished.
            spans.push(PacketSpan {
                start: run_start,
                end: pos,
                complete: false,
            });
        }
    }
    spans
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn sample_header_bytes(segments: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; FIXED_HEADER_LEN];
        v[0..4].copy_from_slice(&CAPTURE_PATTERN);
        v[4] = 0;
        v[5] = OggHeaderFlags::BOS.bits();
        v[6..14].copy_from_slice(&42i64.to_le_bytes());
        v[14..18].copy_from_slice(&7u32.to_le_bytes());
        v[18..22].copy_from_slice(&0u32.to_le_bytes());
        // checksum left zero
        v[26] = u8::try_from(segments.len()).unwrap();
        v.extend_from_slice(segments);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parses_a_well_formed_header() {
        let bytes = sample_header_bytes(&[5, 3], &[0u8; 8]);
        let (h, body_at) = parse_header(&bytes).unwrap();
        assert_eq!(h.serial, 7);
        assert_eq!(h.granule(), Some(42));
        assert!(h.flags.contains(OggHeaderFlags::BOS));
        assert_eq!(body_at, FIXED_HEADER_LEN + 2);
        assert_eq!(h.body_len(), 8);
    }

    #[test]
    fn unset_granule_is_reserved_not_negative_one_sample() {
        let bytes = sample_header_bytes(&[0], &[]);
        let mut bytes = bytes;
        bytes[6..14].copy_from_slice(&GRANULE_UNSET.to_le_bytes());
        let (h, _) = parse_header(&bytes).unwrap();
        assert_eq!(h.granule(), None);
        assert_eq!(h.granule_position, -1);
    }

    #[test]
    fn rejects_bad_capture_pattern() {
        let mut bytes = sample_header_bytes(&[0], &[]);
        bytes[0] = b'X';
        assert!(matches!(parse_header(&bytes), Err(Error::InvalidData(_))));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut bytes = sample_header_bytes(&[0], &[]);
        bytes[4] = 1;
        assert!(matches!(parse_header(&bytes), Err(Error::Unsupported(_))));
    }

    #[test]
    fn short_input_is_eof_not_a_panic() {
        for len in 0..FIXED_HEADER_LEN {
            let bytes = sample_header_bytes(&[1, 2, 3], &[0u8; 6]);
            assert!(matches!(
                parse_header(&bytes[..len]),
                Err(Error::UnexpectedEof)
            ));
        }
    }

    #[test]
    fn a_single_short_packet_is_one_complete_span() {
        let spans = packet_spans(&[10]);
        assert_eq!(
            spans,
            vec![PacketSpan {
                start: 0,
                end: 10,
                complete: true
            }]
        );
    }

    #[test]
    fn two_packets_in_one_page() {
        let spans = packet_spans(&[5, 3]);
        assert_eq!(
            spans,
            vec![
                PacketSpan {
                    start: 0,
                    end: 5,
                    complete: true
                },
                PacketSpan {
                    start: 5,
                    end: 8,
                    complete: true
                }
            ]
        );
    }

    #[test]
    fn a_packet_that_is_an_exact_multiple_of_255_needs_a_trailing_zero() {
        let spans = packet_spans(&[255, 0]);
        assert_eq!(
            spans,
            vec![PacketSpan {
                start: 0,
                end: 255,
                complete: true
            }]
        );
    }

    #[test]
    fn a_table_ending_on_255_is_an_open_continuation() {
        let spans = packet_spans(&[255, 255, 255]);
        assert_eq!(
            spans,
            vec![PacketSpan {
                start: 0,
                end: 765,
                complete: false
            }]
        );
    }

    #[test]
    fn an_empty_table_has_no_spans() {
        assert!(packet_spans(&[]).is_empty());
    }

    #[test]
    fn a_zero_length_packet_is_representable() {
        // Two consecutive zero-length packets: two lacing values of 0.
        let spans = packet_spans(&[0, 0]);
        assert_eq!(
            spans,
            vec![
                PacketSpan {
                    start: 0,
                    end: 0,
                    complete: true
                },
                PacketSpan {
                    start: 0,
                    end: 0,
                    complete: true
                }
            ]
        );
    }

    #[test]
    fn checksum_round_trips_through_the_zeroing_convention() {
        let mut bytes = sample_header_bytes(&[3], &[1, 2, 3]);
        let crc = crate::crc::crc32(&{
            let mut z = bytes.clone();
            z[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
            z
        });
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        assert!(verify_checksum(&bytes).unwrap());
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        assert!(!verify_checksum(&bytes).unwrap());
    }

    #[test]
    fn max_page_len_matches_the_worst_case_arithmetic() {
        assert_eq!(MAX_PAGE_LEN, 27 + 255 + 255 * 255);
        assert_eq!(MAX_PAGE_LEN, 65_307);
    }
}
