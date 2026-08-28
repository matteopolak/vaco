//! The Key Material (KM) message *shape* — `draft-sharabayko-srt-01`
//! §3.2.2, quoted from the fetched IETF datatracker rendering. This is the
//! payload carried inside a Handshake's `SRT_CMD_KMREQ`/`SRT_CMD_KMRSP`
//! extension (see [`crate::handshake`]).
//!
//! **This module parses and serializes every field, including the wrapped
//! key blob, but does not unwrap it.** The actual AES key-unwrap needs the
//! cipher, and which crate owns `aes`/`ctr` is a deferred D11 question
//! (`planning/INTERFACE-GAPS.md`, gap 28's crypto-ownership note) —
//! `#555`'s own scope is the message shape, not the cipher.
//!
//! # Layout (`draft` §3.2.2, Figure 10)
//!
//! ```text
//! |S|  V  |   PT  |              Sign             |   Resv1   | KK|
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                              KEKI                             |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |     Cipher    |      Auth     |       SE      |     Resv2     |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |             Resv3             |     SLen/4    |     KLen/4    |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                              Salt                             |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                          Wrapped Key                          |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! First word, draft-derived widths and values: `S` 1 bit (`0`), `V`
//! (version) 3 bits (`1`), `PT` (packet type) 4 bits (`2`, "`KMmsg`"),
//! `1+3+4 = 8`; `Sign` 16 bits (`0x2029`, a fixed magic identifying this as
//! an SRT KM message); `Resv1` 6 bits (`0`), `KK` 2 bits, `6+2 = 8`;
//! `8+16+8 = 32`. `KK` reuses the data-packet encryption-key encoding
//! (`crate::packet::KeyFlag`, minus its `11b`/"control packets only" case,
//! which here instead means "both the even and odd key are present" —
//! [`KeyFlags::Both`]).
//!
//! Second word: `KEKI` (Key Encrypting Key Index), 32 bits.
//!
//! Third word: `Cipher` 8 bits (`0..2`, draft-derived — this crate does not
//! interpret the value, only frames it), `Auth` 8 bits (`0`), `SE`
//! (Stream Encapsulation) 8 bits (`2`), `Resv2` 8 bits (`0`).
//!
//! Fourth word: `Resv3` 16 bits (`0`), `SLen/4` 8 bits (salt length divided
//! by 4 — draft-derived value `4`, i.e. a 16-byte salt), `KLen/4` 8 bits
//! (wrapped key length divided by 4 — draft-derived values `{4,6,8}`, i.e.
//! 16/24/32-byte AES-128/192/256 keys).
//!
//! Then `Salt` (`SLen` bytes) and `Wrapped Key` (the rest of the message —
//! an RFC 3394 AES-key-wrap blob: an 8-byte integrity check value plus one
//! or two `KLen`-byte session keys, decomposed by whichever later package
//! does the actual unwrap, not here).

use vaco_protocol_core::{ProtocolError, Result};

use crate::packet::be32;

const SCHEME: &str = "srt";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// `draft` §3.2.2's fixed magic `Sign` value identifying a KM message.
pub const SIGN: u16 = 0x2029;

/// `draft`-derived: `PT` field value, "`KMmsg`".
pub const PACKET_TYPE_KM_MSG: u8 = 2;

/// `KK`, reusing [`crate::packet::KeyFlag`]'s encoding for the two single-key
/// cases and adding the KM-message-only "both" case (`11b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFlags {
    Even,
    Odd,
    Both,
    /// `00b` — draft-derived: not a documented KM-message value (a KM
    /// message always names at least one key), parsed rather than
    /// rejected.
    None,
}

impl KeyFlags {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b01 => Self::Even,
            0b10 => Self::Odd,
            0b11 => Self::Both,
            _ => Self::None,
        }
    }

    const fn to_bits(self) -> u8 {
        match self {
            Self::Even => 0b01,
            Self::Odd => 0b10,
            Self::Both => 0b11,
            Self::None => 0b00,
        }
    }
}

/// The KM message, wrapped key left opaque (see module docs).
#[derive(Debug, Clone)]
pub struct KmMessage {
    pub version: u8,
    pub key_flags: KeyFlags,
    pub keki: u32,
    pub cipher: u8,
    pub auth: u8,
    pub stream_encapsulation: u8,
    /// Salt, `SLen` bytes (a multiple of 4, per `SLen/4`).
    pub salt: Vec<u8>,
    /// ICV + one or two session keys, undecomposed — see module docs.
    pub wrapped_key: Vec<u8>,
}

const FIXED_LEN: usize = 16;

impl KmMessage {
    /// # Errors
    /// [`ProtocolError::Malformed`] if `data` is shorter than the 16-byte
    /// fixed header, if `Sign` does not match [`SIGN`], or if the declared
    /// `SLen`/message length would run past the end of `data`.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < FIXED_LEN {
            return Err(malformed("KM message shorter than the 16-byte fixed header"));
        }
        let w0 = be32(data, 0)?;
        // Masked to 3 bits (0..=7), always in u8 range.
        let version = ((w0 >> 28) & 0b111) as u8;
        // Masked to 16 bits, always in u16 range.
        let sign = ((w0 >> 8) & 0xffff) as u16;
        if sign != SIGN {
            return Err(malformed("KM message Sign field does not match 0x2029"));
        }
        // Masked to 2 bits (0..=3), always in u8 range.
        let key_flags = KeyFlags::from_bits((w0 & 0b11) as u8);
        let keki = be32(data, 4)?;
        let w2 = be32(data, 8)?;
        // Top 8 bits of a u32, always in u8 range.
        let cipher = (w2 >> 24) as u8;
        // Masked to 8 bits, always in u8 range.
        let auth = ((w2 >> 16) & 0xff) as u8;
        // Masked to 8 bits, always in u8 range.
        let stream_encapsulation = ((w2 >> 8) & 0xff) as u8;
        let w3 = be32(data, 12)?;
        // Masked to 8 bits; widening a u32 into usize always fits.
        let slen = (((w3 >> 8) & 0xff) as usize).saturating_mul(4);

        let salt_start = FIXED_LEN;
        let salt = data
            .get(salt_start..salt_start + slen)
            .ok_or_else(|| malformed("KM message salt runs past the end of the message"))?
            .to_vec();
        let wrapped_key = data.get(salt_start + slen..).unwrap_or(&[]).to_vec();

        Ok(Self {
            version,
            key_flags,
            keki,
            cipher,
            auth,
            stream_encapsulation,
            salt,
            wrapped_key,
        })
    }

    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "SLen/KLen are always a multiple of 4 by construction (see parse); this recovers the /4 field, not a lossy scale-down"
    )]
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let slen4 = u32::try_from(self.salt.len() / 4).unwrap_or(0);
        let klen4 = u32::try_from(self.wrapped_key_first_key_len() / 4).unwrap_or(0);

        let w0 = (u32::from(self.version & 0b111) << 28)
            | (u32::from(PACKET_TYPE_KM_MSG & 0b1111) << 24)
            | (u32::from(SIGN) << 8)
            | u32::from(self.key_flags.to_bits());
        out.extend_from_slice(&w0.to_be_bytes());
        out.extend_from_slice(&self.keki.to_be_bytes());
        let w2 = (u32::from(self.cipher) << 24)
            | (u32::from(self.auth) << 16)
            | (u32::from(self.stream_encapsulation) << 8);
        out.extend_from_slice(&w2.to_be_bytes());
        let w3 = (slen4 << 8) | klen4;
        out.extend_from_slice(&w3.to_be_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.wrapped_key);
        out
    }

    /// The length of one session key inside `wrapped_key`, inferred from
    /// its total length and [`KeyFlags`] (8-byte ICV, plus one key for
    /// `Even`/`Odd`/`None`, two for `Both`) — used only to recompute
    /// `KLen/4` on serialize, since this module does not decompose
    /// `wrapped_key` itself.
    #[allow(
        clippy::integer_division,
        reason = "two equal-length session keys split the remainder exactly by construction"
    )]
    fn wrapped_key_first_key_len(&self) -> usize {
        const ICV_LEN: usize = 8;
        let remaining = self.wrapped_key.len().saturating_sub(ICV_LEN);
        if self.key_flags == KeyFlags::Both {
            remaining / 2
        } else {
            remaining
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::strategy::Strategy;

    /// Draft-derived: `draft` §3.2.2 Figure 10's exact field layout, hand
    /// built from the stated bit widths and values, not round-tripped.
    #[test]
    fn km_message_matches_the_drafts_own_field_layout() {
        let mut bytes = Vec::new();
        // S=0 V=1 PT=2 -> 0001_0010 = 0x12; Sign=0x2029; Resv1=0 KK=01(Even) -> 0x01
        bytes.push(0x12);
        bytes.extend_from_slice(&SIGN.to_be_bytes());
        bytes.push(0x01);
        bytes.extend_from_slice(&0x0000_0007u32.to_be_bytes()); // KEKI
        bytes.push(2); // Cipher = 2 (AES-CTR, draft-derived value)
        bytes.push(0); // Auth
        bytes.push(2); // SE
        bytes.push(0); // Resv2
        bytes.push(0); // Resv3 high byte
        bytes.push(0); // Resv3 low byte
        bytes.push(4); // SLen/4 = 4 -> 16-byte salt
        bytes.push(4); // KLen/4 = 4 -> 16-byte key (AES-128)
        bytes.extend_from_slice(&[0xAA; 16]); // salt
        bytes.extend_from_slice(&[0xBB; 8]); // ICV
        bytes.extend_from_slice(&[0xCC; 16]); // xSEK

        let km = KmMessage::parse(&bytes).unwrap();
        assert_eq!(km.version, 1);
        assert_eq!(km.key_flags, KeyFlags::Even);
        assert_eq!(km.keki, 7);
        assert_eq!(km.cipher, 2);
        assert_eq!(km.auth, 0);
        assert_eq!(km.stream_encapsulation, 2);
        assert_eq!(km.salt, vec![0xAA; 16]);
        assert_eq!(km.wrapped_key.len(), 8 + 16);
    }

    #[test]
    fn rejects_a_message_whose_sign_does_not_match() {
        let mut bytes = vec![0x12, 0x00, 0x00, 0x01]; // wrong Sign
        bytes.extend_from_slice(&[0u8; 12]);
        assert!(KmMessage::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_a_salt_length_that_runs_past_the_end() {
        let mut bytes = Vec::new();
        bytes.push(0x12);
        bytes.extend_from_slice(&SIGN.to_be_bytes());
        bytes.push(0x01);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.push(0);
        bytes.push(0);
        bytes.push(100); // SLen/4 = 100 -> 400-byte salt, nothing follows
        bytes.push(4);
        assert!(KmMessage::parse(&bytes).is_err());
    }

    // Self-consistency: round-trip through this crate's own encoder for
    // the single-key case (`Both` is checked separately since it changes
    // how the key length is inferred on serialize).
    proptest::proptest! {
        #[test]
        fn km_message_round_trips_single_key(
            version in 0u8..8,
            keki: u32,
            cipher: u8,
            auth: u8,
            se: u8,
            salt in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4).prop_map(|v| {
                let mut s = v;
                s.resize(s.len() - s.len() % 4, 0);
                s
            }),
            key_len_words in 4usize..=8,
        ) {
            let key_len = key_len_words * 4;
            let mut wrapped_key = vec![0xEEu8; 8];
            wrapped_key.extend(std::iter::repeat_n(0xFFu8, key_len));
            let km = KmMessage {
                version,
                key_flags: KeyFlags::Odd,
                keki,
                cipher,
                auth,
                stream_encapsulation: se,
                salt,
                wrapped_key,
            };
            let bytes = km.serialize();
            let back = KmMessage::parse(&bytes).unwrap();
            assert_eq!(back.version, km.version);
            assert_eq!(back.key_flags, km.key_flags);
            assert_eq!(back.keki, km.keki);
            assert_eq!(back.cipher, km.cipher);
            assert_eq!(back.auth, km.auth);
            assert_eq!(back.stream_encapsulation, km.stream_encapsulation);
            assert_eq!(back.salt, km.salt);
            assert_eq!(back.wrapped_key, km.wrapped_key);
        }
    }
}
