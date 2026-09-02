//! The Keep-Alive message — `VSF TR-06-2:2022` §5.6.3/§5.6.4 (`Fig. 8`),
//! draft-derived throughout.
//!
//! # What this module does not do
//!
//! §5.6.4's JSON payload (`tunnelIP`/`remoteIP`/`excludedIP`/`routing`/
//! `pskRotation`/`vendor`/`features`) is carried here as opaque bytes, not
//! parsed into a structured type. Adopting a JSON crate is a D10/D11
//! decision this crate does not make as a side effect of building GRE
//! framing — nothing in `#559`'s own scope needs to read a field out of
//! that payload, only to carry it. A future package that does can parse
//! [`KeepAliveMessage::json_payload`] with whatever it adopts.

use vaco_core::{Error, Result};

fn malformed(detail: &'static str) -> Error {
    Error::InvalidData(detail)
}

/// §5.6.3 `Fig. 8`'s capability flags, `X` through `F`, packed into the
/// 16 bits following the 48-bit MAC address (13 named flags, MSB-first,
/// then a 3-bit `Rsvd1` this module does not expose since the spec says
/// to ignore it on reception).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is one independent wire bit (Fig. 8) -- collapsing them into enums would not reduce the thirteen independent yes/no facts a packet actually carries, only rename them"
)]
pub struct CapabilityFlags {
    /// `X` — more capabilities follow in the JSON payload (reserved for
    /// future use).
    pub more_capabilities: bool,
    /// `R` — routing (non-RIST traffic) capability.
    pub routing: bool,
    /// `B` — bonding support (RIST Simple Profile).
    pub bonding: bool,
    /// `A` — adaptive encoding support.
    pub adaptive_encoding: bool,
    /// `P` — SMPTE-2022-1 FEC support.
    pub fec: bool,
    /// `E` — SMPTE-2022-7 seamless redundancy switch support.
    pub seamless_switch: bool,
    /// `L` — load sharing (reserved for future use).
    pub load_sharing: bool,
    /// `N` — NULL packet deletion support (§8.3).
    pub null_packet_deletion: bool,
    /// `D` — this is a Disconnect message (§5.6.5).
    pub disconnect: bool,
    /// `T` — this is a Reconnect message (§5.6.6).
    pub reconnect: bool,
    /// `V` — Reduced Overhead Mode support (§5.3.2).
    pub reduced_overhead: bool,
    /// `J` — JSON send/receive/process capability (§5.6.4).
    pub json: bool,
    /// `F` — on-the-fly PSK passphrase change capability (§7.4).
    pub psk_on_the_fly: bool,
}

impl CapabilityFlags {
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self {
            more_capabilities: (bits >> 15) & 1 != 0,
            routing: (bits >> 14) & 1 != 0,
            bonding: (bits >> 13) & 1 != 0,
            adaptive_encoding: (bits >> 12) & 1 != 0,
            fec: (bits >> 11) & 1 != 0,
            seamless_switch: (bits >> 10) & 1 != 0,
            load_sharing: (bits >> 9) & 1 != 0,
            null_packet_deletion: (bits >> 8) & 1 != 0,
            disconnect: (bits >> 7) & 1 != 0,
            reconnect: (bits >> 6) & 1 != 0,
            reduced_overhead: (bits >> 5) & 1 != 0,
            json: (bits >> 4) & 1 != 0,
            psk_on_the_fly: (bits >> 3) & 1 != 0,
        }
    }

    #[must_use]
    pub const fn to_bits(self) -> u16 {
        (self.more_capabilities as u16) << 15
            | (self.routing as u16) << 14
            | (self.bonding as u16) << 13
            | (self.adaptive_encoding as u16) << 12
            | (self.fec as u16) << 11
            | (self.seamless_switch as u16) << 10
            | (self.load_sharing as u16) << 9
            | (self.null_packet_deletion as u16) << 8
            | (self.disconnect as u16) << 7
            | (self.reconnect as u16) << 6
            | (self.reduced_overhead as u16) << 5
            | (self.json as u16) << 4
            | (self.psk_on_the_fly as u16) << 3
        // Rsvd1, bits 2-0, always zero on transmission.
    }
}

/// The Keep-Alive message (`Fig. 8`): a 48-bit MAC address, the capability
/// flags, and an opaque JSON payload (see the module docs on why it is not
/// parsed here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAliveMessage {
    pub mac_address: [u8; 6],
    pub flags: CapabilityFlags,
    pub json_payload: Vec<u8>,
}

impl KeepAliveMessage {
    /// # Errors
    /// [`Error::InvalidData`] if `data` is shorter than the 8-byte fixed
    /// header.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mac_address: [u8; 6] = data
            .get(0..6)
            .ok_or_else(|| malformed("Keep-Alive message shorter than the 6-byte MAC address"))?
            .try_into()
            .map_err(|_| malformed("Keep-Alive MAC address is not 6 bytes"))?;
        let flags_word = data
            .get(6..8)
            .ok_or_else(|| malformed("Keep-Alive message has no capability-flags word"))?;
        let flags_bits = u16::from_be_bytes(
            flags_word
                .try_into()
                .map_err(|_| malformed("Keep-Alive capability-flags word is not 2 bytes"))?,
        );
        let json_payload = data.get(8..).unwrap_or(&[]).to_vec();
        Ok(Self {
            mac_address,
            flags: CapabilityFlags::from_bits(flags_bits),
            json_payload,
        })
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.mac_address);
        out.extend_from_slice(&self.flags.to_bits().to_be_bytes());
        out.extend_from_slice(&self.json_payload);
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    // --- draft-derived: `TR-06-2` Fig. 8's own bit positions, and §5.6's
    // own enumeration of the thirteen named flags.

    #[test]
    fn all_flags_clear_round_trips() {
        let msg = KeepAliveMessage {
            mac_address: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            flags: CapabilityFlags::default(),
            json_payload: vec![],
        };
        let bytes = msg.serialize();
        assert_eq!(bytes.len(), 8);
        assert_eq!(KeepAliveMessage::parse(&bytes).unwrap(), msg);
    }

    #[test]
    fn every_flag_is_at_its_own_bit_independently() {
        // Set exactly one flag at a time and confirm no other flag (nor
        // Rsvd1) is disturbed -- the direct check that each flag's bit
        // position in `from_bits`/`to_bits` matches Fig. 8's left-to-right
        // (MSB-first) ordering X,R,B,A,P,E,L,N,D,T,V,J,F.
        let setters: [(fn(&mut CapabilityFlags), u16); 13] = [
            (|f| f.more_capabilities = true, 1 << 15),
            (|f| f.routing = true, 1 << 14),
            (|f| f.bonding = true, 1 << 13),
            (|f| f.adaptive_encoding = true, 1 << 12),
            (|f| f.fec = true, 1 << 11),
            (|f| f.seamless_switch = true, 1 << 10),
            (|f| f.load_sharing = true, 1 << 9),
            (|f| f.null_packet_deletion = true, 1 << 8),
            (|f| f.disconnect = true, 1 << 7),
            (|f| f.reconnect = true, 1 << 6),
            (|f| f.reduced_overhead = true, 1 << 5),
            (|f| f.json = true, 1 << 4),
            (|f| f.psk_on_the_fly = true, 1 << 3),
        ];
        for (set, expected_bit) in setters {
            let mut flags = CapabilityFlags::default();
            set(&mut flags);
            assert_eq!(
                flags.to_bits(),
                expected_bit,
                "bit mismatch for a single flag"
            );
            assert_eq!(CapabilityFlags::from_bits(expected_bit), flags);
        }
    }

    #[test]
    fn reserved_bits_are_ignored_on_parse_and_zeroed_on_serialize() {
        // Rsvd1 (bits 2-0) set on input must be ignored, and this
        // implementation's own serialize() must never set them.
        let flags = CapabilityFlags::from_bits(0b0000_0000_0000_0111);
        assert_eq!(flags, CapabilityFlags::default());
        assert_eq!(flags.to_bits() & 0b111, 0);
    }

    #[test]
    fn json_payload_round_trips_as_opaque_bytes() {
        let msg = KeepAliveMessage {
            mac_address: [1, 2, 3, 4, 5, 6],
            flags: CapabilityFlags {
                json: true,
                ..CapabilityFlags::default()
            },
            json_payload: br#"{"tunnelIP":"10.0.0.2"}"#.to_vec(),
        };
        let bytes = msg.serialize();
        let parsed = KeepAliveMessage::parse(&bytes).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn disconnect_and_reconnect_flags_round_trip() {
        let msg = KeepAliveMessage {
            mac_address: [0; 6],
            flags: CapabilityFlags {
                disconnect: true,
                ..CapabilityFlags::default()
            },
            json_payload: vec![],
        };
        let bytes = msg.serialize();
        assert!(KeepAliveMessage::parse(&bytes).unwrap().flags.disconnect);
        assert!(!KeepAliveMessage::parse(&bytes).unwrap().flags.reconnect);
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(KeepAliveMessage::parse(&[0u8; 7]).is_err());
    }
}
