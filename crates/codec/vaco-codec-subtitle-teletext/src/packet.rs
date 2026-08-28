//! Magazine/packet address decode (EN 300 706 §7.1.2) and the EN 300 472
//! 46-byte data-unit framing this crate's input arrives in.
//!
//! # Framing, measured rather than guessed
//!
//! `vaco-subtitle-bitmap`'s `dvbtxt` demuxer (`crates/format/vaco-subtitle-
//! bitmap/src/dvbtxt/teletext.rs`) already documents, and `vaco-demux-
//! mpegts` already detects via its teletext descriptor handling, that a DVB
//! teletext elementary stream — whether read as a raw file or demultiplexed
//! from a PES stream — is a sequence of fixed 46-byte data units:
//! `data_unit_id`(1) — `0x02` non-subtitle, `0x03` subtitle, `0xFF` stuffing
//! — `data_unit_length`(1, always `0x2C` = 44), then a 2-byte line/field
//! framing field this crate does not need for page assembly, followed by
//! the 42-byte EN 300 706 packet itself (bytes 4-45 of that spec's own
//! numbering: 2 Hamming 8/4 address bytes plus 40 data bytes).

use crate::hamming;

/// `data_unit_length`'s one legal value (EN 300 472).
pub const DATA_UNIT_LENGTH: u8 = 0x2C;

/// `data_unit_id`(1) + `data_unit_length`(1) + 2 bytes framing + 42-byte
/// packet.
pub const RECORD_LEN: usize = 46;

/// The 42-byte EN 300 706 packet within a data unit's data field.
pub const PACKET_LEN: usize = 42;

const fn is_plausible_unit_id(id: u8) -> bool {
    matches!(id, 0x02 | 0x03 | 0xFF)
}

/// A magazine (1-8) and packet number (0-31), decoded from a packet's first
/// two bytes (EN 300 706 §7.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketAddress {
    pub magazine: u8,
    pub packet: u8,
    /// Whether either address byte's Hamming 8/4 check failed
    /// uncorrectably; callers should treat the address as unreliable.
    pub corrupt: bool,
}

impl PacketAddress {
    /// Decode from `packet[0..2]` (EN 300 706 bytes 4-5).
    #[must_use]
    pub fn decode(byte4: u8, byte5: u8) -> Self {
        let (d1, c1) = hamming::decode8(byte4);
        let (d2, c2) = hamming::decode8(byte5);
        let magazine_bits = d1 & 0x7;
        let magazine = if magazine_bits == 0 { 8 } else { magazine_bits };
        let packet = ((d1 >> 3) & 1) | (d2 << 1);
        Self {
            magazine,
            packet,
            corrupt: !c1.is_usable() || !c2.is_usable(),
        }
    }
}

/// One 42-byte EN 300 706 packet, addressed and ready for
/// [`crate::page::Page`] to interpret.
#[derive(Debug, Clone, Copy)]
pub struct Packet<'a> {
    pub address: PacketAddress,
    /// EN 300 706 bytes 6-45: 40 bytes for a header or body row, or a
    /// designation byte plus Hamming 24/18 triplets for an enhancement
    /// packet.
    pub payload: &'a [u8],
}

/// Split `data` into EN 300 472 data units and yield the decoded packet
/// address plus body for each non-stuffing one.
///
/// Stops cleanly (rather than erroring) at the first byte that does not
/// start a well-formed 46-byte record, matching this crate's `dvbtxt`
/// sibling's own probe logic — a demuxer packet boundary need not land on a
/// data-unit boundary, and the remainder of a push is simply not yet
/// available.
pub fn packets(data: &[u8]) -> impl Iterator<Item = Packet<'_>> {
    DataUnits { data }.filter_map(|record| {
        let id = *record.first()?;
        if id == 0xFF {
            return None;
        }
        let packet = record.get(4..4 + PACKET_LEN)?;
        let byte4 = *packet.first()?;
        let byte5 = *packet.get(1)?;
        let payload = packet.get(2..)?;
        Some(Packet {
            address: PacketAddress::decode(byte4, byte5),
            payload,
        })
    })
}

struct DataUnits<'a> {
    data: &'a [u8],
}

impl<'a> Iterator for DataUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let record = self.data.get(..RECORD_LEN)?;
        let &id = record.first()?;
        let &len = record.get(1)?;
        if !is_plausible_unit_id(id) || len != DATA_UNIT_LENGTH {
            return None;
        }
        self.data = self.data.get(RECORD_LEN..).unwrap_or(&[]);
        Some(record)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

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

    #[test]
    fn address_decodes_magazine_and_packet_number() {
        // Magazine 3, packet 7: address value = magazine(3) | packet(7)<<3 = 0x3B.
        let address = 0x3Bu8;
        let byte4 = hamming_byte(address & 0xF);
        let byte5 = hamming_byte((address >> 4) & 0xF);
        let decoded = PacketAddress::decode(byte4, byte5);
        assert_eq!(decoded.magazine, 3);
        assert_eq!(decoded.packet, 7);
        assert!(!decoded.corrupt);
    }

    #[test]
    fn magazine_zero_means_eight() {
        let byte4 = hamming_byte(0);
        let byte5 = hamming_byte(0);
        let decoded = PacketAddress::decode(byte4, byte5);
        assert_eq!(decoded.magazine, 8);
        assert_eq!(decoded.packet, 0);
    }

    fn record(id: u8, packet_body: [u8; PACKET_LEN]) -> [u8; RECORD_LEN] {
        let mut r = [0u8; RECORD_LEN];
        r[0] = id;
        r[1] = DATA_UNIT_LENGTH;
        r[2] = 0xC0; // reserved bits + parity/line-offset, unused by this crate
        r[3] = 0xE4; // framing code
        r[4..].copy_from_slice(&packet_body);
        r
    }

    #[test]
    fn packets_iterates_a_run_and_skips_stuffing() {
        let byte4 = hamming_byte(0);
        let byte5 = hamming_byte(0);
        let mut body = [0u8; PACKET_LEN];
        body[0] = byte4;
        body[1] = byte5;

        let mut data = Vec::new();
        data.extend_from_slice(&record(0x02, body));
        data.extend_from_slice(&record(0xFF, [0u8; PACKET_LEN]));
        data.extend_from_slice(&record(0x03, body));

        let found: Vec<_> = packets(&data).collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].address.magazine, 8);
        assert_eq!(found[0].payload.len(), 40);
    }

    #[test]
    fn packets_stops_cleanly_on_a_truncated_trailer() {
        let byte4 = hamming_byte(0);
        let byte5 = hamming_byte(0);
        let mut body = [0u8; PACKET_LEN];
        body[0] = byte4;
        body[1] = byte5;
        let mut data = record(0x02, body).to_vec();
        data.extend_from_slice(&[0x02, DATA_UNIT_LENGTH, 0, 0]); // incomplete

        let found: Vec<_> = packets(&data).collect();
        assert_eq!(found.len(), 1);
    }
}
