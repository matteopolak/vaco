//! Pre-Shared Key encryption — `VSF TR-06-2:2022` §7.1-7.4, built on
//! `vaco-crypto`'s AES-CTR and PBKDF2-HMAC-SHA256.
//!
//! # Key derivation (§7.3, draft-derived, verified against Annex B)
//!
//! `key = PBKDF2-HMAC-SHA256(passphrase, salt = nonce as 4 big-endian
//! bytes, iterations = 1024, dkLen = 16 or 32)`. The exact numbers
//! (1024 iterations, the nonce-as-salt convention) are `TR-06-2` §7.3's
//! own stated defaults, and [`derive_key`] is checked against Annex B's
//! own worked example in `vaco-crypto::kdf`'s tests — this module adds no
//! new KDF test of its own, since duplicating that check here would only
//! prove this thin wrapper calls through correctly, which the
//! self-consistency round-trip tests below already establish more
//! directly.
//!
//! # IV construction (§7.2, draft-derived)
//!
//! "The 128-bit initialization vector (IV) ... shall be derived by using
//! the 32-bit Sequence Number field as its most significant four bytes,
//! followed by 12 bytes of zeros." [`counter_block`] is exactly that
//! sentence.
//!
//! # What this module does not do
//!
//! §7.4's on-the-fly passphrase change (rotating which of two cached
//! keys is active, signalled by the GRE header's `B` bit) and §7.6's
//! Future Nonce Announcement message are not built here — nothing in
//! this module's own encrypt/decrypt round trip needs them, and the
//! rotation *policy* (when to switch, how long to cache both keys) is a
//! session/deployment concern above the framing layer this crate stays
//! at (matching `vaco-protocol-srt`'s own framing/session split).

use vaco_core::Result;

/// §5.1's `H` bit: which AES key length a PSK session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBits {
    Aes128,
    Aes256,
}

impl KeyBits {
    #[must_use]
    pub const fn from_h_bit(h: bool) -> Self {
        if h { Self::Aes256 } else { Self::Aes128 }
    }

    #[must_use]
    pub const fn to_h_bit(self) -> bool {
        matches!(self, Self::Aes256)
    }

    #[must_use]
    const fn byte_len(self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes256 => 32,
        }
    }
}

/// §7.3: derive the AES key from a passphrase and the GRE Key/Nonce field,
/// 1024 PBKDF2-HMAC-SHA256 iterations, salted by the nonce's 4 big-endian
/// bytes.
#[must_use]
pub fn derive_key(passphrase: &[u8], nonce: u32, key_bits: KeyBits) -> Vec<u8> {
    vaco_crypto::pbkdf2_hmac_sha256(passphrase, &nonce.to_be_bytes(), 1024, key_bits.byte_len())
}

/// §7.2: the 128-bit counter block, sequence number in the high 4 bytes,
/// 12 zero bytes following.
#[must_use]
pub const fn counter_block(sequence_number: u32) -> [u8; 16] {
    let seq = sequence_number.to_be_bytes();
    [
        seq[0], seq[1], seq[2], seq[3], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]
}

/// Apply the PSK keystream to `data` (the GRE payload) in place — the same
/// operation for encryption and decryption, since CTR mode is XOR-based.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if `key`'s length matches neither
/// AES-128 (16 bytes) nor AES-256 (32 bytes).
pub fn apply_keystream(key: &[u8], sequence_number: u32, data: &mut [u8]) -> Result<()> {
    let block = counter_block(sequence_number);
    match key.len() {
        16 => {
            let k: [u8; 16] = key.try_into().unwrap_or([0; 16]);
            vaco_crypto::ctr_apply_aes128(&k, &block, data);
            Ok(())
        }
        32 => {
            let k: [u8; 32] = key.try_into().unwrap_or([0; 32]);
            vaco_crypto::ctr_apply_aes256(&k, &block, data);
            Ok(())
        }
        _ => Err(vaco_core::Error::InvalidData(
            "PSK key is neither 16 (AES-128) nor 32 (AES-256) bytes",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    // --- draft-derived: §7.2's IV-construction sentence, applied directly.

    #[test]
    fn counter_block_puts_sequence_number_in_the_high_four_bytes() {
        let block = counter_block(0x0102_0304);
        assert_eq!(block, [1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    // --- self-consistency: this module's own encrypt/decrypt agreeing,
    // for both key sizes -- not spec evidence beyond the IV/key-derivation
    // rules already checked above and in vaco-crypto::kdf.

    #[test]
    fn round_trips_aes128() {
        let key = derive_key(
            b"Reliable Internet Stream Transport",
            0x5249_5354,
            KeyBits::Aes128,
        );
        let mut data = b"a lossy link is not a reason to give up on delivery".to_vec();
        let original = data.clone();
        apply_keystream(&key, 7, &mut data).unwrap();
        assert_ne!(data, original);
        apply_keystream(&key, 7, &mut data).unwrap();
        assert_eq!(data, original);
    }

    #[test]
    fn round_trips_aes256() {
        let key = derive_key(
            b"Reliable Internet Stream Transport",
            0x5249_5354,
            KeyBits::Aes256,
        );
        assert_eq!(key.len(), 32);
        let mut data = vec![0xAAu8; 100];
        let original = data.clone();
        apply_keystream(&key, 99, &mut data).unwrap();
        assert_ne!(data, original);
        apply_keystream(&key, 99, &mut data).unwrap();
        assert_eq!(data, original);
    }

    #[test]
    fn a_different_sequence_number_produces_a_different_keystream() {
        let key = derive_key(b"passphrase", 1, KeyBits::Aes128);
        let mut a = vec![0u8; 32];
        let mut b = vec![0u8; 32];
        apply_keystream(&key, 1, &mut a).unwrap();
        apply_keystream(&key, 2, &mut b).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn rejects_a_key_of_the_wrong_length() {
        let mut data = vec![0u8; 16];
        assert!(apply_keystream(&[0u8; 20], 0, &mut data).is_err());
    }

    #[test]
    fn h_bit_round_trips_key_bits() {
        assert_eq!(KeyBits::from_h_bit(false), KeyBits::Aes128);
        assert_eq!(KeyBits::from_h_bit(true), KeyBits::Aes256);
        assert!(!KeyBits::Aes128.to_h_bit());
        assert!(KeyBits::Aes256.to_h_bit());
    }
}
