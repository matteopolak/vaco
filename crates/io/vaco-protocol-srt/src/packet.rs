//! The SRT common packet header, and the data/control packet framings that
//! sit on top of it — `draft-sharabayko-srt-01` §3 ("Packet Structure"),
//! quoted from the fetched IETF datatracker rendering of that section.
//!
//! # Common header (`draft` §3, Figure 2)
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+- SRT Header +-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |F|        (Field meaning depends on the packet type)           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |          (Field meaning depends on the packet type)           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                           Timestamp                           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                     Destination Socket ID                     |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                        Packet Contents                        |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! `F`: "The data packet has this flag set to '0'. The control packet has
//! this flag set to '1'." Every parse in this module reads that one bit
//! first and dispatches on it — nothing else in the header is fixed-shape
//! across both packet kinds.
//!
//! # Data packet (`draft` §3.1, Figure 3)
//!
//! ```text
//! |0|                    Packet Sequence Number                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |P P|O|K K|R|                   Message Number                  |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                           Timestamp                           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                     Destination Socket ID                     |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                              Data                             |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! Field widths, second word: `PP` 2 bits (packet position: `10` first,
//! `00` middle, `01` last, `11` a single-packet message), `O` 1 bit (order:
//! deliver in order when set), `KK` 2 bits (encryption key: `00`
//! unencrypted, `01` even key, `10` odd key — `11` is reserved for control
//! packets, per the same field's control-packet meaning below), `R` 1 bit
//! (retransmitted), `Message Number` 26 bits. `2+1+2+1+26 = 32`.
//!
//! # Control packet (`draft` §3.2, Figure 4)
//!
//! ```text
//! |1|         Control Type        |            Subtype            |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                   Type-specific Information                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                           Timestamp                           |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                     Destination Socket ID                     |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                   Control Information Field                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! `Control Type` is 15 bits, `Subtype` 16 bits (`1+15+16 = 32`). Per-type
//! packets reuse "Subtype" and "Type-specific Information" for their own
//! named fields — ACK's word 1 is its own Acknowledgement Number, `DropReq`'s
//! is a Message Number, `PeerError`'s is an Error Code — this module keeps
//! the two raw (`subtype_or_reserved`, `type_specific`) and leaves that
//! reinterpretation to [`crate::handshake`] and whichever later package
//! adds ACK/NAK; this package only needs every control packet to parse and
//! re-serialize without loss, not to act on all of them (`#557`/`#556`'s
//! job, not this one's).
//!
//! Control Type values (`draft` Table 1, all draft-derived):
//!
//! | Value | Type | | Value | Type |
//! |---|---|---|---|---|
//! | `0x0000` | Handshake | | `0x0005` | Shutdown |
//! | `0x0001` | KeepAlive | | `0x0006` | AckAck |
//! | `0x0002` | Ack | | `0x0007` | DropReq |
//! | `0x0003` | Nak | | `0x0008` | PeerError |
//! | `0x0004` | CongestionWarning | | `0x7FFF` | UserDefined |

use vaco_limits::Budget;
use vaco_protocol_core::{ProtocolError, Result};

const SCHEME: &str = "srt";

pub(crate) fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// The one bit that decides everything else about how a packet parses.
///
/// Draft-derived: `draft` §3, Figure 2's `F` field.
#[must_use]
pub fn is_control_packet(first_word: u32) -> bool {
    first_word & 0x8000_0000 != 0
}

/// `draft` Table 1 — draft-derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ControlType {
    Handshake,
    KeepAlive,
    Ack,
    Nak,
    CongestionWarning,
    Shutdown,
    AckAck,
    DropReq,
    PeerError,
    UserDefined,
    /// A 15-bit value not in the table above — parsed, not rejected, so a
    /// future extension this crate does not yet know about still frames
    /// correctly (only its own body is left opaque).
    Other(u16),
}

impl ControlType {
    #[must_use]
    pub const fn from_u15(v: u16) -> Self {
        match v {
            0x0000 => Self::Handshake,
            0x0001 => Self::KeepAlive,
            0x0002 => Self::Ack,
            0x0003 => Self::Nak,
            0x0004 => Self::CongestionWarning,
            0x0005 => Self::Shutdown,
            0x0006 => Self::AckAck,
            0x0007 => Self::DropReq,
            0x0008 => Self::PeerError,
            0x7fff => Self::UserDefined,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub const fn to_u15(self) -> u16 {
        match self {
            Self::Handshake => 0x0000,
            Self::KeepAlive => 0x0001,
            Self::Ack => 0x0002,
            Self::Nak => 0x0003,
            Self::CongestionWarning => 0x0004,
            Self::Shutdown => 0x0005,
            Self::AckAck => 0x0006,
            Self::DropReq => 0x0007,
            Self::PeerError => 0x0008,
            Self::UserDefined => 0x7fff,
            Self::Other(v) => v,
        }
    }
}

/// `draft` §3.1, Figure 3's `PP` field — draft-derived values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketPosition {
    /// `10b`
    First,
    /// `00b`
    Middle,
    /// `01b`
    Last,
    /// `11b`
    Single,
}

impl PacketPosition {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b10 => Self::First,
            0b00 => Self::Middle,
            0b01 => Self::Last,
            _ => Self::Single,
        }
    }

    const fn to_bits(self) -> u8 {
        match self {
            Self::First => 0b10,
            Self::Middle => 0b00,
            Self::Last => 0b01,
            Self::Single => 0b11,
        }
    }
}

/// `draft` §3.1, Figure 3's `KK` field. `11b` is documented there as
/// "control packets only" — a data packet never carries it, so
/// [`DataPacket::parse`] treats it the same as `Unencrypted` rather than a
/// hard error: a malformed/adversarial `KK` bit pattern should not be a
/// panic or a rejected frame, and there is nothing more specific to do with
/// it at the framing layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFlag {
    Unencrypted,
    Even,
    Odd,
}

impl KeyFlag {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b01 => Self::Even,
            0b10 => Self::Odd,
            _ => Self::Unencrypted,
        }
    }

    const fn to_bits(self) -> u8 {
        match self {
            Self::Unencrypted => 0b00,
            Self::Even => 0b01,
            Self::Odd => 0b10,
        }
    }
}

/// One data packet, header fields plus payload — `draft` §3.1.
#[derive(Debug, Clone)]
pub struct DataPacket {
    /// 31 bits wide on the wire; stored widened. Wraps per `draft`'s own
    /// sequence-number arithmetic (not implemented here — #556's ARQ owns
    /// wraparound comparison).
    pub seq_no: u32,
    pub position: PacketPosition,
    pub in_order: bool,
    pub key: KeyFlag,
    pub retransmitted: bool,
    /// 26 bits wide on the wire.
    pub msg_no: u32,
    pub timestamp: u32,
    pub dest_socket_id: u32,
    pub payload: Vec<u8>,
}

const HEADER_LEN: usize = 16;

impl DataPacket {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is shorter than the 16-byte
    /// header, if the `F` bit is set (a control packet given to the wrong
    /// parser), or if `seq_no`'s top bit is set (that bit is `F`, so a data
    /// packet's own sequence number is at most 31 bits — this is checked
    /// explicitly rather than silently masked, since a masked value would
    /// misrepresent what the sender actually put on the wire).
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        budget
            .charge(data.len() as u64)
            .map_err(|_| malformed("data packet exceeds the parse budget"))?;
        if data.len() < HEADER_LEN {
            return Err(malformed("data packet shorter than the 16-byte header"));
        }
        let w0 = be32(data, 0)?;
        if is_control_packet(w0) {
            return Err(malformed("F bit set: this is a control packet"));
        }
        let seq_no = w0 & 0x7fff_ffff;
        let w1 = be32(data, 4)?;
        let position = PacketPosition::from_bits(u8::try_from(w1 >> 30).unwrap_or(0));
        let in_order = (w1 >> 29) & 1 != 0;
        let key = KeyFlag::from_bits(u8::try_from((w1 >> 27) & 0b11).unwrap_or(0));
        let retransmitted = (w1 >> 26) & 1 != 0;
        let msg_no = w1 & 0x03ff_ffff;
        let timestamp = be32(data, 8)?;
        let dest_socket_id = be32(data, 12)?;
        let payload = data.get(HEADER_LEN..).unwrap_or(&[]).to_vec();
        Ok(Self {
            seq_no,
            position,
            in_order,
            key,
            retransmitted,
            msg_no,
            timestamp,
            dest_socket_id,
            payload,
        })
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.seq_no & 0x7fff_ffff).to_be_bytes());
        let w1 = (u32::from(self.position.to_bits()) << 30)
            | (u32::from(self.in_order) << 29)
            | (u32::from(self.key.to_bits()) << 27)
            | (u32::from(self.retransmitted) << 26)
            | (self.msg_no & 0x03ff_ffff);
        out.extend_from_slice(&w1.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.dest_socket_id.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// One control packet: the fixed 16-byte frame plus a raw `cif` — `draft`
/// §3.2. Type-specific reinterpretation of `subtype_or_reserved`/
/// `type_specific`/`cif` is [`crate::handshake`]'s job for `Handshake`, and
/// left to whichever later package needs it for everything else.
#[derive(Debug, Clone)]
pub struct ControlPacket {
    pub control_type: ControlType,
    /// Raw 16 bits: `Subtype` for `Handshake`-family extension use, or the
    /// generic `Reserved` word other control types show in `draft`'s own
    /// per-type figures.
    pub subtype_or_reserved: u16,
    /// Raw 32 bits: `Type-specific Information` in the base frame, reused
    /// as the Acknowledgement Number (`Ack`/`AckAck`), Message Number
    /// (`DropReq`) or Error Code (`PeerError`) by those specific types.
    pub type_specific: u32,
    pub timestamp: u32,
    pub dest_socket_id: u32,
    /// The Control Information Field, unparsed. Empty for the control
    /// types whose own figure shows nothing after Destination Socket ID
    /// (`KeepAlive`, `CongestionWarning`, `Shutdown`, `AckAck`,
    /// `PeerError`).
    pub cif: Vec<u8>,
}

impl ControlPacket {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is shorter than the 16-byte
    /// frame, or if the `F` bit is clear (a data packet given to the wrong
    /// parser).
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        budget
            .charge(data.len() as u64)
            .map_err(|_| malformed("control packet exceeds the parse budget"))?;
        if data.len() < HEADER_LEN {
            return Err(malformed(
                "control packet shorter than the 16-byte base frame",
            ));
        }
        let w0 = be32(data, 0)?;
        if !is_control_packet(w0) {
            return Err(malformed("F bit clear: this is a data packet"));
        }
        let control_type = ControlType::from_u15(u16::try_from((w0 >> 16) & 0x7fff).unwrap_or(0));
        let subtype_or_reserved = u16::try_from(w0 & 0xffff).unwrap_or(0);
        let type_specific = be32(data, 4)?;
        let timestamp = be32(data, 8)?;
        let dest_socket_id = be32(data, 12)?;
        let cif = data.get(HEADER_LEN..).unwrap_or(&[]).to_vec();
        Ok(Self {
            control_type,
            subtype_or_reserved,
            type_specific,
            timestamp,
            dest_socket_id,
            cif,
        })
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let w0 = 0x8000_0000u32
            | (u32::from(self.control_type.to_u15() & 0x7fff) << 16)
            | u32::from(self.subtype_or_reserved);
        out.extend_from_slice(&w0.to_be_bytes());
        out.extend_from_slice(&self.type_specific.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.dest_socket_id.to_be_bytes());
        out.extend_from_slice(&self.cif);
        out
    }
}

/// One packet, either kind — dispatches on the `F` bit, per `draft` §3.
#[derive(Debug, Clone)]
pub enum SrtPacket {
    Data(DataPacket),
    Control(ControlPacket),
}

impl SrtPacket {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is too short to hold even the
    /// common 16-byte frame.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        if data.len() < HEADER_LEN {
            return Err(malformed("packet shorter than the 16-byte common header"));
        }
        let w0 = be32(data, 0)?;
        if is_control_packet(w0) {
            ControlPacket::parse(data, budget).map(Self::Control)
        } else {
            DataPacket::parse(data, budget).map(Self::Data)
        }
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            Self::Data(d) => d.serialize(),
            Self::Control(c) => c.serialize(),
        }
    }
}

/// A bounds-checked big-endian `u32` read, so every field access in this
/// crate goes through `slice::get` rather than direct indexing
/// (`clippy::indexing_slicing`).
pub(crate) fn be32(data: &[u8], at: usize) -> Result<u32> {
    let bytes: [u8; 4] = data
        .get(at..at + 4)
        .ok_or_else(|| malformed("truncated 32-bit field"))?
        .try_into()
        .map_err(|_| malformed("truncated 32-bit field"))?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget::new(vaco_limits::Limits::permissive())
    }

    /// Draft-derived: `draft` §3.1 Figure 3's exact field layout, built by
    /// hand from the bit widths the figure states and checked byte for
    /// byte, not round-tripped through this crate's own serializer.
    #[test]
    fn data_packet_matches_the_drafts_own_field_layout() {
        // seq_no = 0x12345678 & 0x7fffffff (F bit clear).
        // PP=10 (First), O=1, KK=01 (Even), R=0, msg_no=0x0000001.
        // word1 = 1000_1_01_0_000...0001 = 0x8A00_0001? compute by hand:
        // PP(2)=10 O(1)=1 KK(2)=01 R(1)=0 -> top byte bits: 10 1 01 0 = 1010 1010b = 0xAA,
        // then remaining 24 bits are the low 24 bits of msg_no (top 2 bits
        // of msg_no's 26 live in that same byte's low 2 bits: byte is
        // PP(2)O(1)KK(2)R(1)+msg_no[25:24](2) = 8 bits.
        // msg_no = 1 -> top 2 bits (25:24) = 00, so byte = 1010_1000 = 0xA8.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // F=0 (0x12 has top bit clear)
        bytes.extend_from_slice(&[0xA8, 0x00, 0x00, 0x01]); // word1
        bytes.extend_from_slice(&0x0000_0064u32.to_be_bytes()); // timestamp = 100
        bytes.extend_from_slice(&0x0000_002Au32.to_be_bytes()); // dest socket id = 42
        bytes.extend_from_slice(b"payload");

        let mut b = budget();
        let pkt = DataPacket::parse(&bytes, &mut b).unwrap();
        assert_eq!(pkt.seq_no, 0x1234_5678);
        assert_eq!(pkt.position, PacketPosition::First);
        assert!(pkt.in_order);
        assert_eq!(pkt.key, KeyFlag::Even);
        assert!(!pkt.retransmitted);
        assert_eq!(pkt.msg_no, 1);
        assert_eq!(pkt.timestamp, 100);
        assert_eq!(pkt.dest_socket_id, 42);
        assert_eq!(pkt.payload, b"payload");
    }

    // Self-consistency: this crate's own encoder and decoder agree, for
    // every corner of the bitfield space, not just the one hand-built
    // example above. Does not, on its own, prove either side matches the
    // draft — see the hand-built test above and
    // `handshake::tests` for the draft-derived checks.
    proptest::proptest! {
        #[test]
        fn data_packet_round_trips(
            seq_no in 0u32..=0x7fff_ffff,
            pp in 0u8..4,
            in_order: bool,
            kk in 0u8..3,
            retransmitted: bool,
            msg_no in 0u32..=0x03ff_ffff,
            timestamp: u32,
            dest_socket_id: u32,
            payload in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
        ) {
            let pkt = DataPacket {
                seq_no,
                position: PacketPosition::from_bits(pp),
                in_order,
                key: KeyFlag::from_bits(kk),
                retransmitted,
                msg_no,
                timestamp,
                dest_socket_id,
                payload,
            };
            let bytes = pkt.serialize();
            let mut b = budget();
            let back = DataPacket::parse(&bytes, &mut b).unwrap();
            assert_eq!(back.seq_no, pkt.seq_no);
            assert_eq!(back.position, pkt.position);
            assert_eq!(back.in_order, pkt.in_order);
            assert_eq!(back.key, pkt.key);
            assert_eq!(back.retransmitted, pkt.retransmitted);
            assert_eq!(back.msg_no, pkt.msg_no);
            assert_eq!(back.timestamp, pkt.timestamp);
            assert_eq!(back.dest_socket_id, pkt.dest_socket_id);
            assert_eq!(back.payload, pkt.payload);
        }
    }

    /// Draft-derived: `draft` Table 1's control type values, checked
    /// against the numeric constants directly, not round-tripped.
    #[test]
    fn control_type_values_match_the_draft_table() {
        assert_eq!(ControlType::Handshake.to_u15(), 0x0000);
        assert_eq!(ControlType::KeepAlive.to_u15(), 0x0001);
        assert_eq!(ControlType::Ack.to_u15(), 0x0002);
        assert_eq!(ControlType::Nak.to_u15(), 0x0003);
        assert_eq!(ControlType::CongestionWarning.to_u15(), 0x0004);
        assert_eq!(ControlType::Shutdown.to_u15(), 0x0005);
        assert_eq!(ControlType::AckAck.to_u15(), 0x0006);
        assert_eq!(ControlType::DropReq.to_u15(), 0x0007);
        assert_eq!(ControlType::PeerError.to_u15(), 0x0008);
        assert_eq!(ControlType::UserDefined.to_u15(), 0x7fff);
    }

    /// Draft-derived: `draft` §3.2 Figure 4's exact field layout.
    #[test]
    fn control_packet_matches_the_drafts_own_field_layout() {
        let mut bytes = Vec::new();
        // F=1, Control Type=0x0002 (Ack), Subtype/Reserved=0x0000.
        bytes.extend_from_slice(&0x8002_0000u32.to_be_bytes());
        bytes.extend_from_slice(&0x0000_0007u32.to_be_bytes()); // ack number 7
        bytes.extend_from_slice(&0x0000_0064u32.to_be_bytes()); // timestamp 100
        bytes.extend_from_slice(&0x0000_002Au32.to_be_bytes()); // dest socket id 42
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // opaque CIF bytes

        let mut b = budget();
        let pkt = ControlPacket::parse(&bytes, &mut b).unwrap();
        assert_eq!(pkt.control_type, ControlType::Ack);
        assert_eq!(pkt.subtype_or_reserved, 0);
        assert_eq!(pkt.type_specific, 7);
        assert_eq!(pkt.timestamp, 100);
        assert_eq!(pkt.dest_socket_id, 42);
        assert_eq!(pkt.cif, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    proptest::proptest! {
        #[test]
        fn control_packet_round_trips(
            control_type_raw in 0u16..0x8000,
            subtype in proptest::prelude::any::<u16>(),
            type_specific: u32,
            timestamp: u32,
            dest_socket_id: u32,
            cif in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
        ) {
            let pkt = ControlPacket {
                control_type: ControlType::from_u15(control_type_raw),
                subtype_or_reserved: subtype,
                type_specific,
                timestamp,
                dest_socket_id,
                cif,
            };
            let bytes = pkt.serialize();
            let mut b = budget();
            let back = ControlPacket::parse(&bytes, &mut b).unwrap();
            assert_eq!(back.control_type, pkt.control_type);
            assert_eq!(back.subtype_or_reserved, pkt.subtype_or_reserved);
            assert_eq!(back.type_specific, pkt.type_specific);
            assert_eq!(back.timestamp, pkt.timestamp);
            assert_eq!(back.dest_socket_id, pkt.dest_socket_id);
            assert_eq!(back.cif, pkt.cif);
        }
    }

    #[test]
    fn dispatches_on_the_f_bit() {
        let mut b = budget();
        let data = DataPacket {
            seq_no: 1,
            position: PacketPosition::Single,
            in_order: false,
            key: KeyFlag::Unencrypted,
            retransmitted: false,
            msg_no: 1,
            timestamp: 0,
            dest_socket_id: 0,
            payload: vec![1, 2, 3],
        };
        assert!(matches!(
            SrtPacket::parse(&data.serialize(), &mut b).unwrap(),
            SrtPacket::Data(_)
        ));
        let ctrl = ControlPacket {
            control_type: ControlType::KeepAlive,
            subtype_or_reserved: 0,
            type_specific: 0,
            timestamp: 0,
            dest_socket_id: 0,
            cif: Vec::new(),
        };
        assert!(matches!(
            SrtPacket::parse(&ctrl.serialize(), &mut b).unwrap(),
            SrtPacket::Control(_)
        ));
    }

    #[test]
    fn rejects_truncated_input_rather_than_panicking() {
        let mut b = budget();
        assert!(SrtPacket::parse(&[0u8; 15], &mut b).is_err());
        assert!(SrtPacket::parse(&[], &mut b).is_err());
    }
}
