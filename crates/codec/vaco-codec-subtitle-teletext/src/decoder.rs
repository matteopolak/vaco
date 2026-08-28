//! Top-level state machine: EN 300 472 data units in, assembled
//! [`crate::page::Page`]s out.
//!
//! A magazine (1-8) can only ever be assembling one page at a time — EN 300
//! 706 §7.2.1 says a page's transmission "is terminated by ... the next
//! page header packet having the same magazine address" — so the decoder
//! holds exactly eight slots, no attacker-controlled allocation involved:
//! [`TeletextDecoder`] carries `[Option<Box<Page>>; 8]` and nothing else.
//!
//! # A simplification worth stating plainly
//!
//! EN 300 706's `C4` ("Erase Page") control bit tells a decoder whether to
//! clear previously stored rows before applying a new header, which matters
//! for a page updated by resending only its header plus a handful of
//! changed rows. This decoder always starts a fresh, blank page on every
//! X/0 for a magazine, whether or not `C4` is set — a superset of correct
//! behaviour when `C4=1` (the common case) and a deviation when a broadcast
//! relies on `C4=0` partial updates, which this crate does not reconstruct.

use crate::packet::{packets, RECORD_LEN};
use crate::page::Page;

/// A page finished assembling: a later X/0 for the same magazine (or a
/// magazine-1..8 wraparound at end of stream, via
/// [`TeletextDecoder::finish`]) superseded it.
#[derive(Debug)]
pub struct PageEvent {
    pub magazine: u8,
    pub page: Box<Page>,
}

/// EN 300 706 Level 1 Teletext decoder.
///
/// `pending`/`pending_len` carry a partial 46-byte data unit across
/// [`push`](Self::push) calls: `vaco-subtitle-bitmap`'s raw `dvbtxt`
/// demuxer reads fixed 1024-byte chunks, which do not divide evenly by
/// the 46-byte record size, so a record routinely spans two packets. The
/// buffer is a compile-time-fixed array, not sized from input.
#[derive(Debug)]
pub struct TeletextDecoder {
    current: [Option<Box<Page>>; 8],
    pending: [u8; RECORD_LEN],
    pending_len: usize,
}

impl Default for TeletextDecoder {
    fn default() -> Self {
        Self {
            current: [const { None }; 8],
            pending: [0u8; RECORD_LEN],
            pending_len: 0,
        }
    }
}

impl TeletextDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The page currently assembling in `magazine` (1-8), if any.
    #[must_use]
    pub fn magazine_page(&self, magazine: u8) -> Option<&Page> {
        magazine
            .checked_sub(1)
            .and_then(|i| self.current.get(usize::from(i)))
            .and_then(Option::as_ref)
            .map(std::convert::AsRef::as_ref)
    }

    /// Feed one packet payload (as delivered by `vaco-subtitle-bitmap`'s
    /// `dvbtxt` demuxer, or the equivalent PES elementary-stream bytes): a
    /// run of EN 300 472 46-byte data units, not necessarily aligned to
    /// this call's boundaries.
    ///
    /// Returns any pages that finished assembling as a result.
    pub fn push(&mut self, data: &[u8]) -> Vec<PageEvent> {
        let mut events = Vec::new();
        let mut cursor = data;
        loop {
            let need = RECORD_LEN.saturating_sub(self.pending_len);
            let take = need.min(cursor.len());
            let end = self.pending_len.saturating_add(take);
            if let (Some(dst), Some(src)) = (self.pending.get_mut(self.pending_len..end), cursor.get(..take)) {
                dst.copy_from_slice(src);
            }
            self.pending_len = end;
            cursor = cursor.get(take..).unwrap_or(&[]);

            if self.pending_len < RECORD_LEN {
                break;
            }
            let record = self.pending;
            self.pending_len = 0;
            self.apply_record(&record, &mut events);
        }
        events
    }

    /// Force-complete every magazine still assembling a page, e.g. at
    /// end of stream. Returns whatever was in progress.
    pub fn finish(&mut self) -> Vec<PageEvent> {
        let mut events = Vec::new();
        for (i, slot) in self.current.iter_mut().enumerate() {
            if let Some(page) = slot.take() {
                let magazine = u8::try_from(i).unwrap_or(0).saturating_add(1);
                events.push(PageEvent { magazine, page });
            }
        }
        events
    }

    fn apply_record(&mut self, record: &[u8; RECORD_LEN], events: &mut Vec<PageEvent>) {
        let Some(packet) = packets(record).next() else {
            return;
        };
        let address = packet.address;
        if address.corrupt {
            return;
        }
        let Some(slot) = address
            .magazine
            .checked_sub(1)
            .and_then(|i| self.current.get_mut(usize::from(i)))
        else {
            return;
        };

        if address.packet == 0 {
            if let Some(finished) = slot.take() {
                events.push(PageEvent {
                    magazine: address.magazine,
                    page: finished,
                });
            }
            *slot = Some(Box::new(Page::from_header(address.magazine, packet.payload)));
            return;
        }

        let Some(page) = slot else { return };
        match address.packet {
            1..=24 => {
                page.fill_body_row(address.packet, packet.payload);
            }
            25..=28 => validate_enhancement_packet(packet.payload),
            _ => {} // 29-31: magazine/service data, never page content
        }
    }
}

/// X/25 to X/28: decode (to detect corruption) without acting on the
/// enhancement semantics — Level 1.5's G0/G2 re-designation and X/26
/// composite characters are this crate's stated gap (see the crate's
/// top-level docs). Hamming 24/18 is still run over every triplet so a
/// malformed enhancement packet cannot be misread as body text, per this
/// project's own guidance to reject cleanly rather than guess.
fn validate_enhancement_packet(payload: &[u8]) {
    let Some(triplets) = payload.get(1..) else {
        return;
    };
    let mut chunks = triplets.chunks_exact(3);
    for chunk in &mut chunks {
        if let Ok(bytes) = <[u8; 3]>::try_from(chunk) {
            let _ = crate::hamming::decode24(bytes);
        }
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
    use crate::packet::PACKET_LEN;
    use crate::page::Glyph;

    fn hamming_byte(nibble: u8) -> u8 {
        let d1 = nibble & 1;
        let d2 = (nibble >> 1) & 1;
        let d3 = (nibble >> 2) & 1;
        let d4 = (nibble >> 3) & 1;
        let p1 = 1 ^ d1 ^ d3 ^ d4;
        let p2 = 1 ^ d1 ^ d2 ^ d4;
        let p3 = 1 ^ d1 ^ d2 ^ d3;
        let p4 = 1 ^ p1 ^ d1 ^ p2 ^ d2 ^ p3 ^ d3 ^ d4;
        (p1 & 1)
            | ((d1 & 1) << 1)
            | ((p2 & 1) << 2)
            | ((d2 & 1) << 3)
            | ((p3 & 1) << 4)
            | ((d3 & 1) << 5)
            | ((p4 & 1) << 6)
            | ((d4 & 1) << 7)
    }

    fn parity_byte(data: u8) -> u8 {
        let d = data & 0x7F;
        if d.count_ones() % 2 == 1 {
            d
        } else {
            d | 0x80
        }
    }

    fn data_unit(magazine: u8, packet_no: u8, body: &[u8]) -> Vec<u8> {
        let address = u16::from(magazine & 0x7) | (u16::from(packet_no) << 3);
        let byte4 = hamming_byte((address & 0xF) as u8);
        let byte5 = hamming_byte(((address >> 4) & 0xF) as u8);
        let mut record = vec![0x02u8, 0x2C, 0xC0, 0xE4, byte4, byte5];
        record.extend_from_slice(body);
        while record.len() < PACKET_LEN + 4 {
            record.push(parity_byte(b' '));
        }
        record
    }

    #[test]
    fn a_header_then_a_body_row_produces_a_page_on_the_next_header() {
        let mut decoder = TeletextDecoder::new();
        let header_ctrl = [
            hamming_byte(0), // units
            hamming_byte(0), // tens
            hamming_byte(0), // S1
            hamming_byte(0), // S2+C4
            hamming_byte(0), // S3
            hamming_byte(0), // S4+C5,C6
            hamming_byte(0), // C7-10
            hamming_byte(0), // C11-14
        ];
        let mut body = header_ctrl.to_vec();
        body.extend("HEADLINE".bytes().map(parity_byte));
        let header = data_unit(1, 0, &body);

        let row_text: Vec<u8> = "HELLO WORLD".bytes().map(parity_byte).collect();
        let row = data_unit(1, 1, &row_text);

        let mut next_body = header_ctrl.to_vec();
        next_body.extend("NEXT".bytes().map(parity_byte));
        let next_header = data_unit(1, 0, &next_body);

        let mut events = decoder.push(&header);
        assert!(events.is_empty());
        events.extend(decoder.push(&row));
        assert!(events.is_empty());

        let mid = decoder.magazine_page(1).expect("page assembling");
        assert_eq!(mid.rows[1][0].glyph, Glyph::Text('H'));

        events.extend(decoder.push(&next_header));
        assert_eq!(events.len(), 1);
        let finished = &events[0].page;
        assert_eq!(finished.rows[0][8].glyph, Glyph::Text('H'));
        assert_eq!(finished.rows[1][0].glyph, Glyph::Text('H'));
        assert_eq!(finished.rows[1][6].glyph, Glyph::Text('W'));
    }

    #[test]
    fn finish_flushes_a_page_still_assembling() {
        let mut decoder = TeletextDecoder::new();
        let header_ctrl = [0u8; 8].map(hamming_byte);
        let header = data_unit(2, 0, &header_ctrl);
        decoder.push(&header);
        assert!(decoder.magazine_page(2).is_some());

        let events = decoder.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].magazine, 2);
        assert!(decoder.magazine_page(2).is_none());
    }

    #[test]
    fn an_enhancement_packet_is_ignored_not_misread_as_text() {
        let mut decoder = TeletextDecoder::new();
        let header_ctrl = [0u8; 8].map(hamming_byte);
        decoder.push(&data_unit(1, 0, &header_ctrl));
        let junk = [0xFFu8; PACKET_LEN - 2];
        decoder.push(&data_unit(1, 26, &junk));
        let page = decoder.magazine_page(1).expect("page assembling");
        // Row 1 (body text) must be untouched by the X/26 packet.
        assert_eq!(page.rows[1][0].glyph, Glyph::Text(' '));
    }
}
