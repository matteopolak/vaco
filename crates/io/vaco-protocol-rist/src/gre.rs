//! GRE-over-UDP tunnelling — `VSF TR-06-2:2022` §5, built on RFC 8086's own
//! GRE header (C/K/S flags, `Reserved0`, `Ver`, `Protocol Type`, and the
//! optional Checksum/Key/Sequence-Number words RFC 2890 defines), with the
//! RIST-specific `H`/`RV` bits §5.1 carves out of `Reserved0`'s bits 9-12.
//!
//! draft-derived throughout: every field, bit position and figure here is
//! `TR-06-2`'s own (Fig. 1-6), not inferred.
//!
//! # What this module does not do
//!
//! Full Datagram Mode's payload is "a full layer-3 IP packet" (§5.3.1) --
//! this module carries that payload as opaque bytes rather than parsing an
//! IP header, since nothing in this crate needs to look inside it (RIST
//! traffic arrives via Reduced Overhead Mode in practice; Full Datagram
//! Mode exists for generic non-RIST tunnelled traffic, out of this crate's
//! scope by definition). Likewise the JSON Keep-Alive payload
//! ([`crate::keepalive`]) is carried as opaque bytes -- no JSON crate is a
//! D10/D11 decision this module does not make on its own.

use vaco_core::{Error, Result};

fn malformed(detail: &'static str) -> Error {
    Error::InvalidData(detail)
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16> {
    let s = buf.get(at..at + 2).ok_or_else(|| malformed("GRE field runs past the buffer"))?;
    let arr: [u8; 2] = s.try_into().map_err(|_| malformed("GRE field is not 2 bytes"))?;
    Ok(u16::from_be_bytes(arr))
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32> {
    let s = buf.get(at..at + 4).ok_or_else(|| malformed("GRE field runs past the buffer"))?;
    let arr: [u8; 4] = s.try_into().map_err(|_| malformed("GRE field is not 4 bytes"))?;
    Ok(u32::from_be_bytes(arr))
}

/// The VSF `EtherType` (§5.2): `TR-06-2` traffic's GRE `Protocol Type` when
/// carrying a VSF Packet Header.
pub const VSF_ETHERTYPE: u16 = 0xCCE0;
/// Full Datagram Mode's GRE `Protocol Type` (§5.3.1): plain IPv4.
pub const PROTOCOL_TYPE_IP: u16 = 0x0800;

/// §5.1's `RV` field (`Reserved0` bits 10-12): which `TR-06-2` revision's
/// packet format a sender is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RistVersion {
    V2020,
    V2021,
    V2022,
    /// `RV` 011/100: unlisted, but the spec says to assume backward
    /// compatibility with `TR-06-2:2022` and process it as such.
    AssumeCompatible(u8),
    /// `RV` 101/110/111: unlisted and not assumed compatible -- the spec
    /// says to discard the packet. This module still parses it (so a
    /// caller can log/count it before discarding) rather than erroring.
    Unknown(u8),
}

impl RistVersion {
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0b111 {
            0b000 => Self::V2020,
            0b001 => Self::V2021,
            0b010 => Self::V2022,
            0b011 | 0b100 => Self::AssumeCompatible(bits & 0b111),
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn to_bits(self) -> u8 {
        match self {
            Self::V2020 => 0b000,
            Self::V2021 => 0b001,
            Self::V2022 => 0b010,
            Self::AssumeCompatible(bits) | Self::Unknown(bits) => bits,
        }
    }

    /// Whether a receiver should process this packet at all (§5.1: `RV`
    /// 101/110/111 packets "shall" be discarded as unknown format).
    #[must_use]
    pub const fn is_processable(self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

/// The GRE header (`Fig. 1`/`Fig. 2`/`Fig. 9`): the fixed first word plus
/// whichever of the Checksum, Key/Nonce and Sequence Number words RFC
/// 2890's own `C`/`K`/`S` flags say are present, in that fixed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GreHeader {
    /// `C` -- checksum word present. `TR-06-2` §5.1: "devices ... should
    /// set the C ... field to zero to reduce overhead", so this is
    /// supported but expected to almost always be `false`.
    pub checksum: Option<u16>,
    /// `K` -- for PSK (§7.1) this 32 bits is the Key/Nonce field, not a
    /// traditional GRE key identifier.
    pub key_or_nonce: Option<u32>,
    pub sequence_number: Option<u32>,
    /// `H` -- PSK AES key length (§5.1): `false` = 128-bit, `true` =
    /// 256-bit. Meaningless when `rv` is `V2020` (key length is then
    /// out-of-band) -- carried here unconditionally regardless, since
    /// this module does not know which mode a given packet is in.
    pub h: bool,
    pub rv: RistVersion,
    pub protocol_type: u16,
}

impl GreHeader {
    /// # Errors
    /// [`Error::InvalidData`] if `data` is shorter than the fixed word plus
    /// whichever optional words `C`/`K`/`S` declare present.
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        let w0 = u32_at(data, 0)?;
        let c = (w0 >> 31) & 1 != 0;
        let k = (w0 >> 29) & 1 != 0;
        let s = (w0 >> 28) & 1 != 0;
        let h = (w0 >> 22) & 1 != 0;
        let rv = RistVersion::from_bits(u8::try_from((w0 >> 19) & 0b111).unwrap_or(0));
        let protocol_type = u16::try_from(w0 & 0xffff).unwrap_or(0);

        let mut pos = 4usize;
        let checksum = if c {
            let word = u32_at(data, pos)?;
            pos += 4;
            Some(u16::try_from(word >> 16).unwrap_or(0)) // Reserved1 (low 16 bits) discarded, must be 0
        } else {
            None
        };
        let key_or_nonce = if k {
            let word = u32_at(data, pos)?;
            pos += 4;
            Some(word)
        } else {
            None
        };
        let sequence_number = if s {
            let word = u32_at(data, pos)?;
            pos += 4;
            Some(word)
        } else {
            None
        };

        Ok((
            Self {
                checksum,
                key_or_nonce,
                sequence_number,
                h,
                rv,
                protocol_type,
            },
            pos,
        ))
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut w0 = 0u32;
        if self.checksum.is_some() {
            w0 |= 1 << 31;
        }
        if self.key_or_nonce.is_some() {
            w0 |= 1 << 29;
        }
        if self.sequence_number.is_some() {
            w0 |= 1 << 28;
        }
        if self.h {
            w0 |= 1 << 22;
        }
        w0 |= u32::from(self.rv.to_bits() & 0b111) << 19;
        // Ver (bits 13-15) is always 0 per RFC 2784 -- not stored as a field.
        w0 |= u32::from(self.protocol_type);
        out.extend_from_slice(&w0.to_be_bytes());
        if let Some(checksum) = self.checksum {
            // Reserved1 (low 16 bits) is always 0 on transmission.
            let word = u32::from(checksum) << 16;
            out.extend_from_slice(&word.to_be_bytes());
        }
        if let Some(key) = self.key_or_nonce {
            out.extend_from_slice(&key.to_be_bytes());
        }
        if let Some(seq) = self.sequence_number {
            out.extend_from_slice(&seq.to_be_bytes());
        }
        out
    }
}

/// §5.2's VSF Packet Header (`Fig. 3`), present whenever
/// [`GreHeader::protocol_type`] is [`VSF_ETHERTYPE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsfHeader {
    pub protocol_type: u16,
    pub subtype: u16,
}

/// §5.2: `VSF Protocol Type` `0x0000`, "RIST Packet, defined by this
/// Specification".
pub const VSF_PROTOCOL_TYPE_RIST: u16 = 0x0000;
/// §5.2/§5.3.2: `VSF Protocol Subtype` `0x0000`, Reduced Overhead data
/// packets.
pub const VSF_SUBTYPE_REDUCED_OVERHEAD: u16 = 0x0000;
/// §5.2/§5.6.3: `VSF Protocol Subtype` `0x8000`, Keep-Alive control
/// packets.
pub const VSF_SUBTYPE_KEEP_ALIVE: u16 = 0x8000;
/// §5.2/§7.6: `VSF Protocol Subtype` `0x8001`, Future Nonce Announcement
/// control packets.
pub const VSF_SUBTYPE_FUTURE_NONCE: u16 = 0x8001;

impl VsfHeader {
    /// # Errors
    /// [`Error::InvalidData`] if `data` is shorter than 4 bytes.
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        let protocol_type = u16_at(data, 0)?;
        let subtype = u16_at(data, 2)?;
        Ok((
            Self {
                protocol_type,
                subtype,
            },
            4,
        ))
    }

    #[must_use]
    pub fn serialize(&self) -> [u8; 4] {
        let mut out = [0u8; 4];
        out[..2].copy_from_slice(&self.protocol_type.to_be_bytes());
        out[2..].copy_from_slice(&self.subtype.to_be_bytes());
        out
    }
}

/// §5.3.2's Reduced UDP Header (`Fig. 5`): just the two port fields, since
/// "the remainder of the packet payload shall be the full, unchanged
/// payload of the original UDP packet" (no length/checksum -- §5.3.2's own
/// receiving-end rules derive the payload size from the outer GRE length).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReducedUdpHeader {
    pub source_port: u16,
    pub destination_port: u16,
}

impl ReducedUdpHeader {
    /// # Errors
    /// [`Error::InvalidData`] if `data` is shorter than 4 bytes.
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        let source_port = u16_at(data, 0)?;
        let destination_port = u16_at(data, 2)?;
        Ok((
            Self {
                source_port,
                destination_port,
            },
            4,
        ))
    }

    #[must_use]
    pub fn serialize(&self) -> [u8; 4] {
        let mut out = [0u8; 4];
        out[..2].copy_from_slice(&self.source_port.to_be_bytes());
        out[2..].copy_from_slice(&self.destination_port.to_be_bytes());
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    // --- draft-derived: `TR-06-2` Fig. 1/2's own field positions.

    #[test]
    fn header_with_no_options_round_trips() {
        let header = GreHeader {
            checksum: None,
            key_or_nonce: None,
            sequence_number: None,
            h: false,
            rv: RistVersion::V2022,
            protocol_type: VSF_ETHERTYPE,
        };
        let bytes = header.serialize();
        assert_eq!(bytes.len(), 4, "no options -> just the fixed word");
        let (parsed, consumed) = GreHeader::parse(&bytes).unwrap();
        assert_eq!(consumed, 4);
        assert_eq!(parsed, header);
    }

    #[test]
    fn header_with_sequence_number_round_trips() {
        let header = GreHeader {
            checksum: None,
            key_or_nonce: None,
            sequence_number: Some(42),
            h: false,
            rv: RistVersion::V2022,
            protocol_type: PROTOCOL_TYPE_IP,
        };
        let bytes = header.serialize();
        assert_eq!(bytes.len(), 8);
        let (parsed, consumed) = GreHeader::parse(&bytes).unwrap();
        assert_eq!(consumed, 8);
        assert_eq!(parsed, header);
    }

    /// §7.1's own PSK shape: K=1 (Key/Nonce) and S=1 (Sequence Number),
    /// C=0 -- "the GRE header shall be transmitted in the clear" (§7).
    #[test]
    fn psk_shaped_header_round_trips() {
        let header = GreHeader {
            checksum: None,
            key_or_nonce: Some(0xDEAD_BEEF),
            sequence_number: Some(7),
            h: true, // 256-bit key
            rv: RistVersion::V2022,
            protocol_type: VSF_ETHERTYPE,
        };
        let bytes = header.serialize();
        assert_eq!(bytes.len(), 12);
        let (parsed, consumed) = GreHeader::parse(&bytes).unwrap();
        assert_eq!(consumed, 12);
        assert_eq!(parsed, header);
        assert!(parsed.h);
    }

    #[test]
    fn header_with_checksum_round_trips() {
        let header = GreHeader {
            checksum: Some(0xABCD),
            key_or_nonce: None,
            sequence_number: None,
            h: false,
            rv: RistVersion::V2020,
            protocol_type: PROTOCOL_TYPE_IP,
        };
        let bytes = header.serialize();
        assert_eq!(bytes.len(), 8);
        let (parsed, _) = GreHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn rv_backward_compatible_and_unknown_values_are_told_apart() {
        assert!(RistVersion::from_bits(0b011).is_processable());
        assert!(RistVersion::from_bits(0b100).is_processable());
        assert!(!RistVersion::from_bits(0b101).is_processable());
        assert!(!RistVersion::from_bits(0b110).is_processable());
        assert!(!RistVersion::from_bits(0b111).is_processable());
    }

    #[test]
    fn header_rejects_truncated_optional_word() {
        let header = GreHeader {
            checksum: None,
            key_or_nonce: Some(1),
            sequence_number: None,
            h: false,
            rv: RistVersion::V2022,
            protocol_type: VSF_ETHERTYPE,
        };
        let bytes = header.serialize();
        assert!(GreHeader::parse(&bytes[..bytes.len() - 1]).is_err());
    }

    // --- draft-derived: `TR-06-2` Fig. 3's own field positions.

    #[test]
    fn vsf_header_round_trips() {
        let header = VsfHeader {
            protocol_type: VSF_PROTOCOL_TYPE_RIST,
            subtype: VSF_SUBTYPE_KEEP_ALIVE,
        };
        let bytes = header.serialize();
        let (parsed, consumed) = VsfHeader::parse(&bytes).unwrap();
        assert_eq!(consumed, 4);
        assert_eq!(parsed, header);
    }

    // --- draft-derived: `TR-06-2` Fig. 5's own field positions.

    #[test]
    fn reduced_udp_header_round_trips() {
        let header = ReducedUdpHeader {
            source_port: 3000,
            destination_port: 3001,
        };
        let bytes = header.serialize();
        let (parsed, consumed) = ReducedUdpHeader::parse(&bytes).unwrap();
        assert_eq!(consumed, 4);
        assert_eq!(parsed, header);
    }
}
