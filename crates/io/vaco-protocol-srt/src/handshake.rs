//! The Handshake control packet's Control Information Field (CIF), and its
//! HSREQ/HSRSP extension blocks — `draft-sharabayko-srt-01` §3.2.1 and
//! §3.2.1.1. See [`HandshakeCif`] for the fixed CIF layout,
//! [`HandshakeType`] for the Handshake Type encoding, and [`HsReqBody`]
//! for the HSREQ/HSRSP extension.
//!
//! `Extension Length` is "the length of the Extension Contents field in
//! four-byte blocks" — every extension body is therefore a multiple of 4
//! bytes by construction, checked in [`parse_extensions`] rather than
//! assumed.

use vaco_protocol_core::{ProtocolError, Result};

use crate::packet::be32;

const SCHEME: &str = "srt";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

pub const HS_INDUCTION: u32 = 0x0000_0001;
pub const HS_WAVEAHAND: u32 = 0x0000_0000;
pub const HS_DONE: u32 = 0xffff_fffd;
pub const HS_AGREEMENT: u32 = 0xffff_fffe;
pub const HS_CONCLUSION: u32 = 0xffff_ffff;

/// `draft` Table 7 — draft-derived rejection reason codes, sent as the
/// Handshake Type field's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectReason {
    Unknown,
    System,
    Peer,
    Resource,
    Rogue,
    Backlog,
    Ipe,
    Close,
    Version,
    RdvCookie,
    BadSecret,
    Unsecure,
    MessageApi,
    Congestion,
    Filter,
    Group,
}

impl RejectReason {
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            1000 => Self::Unknown,
            1001 => Self::System,
            1002 => Self::Peer,
            1003 => Self::Resource,
            1004 => Self::Rogue,
            1005 => Self::Backlog,
            1006 => Self::Ipe,
            1007 => Self::Close,
            1008 => Self::Version,
            1009 => Self::RdvCookie,
            1010 => Self::BadSecret,
            1011 => Self::Unsecure,
            1012 => Self::MessageApi,
            1013 => Self::Congestion,
            1014 => Self::Filter,
            1015 => Self::Group,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn to_code(self) -> u32 {
        match self {
            Self::Unknown => 1000,
            Self::System => 1001,
            Self::Peer => 1002,
            Self::Resource => 1003,
            Self::Rogue => 1004,
            Self::Backlog => 1005,
            Self::Ipe => 1006,
            Self::Close => 1007,
            Self::Version => 1008,
            Self::RdvCookie => 1009,
            Self::BadSecret => 1010,
            Self::Unsecure => 1011,
            Self::MessageApi => 1012,
            Self::Congestion => 1013,
            Self::Filter => 1014,
            Self::Group => 1015,
        }
    }
}

/// A Handshake CIF's `Handshake Type` field, covering both the five named
/// states and the rejection-reason encoding (`draft` Table 4,
/// draft-derived):
///
/// | Value | Meaning |
/// |---|---|
/// | `0x00000000` | WAVEAHAND (rendezvous induction) |
/// | `0x00000001` | INDUCTION |
/// | `0xFFFFFFFD` | DONE |
/// | `0xFFFFFFFE` | AGREEMENT |
/// | `0xFFFFFFFF` | CONCLUSION |
/// | `1000..=1015` | a rejection reason (`draft` Table 7) sent *as* the
/// Handshake Type field of a otherwise-CONCLUSION-shaped response |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeType {
    WaveAHand,
    Induction,
    Conclusion,
    Agreement,
    Done,
    Reject(RejectReason),
    /// A value this crate does not recognise — parsed, not rejected.
    Other(u32),
}

impl HandshakeType {
    #[must_use]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            HS_WAVEAHAND => Self::WaveAHand,
            HS_INDUCTION => Self::Induction,
            HS_CONCLUSION => Self::Conclusion,
            HS_AGREEMENT => Self::Agreement,
            HS_DONE => Self::Done,
            other => match RejectReason::from_code(other) {
                Some(r) => Self::Reject(r),
                None => Self::Other(other),
            },
        }
    }

    #[must_use]
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::WaveAHand => HS_WAVEAHAND,
            Self::Induction => HS_INDUCTION,
            Self::Conclusion => HS_CONCLUSION,
            Self::Agreement => HS_AGREEMENT,
            Self::Done => HS_DONE,
            Self::Reject(r) => r.to_code(),
            Self::Other(v) => v,
        }
    }
}

/// `draft` §3.2.1: "Specifies cipher family (0=none, 2=AES-128, 3=AES-192,
/// 4=AES-256)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionField {
    None,
    Aes128,
    Aes192,
    Aes256,
    Other(u16),
}

impl EncryptionField {
    #[must_use]
    pub const fn from_u16(v: u16) -> Self {
        match v {
            0 => Self::None,
            2 => Self::Aes128,
            3 => Self::Aes192,
            4 => Self::Aes256,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub const fn to_u16(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Aes128 => 2,
            Self::Aes192 => 3,
            Self::Aes256 => 4,
            Self::Other(v) => v,
        }
    }
}

/// One raw extension block: type, and its already-length-checked contents
/// (a whole number of 4-byte units, per `draft`'s own `Extension Length`
/// definition).
#[derive(Debug, Clone)]
pub struct Extension {
    pub ext_type: u16,
    pub contents: Vec<u8>,
}

pub const SRT_CMD_HSREQ: u16 = 1;
pub const SRT_CMD_HSRSP: u16 = 2;
pub const SRT_CMD_KMREQ: u16 = 3;
pub const SRT_CMD_KMRSP: u16 = 4;
pub const SRT_CMD_SID: u16 = 5;
pub const SRT_CMD_CONGESTION: u16 = 6;
pub const SRT_CMD_FILTER: u16 = 7;
pub const SRT_CMD_GROUP: u16 = 8;

/// `draft` Table 6 — draft-derived `SRT Flags` bits.
pub mod srt_flags {
    pub const TSBPDSND: u32 = 0x0000_0001;
    pub const TSBPDRCV: u32 = 0x0000_0002;
    pub const CRYPT: u32 = 0x0000_0004;
    pub const TLPKTDROP: u32 = 0x0000_0008;
    pub const PERIODICNAK: u32 = 0x0000_0010;
    pub const REXMITFLG: u32 = 0x0000_0020;
    pub const STREAM: u32 = 0x0000_0040;
    pub const PACKET_FILTER: u32 = 0x0000_0080;
}

/// The fixed 48-byte body of a Handshake CIF, before any extensions
/// (`draft` §3.2.1, Figure 5):
///
/// ```text
/// |                            Version                            |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |        Encryption Field       |        Extension Field        |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                 Initial Packet Sequence Number                |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                 Maximum Transmission Unit Size                |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                    Maximum Flow Window Size                   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         Handshake Type                        |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         SRT Socket ID                         |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                           SYN Cookie                          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                        Peer IP Address                        |  (128 bits)
/// +=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+
/// |         Extension Type        |        Extension Length       |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                       Extension Contents                      |
/// +=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+=+
/// ```
#[derive(Debug, Clone, Copy)]
pub struct HandshakeCif {
    pub version: u32,
    pub encryption: EncryptionField,
    /// Raw 16 bits: an extension-presence bitmask/magic value
    /// (`draft` names `0x4A17` as the `HSv5` magic and otherwise a bitmask of
    /// which extension families are attached — `HSREQ`/`KMREQ`/`CONFIG`).
    pub extension_field: u16,
    pub initial_seq_no: u32,
    pub mtu: u32,
    pub max_flow_window: u32,
    pub handshake_type: HandshakeType,
    pub socket_id: u32,
    pub syn_cookie: u32,
    /// 128 bits, stored as the four 32-bit words `draft`'s figure shows
    /// (an IPv4 address occupies the first word, the rest zero — this
    /// crate does not yet interpret the address, only frames it).
    pub peer_ip: [u32; 4],
}

const FIXED_CIF_LEN: usize = 48;

impl HandshakeCif {
    /// Parses the fixed 48-byte body only; extensions (if any) start at the
    /// returned byte offset within `data` and are read separately via
    /// [`parse_extensions`], since which extensions are expected depends on
    /// `extension_field` and the handshake mode, which this module does not
    /// decide.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is shorter than 48 bytes.
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        if data.len() < FIXED_CIF_LEN {
            return Err(malformed("handshake CIF shorter than 48 bytes"));
        }
        let version = be32(data, 0)?;
        let w1 = be32(data, 4)?;
        // `w1 >> 16` is the top 16 bits of a u32, always in u16 range.
        let encryption = EncryptionField::from_u16((w1 >> 16) as u16);
        // Masked to 16 bits, always in u16 range.
        let extension_field = (w1 & 0xffff) as u16;
        let initial_seq_no = be32(data, 8)?;
        let mtu = be32(data, 12)?;
        let max_flow_window = be32(data, 16)?;
        let handshake_type = HandshakeType::from_u32(be32(data, 20)?);
        let socket_id = be32(data, 24)?;
        let syn_cookie = be32(data, 28)?;
        let peer_ip = [
            be32(data, 32)?,
            be32(data, 36)?,
            be32(data, 40)?,
            be32(data, 44)?,
        ];
        Ok((
            Self {
                version,
                encryption,
                extension_field,
                initial_seq_no,
                mtu,
                max_flow_window,
                handshake_type,
                socket_id,
                syn_cookie,
                peer_ip,
            },
            FIXED_CIF_LEN,
        ))
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.version.to_be_bytes());
        let w1 = (u32::from(self.encryption.to_u16()) << 16) | u32::from(self.extension_field);
        out.extend_from_slice(&w1.to_be_bytes());
        out.extend_from_slice(&self.initial_seq_no.to_be_bytes());
        out.extend_from_slice(&self.mtu.to_be_bytes());
        out.extend_from_slice(&self.max_flow_window.to_be_bytes());
        out.extend_from_slice(&self.handshake_type.to_u32().to_be_bytes());
        out.extend_from_slice(&self.socket_id.to_be_bytes());
        out.extend_from_slice(&self.syn_cookie.to_be_bytes());
        for word in self.peer_ip {
            out.extend_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// Parse every extension block in `data`, in order.
///
/// # Errors
/// [`ProtocolError::Malformed`] if any extension's declared length runs
/// past the end of `data`. A trailing partial block (fewer than 4 bytes
/// left, all zero) is not an error — some peers pad; only a length that
/// claims more than is present is rejected.
pub fn parse_extensions(data: &[u8]) -> Result<Vec<Extension>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        // Top 16 bits of a u32, always in u16 range.
        let ext_type = (be32(data, pos)? >> 16) as u16;
        let ext_len_blocks = be32(data, pos)? & 0xffff;
        let ext_len_bytes = (ext_len_blocks as usize).saturating_mul(4);
        let contents_start = pos + 4;
        let contents = data
            .get(contents_start..contents_start + ext_len_bytes)
            .ok_or_else(|| malformed("handshake extension length runs past the end of the CIF"))?
            .to_vec();
        out.push(Extension { ext_type, contents });
        pos = contents_start + ext_len_bytes;
    }
    Ok(out)
}

/// Serialize one extension block: type, length (in 4-byte units, rounded
/// up — the caller is responsible for contents already being a multiple of
/// 4 bytes; this pads with zero otherwise rather than silently truncating).
#[must_use]
pub fn serialize_extension(ext: &Extension) -> Vec<u8> {
    let mut out = Vec::new();
    let blocks = ext.contents.len().div_ceil(4);
    let header =
        (u32::from(ext.ext_type) << 16) | (u32::try_from(blocks).unwrap_or(u32::MAX) & 0xffff);
    out.extend_from_slice(&header.to_be_bytes());
    out.extend_from_slice(&ext.contents);
    let padding = blocks.saturating_mul(4).saturating_sub(ext.contents.len());
    out.resize(out.len() + padding, 0);
    out
}

/// The HSREQ/HSRSP extension body — `draft` §3.2.1.1.1, Figure 6:
///
/// ```text
/// |                          SRT Version                          |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                           SRT Flags                           |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |      Receiver TSBPD Delay     |       Sender TSBPD Delay      |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// 12 bytes = 3 four-byte blocks, matching `SRT_CMD_HSREQ`/`SRT_CMD_HSRSP`'s
/// own `Extension Length` of 3. `SRT Flags` bit values, `draft` Table 6:
/// `TSBPDSND 0x01`, `TSBPDRCV 0x02`, `CRYPT 0x04`, `TLPKTDROP 0x08`,
/// `PERIODICNAK 0x10`, `REXMITFLG 0x20`, `STREAM 0x40`, `PACKET_FILTER
/// 0x80`.
#[derive(Debug, Clone, Copy)]
pub struct HsReqBody {
    pub srt_version: u32,
    pub srt_flags: u32,
    pub receiver_tsbpd_delay: u16,
    pub sender_tsbpd_delay: u16,
}

impl HsReqBody {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `contents` is shorter than 12 bytes.
    pub fn parse(contents: &[u8]) -> Result<Self> {
        if contents.len() < 12 {
            return Err(malformed("HSREQ/HSRSP extension shorter than 12 bytes"));
        }
        let srt_version = be32(contents, 0)?;
        let srt_flags = be32(contents, 4)?;
        let w2 = be32(contents, 8)?;
        // Top 16 bits of a u32, always in u16 range.
        let receiver_tsbpd_delay = (w2 >> 16) as u16;
        // Masked to 16 bits, always in u16 range.
        let sender_tsbpd_delay = (w2 & 0xffff) as u16;
        Ok(Self {
            srt_version,
            srt_flags,
            receiver_tsbpd_delay,
            sender_tsbpd_delay,
        })
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.srt_version.to_be_bytes());
        out.extend_from_slice(&self.srt_flags.to_be_bytes());
        let w2 = (u32::from(self.receiver_tsbpd_delay) << 16) | u32::from(self.sender_tsbpd_delay);
        out.extend_from_slice(&w2.to_be_bytes());
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Draft-derived: `draft` Table 4's exact numeric constants.
    #[test]
    fn handshake_type_constants_match_the_draft_table() {
        assert_eq!(HandshakeType::WaveAHand.to_u32(), 0x0000_0000);
        assert_eq!(HandshakeType::Induction.to_u32(), 0x0000_0001);
        assert_eq!(HandshakeType::Done.to_u32(), 0xffff_fffd);
        assert_eq!(HandshakeType::Agreement.to_u32(), 0xffff_fffe);
        assert_eq!(HandshakeType::Conclusion.to_u32(), 0xffff_ffff);
    }

    /// Draft-derived: `draft` Table 7's rejection reason codes.
    #[test]
    fn reject_reason_codes_match_the_draft_table() {
        assert_eq!(RejectReason::Unknown.to_code(), 1000);
        assert_eq!(RejectReason::RdvCookie.to_code(), 1009);
        assert_eq!(RejectReason::Group.to_code(), 1015);
        assert_eq!(
            HandshakeType::from_u32(1009),
            HandshakeType::Reject(RejectReason::RdvCookie)
        );
    }

    /// Draft-derived: `draft` §3.2.1's encryption field values.
    #[test]
    fn encryption_field_values_match_the_draft() {
        assert_eq!(EncryptionField::None.to_u16(), 0);
        assert_eq!(EncryptionField::Aes128.to_u16(), 2);
        assert_eq!(EncryptionField::Aes192.to_u16(), 3);
        assert_eq!(EncryptionField::Aes256.to_u16(), 4);
    }

    /// Draft-derived: `draft` §3.2.1.1's flag bitmask values.
    #[test]
    fn srt_flag_bits_match_the_draft_table() {
        assert_eq!(srt_flags::TSBPDSND, 0x01);
        assert_eq!(srt_flags::TSBPDRCV, 0x02);
        assert_eq!(srt_flags::CRYPT, 0x04);
        assert_eq!(srt_flags::TLPKTDROP, 0x08);
        assert_eq!(srt_flags::PERIODICNAK, 0x10);
        assert_eq!(srt_flags::REXMITFLG, 0x20);
        assert_eq!(srt_flags::STREAM, 0x40);
        assert_eq!(srt_flags::PACKET_FILTER, 0x80);
    }

    /// Draft-derived: hand-built 48-byte CIF matching `draft` Figure 5's
    /// field order exactly, checked field by field.
    #[test]
    fn handshake_cif_matches_the_drafts_own_field_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5u32.to_be_bytes()); // version 5
        bytes.extend_from_slice(&[0x00, 0x02, 0x4a, 0x17]); // encryption=Aes128, ext_field=0x4A17
        bytes.extend_from_slice(&1000u32.to_be_bytes()); // initial seq no
        bytes.extend_from_slice(&1500u32.to_be_bytes()); // mtu
        bytes.extend_from_slice(&8192u32.to_be_bytes()); // max flow window
        bytes.extend_from_slice(&HS_CONCLUSION.to_be_bytes());
        bytes.extend_from_slice(&0xAABB_CCDDu32.to_be_bytes()); // socket id
        bytes.extend_from_slice(&0x1122_3344u32.to_be_bytes()); // syn cookie
        bytes.extend_from_slice(&[0u8; 16]); // peer ip, all zero

        let (cif, consumed) = HandshakeCif::parse(&bytes).unwrap();
        assert_eq!(consumed, 48);
        assert_eq!(cif.version, 5);
        assert_eq!(cif.encryption, EncryptionField::Aes128);
        assert_eq!(cif.extension_field, 0x4a17);
        assert_eq!(cif.initial_seq_no, 1000);
        assert_eq!(cif.mtu, 1500);
        assert_eq!(cif.max_flow_window, 8192);
        assert_eq!(cif.handshake_type, HandshakeType::Conclusion);
        assert_eq!(cif.socket_id, 0xAABB_CCDD);
        assert_eq!(cif.syn_cookie, 0x1122_3344);
        assert_eq!(cif.peer_ip, [0, 0, 0, 0]);
    }

    // Self-consistency: round-trip through this crate's own serializer.
    proptest::proptest! {
        #[test]
        fn handshake_cif_round_trips(
            version: u32,
            enc in 0u16..5,
            extension_field: u16,
            initial_seq_no: u32,
            mtu: u32,
            max_flow_window: u32,
            socket_id: u32,
            syn_cookie: u32,
            peer_ip in proptest::collection::vec(proptest::prelude::any::<u32>(), 4..=4),
        ) {
            let cif = HandshakeCif {
                version,
                encryption: EncryptionField::from_u16(enc),
                extension_field,
                initial_seq_no,
                mtu,
                max_flow_window,
                handshake_type: HandshakeType::Conclusion,
                socket_id,
                syn_cookie,
                peer_ip: [peer_ip[0], peer_ip[1], peer_ip[2], peer_ip[3]],
            };
            let bytes = cif.serialize();
            let (back, consumed) = HandshakeCif::parse(&bytes).unwrap();
            assert_eq!(consumed, 48);
            assert_eq!(back.version, cif.version);
            assert_eq!(back.encryption, cif.encryption);
            assert_eq!(back.extension_field, cif.extension_field);
            assert_eq!(back.initial_seq_no, cif.initial_seq_no);
            assert_eq!(back.mtu, cif.mtu);
            assert_eq!(back.max_flow_window, cif.max_flow_window);
            assert_eq!(back.socket_id, cif.socket_id);
            assert_eq!(back.syn_cookie, cif.syn_cookie);
            assert_eq!(back.peer_ip, cif.peer_ip);
        }
    }

    /// Draft-derived: `draft` §3.2.1.1.1 Figure 6's exact HSREQ/HSRSP body.
    #[test]
    fn hsreq_body_matches_the_drafts_own_field_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0001_0402u32.to_be_bytes()); // SRT Version 1.4.2
        bytes.extend_from_slice(
            &(srt_flags::TSBPDSND | srt_flags::TSBPDRCV | srt_flags::CRYPT).to_be_bytes(),
        );
        bytes.extend_from_slice(&[0x00, 0x78, 0x00, 0x78]); // 120ms/120ms delay

        let hsreq = HsReqBody::parse(&bytes).unwrap();
        assert_eq!(hsreq.srt_version, 0x0001_0402);
        assert_eq!(
            hsreq.srt_flags,
            srt_flags::TSBPDSND | srt_flags::TSBPDRCV | srt_flags::CRYPT
        );
        assert_eq!(hsreq.receiver_tsbpd_delay, 120);
        assert_eq!(hsreq.sender_tsbpd_delay, 120);
        assert_eq!(hsreq.serialize(), bytes);
    }

    /// Draft-derived: extension type constants from `draft` Table 5.
    #[test]
    fn extension_type_constants_match_the_draft_table() {
        assert_eq!(SRT_CMD_HSREQ, 1);
        assert_eq!(SRT_CMD_HSRSP, 2);
        assert_eq!(SRT_CMD_KMREQ, 3);
        assert_eq!(SRT_CMD_KMRSP, 4);
        assert_eq!(SRT_CMD_SID, 5);
        assert_eq!(SRT_CMD_CONGESTION, 6);
        assert_eq!(SRT_CMD_FILTER, 7);
        assert_eq!(SRT_CMD_GROUP, 8);
    }

    #[test]
    fn parses_two_back_to_back_extensions() {
        let hsreq = HsReqBody {
            srt_version: 0x0001_0500,
            srt_flags: srt_flags::TSBPDSND,
            receiver_tsbpd_delay: 120,
            sender_tsbpd_delay: 120,
        }
        .serialize();
        let ext1 = serialize_extension(&Extension {
            ext_type: SRT_CMD_HSREQ,
            contents: hsreq,
        });
        let ext2 = serialize_extension(&Extension {
            ext_type: SRT_CMD_SID,
            contents: b"abcd".to_vec(),
        });
        let mut all = ext1.clone();
        all.extend_from_slice(&ext2);

        let parsed = parse_extensions(&all).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].ext_type, SRT_CMD_HSREQ);
        assert_eq!(parsed[1].ext_type, SRT_CMD_SID);
        assert_eq!(parsed[1].contents, b"abcd");
    }

    #[test]
    fn rejects_an_extension_whose_length_runs_past_the_end() {
        // type=HSREQ, length=100 blocks (400 bytes), but no contents follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((u32::from(SRT_CMD_HSREQ) << 16) | 0x0064).to_be_bytes());
        assert!(parse_extensions(&bytes).is_err());
    }
}
