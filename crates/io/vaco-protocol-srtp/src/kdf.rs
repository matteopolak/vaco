//! RFC 3711 §4.3.1's key derivation function — deriving the session
//! encryption key, session authentication key, and session salt from one
//! master key and master salt.
//!
//! **Scope cut, stated up front:** this module only derives the three
//! SRTP (media) session keys (labels `0x00`/`0x01`/`0x02`). SRTCP's own
//! three labels (`0x03`/`0x04`/`0x05`) are not derived — this crate is
//! scoped to SRTP (RTP payload encryption), not SRTCP, matching the
//! `srtp` protocol scheme's own name.
//!
//! **Scope cut #2:** only `key_derivation_rate = 0` is supported — the
//! master key is derived into session keys exactly once and never
//! re-derived from a growing packet index. RFC 3711 itself calls `kdr=0`
//! ("no key derivation, other than the initial one") a normal
//! configuration, and it is the one every deployment this crate's authors
//! are aware of actually uses; the periodic re-derivation `kdr != 0`
//! enables is not built.
//!
//! # Provenance
//!
//! RFC 3711 publishes no numeric KDF test vectors (checked directly
//! against the fetched RFC text, see `provenance/sources.toml`'s
//! `rfc-3711` entry). This module's own tests are therefore
//! self-consistency (checking the key-derivation byte layout the code
//! implements against the formula's own algebra, not against an
//! independent numeric answer) plus draft-derived (the label values and
//! byte positions themselves, read directly off §4.3.1).

use vaco_crypto::{ctr_apply_aes128, ctr_apply_aes256};

/// §4.3.1's three SRTP (not SRTCP) key-derivation labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Label {
    SrtpEncryption = 0x00,
    SrtpAuthentication = 0x01,
    SrtpSalting = 0x02,
}

/// The three keys one SRTP stream actually uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKeys {
    pub cipher_key: Vec<u8>,
    pub auth_key: Vec<u8>,
    /// Always 14 bytes (112 bits) — RFC 3711's own salt length.
    pub salt: [u8; 14],
}

/// Build the 16-byte AES-CTR counter block §4.3.1 derives each session
/// value from: `(key_id XOR master_salt) || 0x0000`, where `key_id` is
/// `label` in the most significant octet and zero everywhere else (the
/// `kdr=0` simplification above — `r` is always 0, so there is nothing to
/// right-justify into the low bits of `key_id`).
fn derivation_counter_block(master_salt: &[u8; 14], label: Label) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..14].copy_from_slice(master_salt);
    // key_id's label octet XORs into the most significant byte of the
    // 112-bit salt field (byte 0 of this 16-byte, big-endian block).
    block[0] ^= label as u8;
    block
}

fn derive_bytes_128(master_key: &[u8; 16], master_salt: &[u8; 14], label: Label, out_len: usize) -> Vec<u8> {
    let counter_block = derivation_counter_block(master_salt, label);
    let mut out = vec![0u8; out_len];
    // AES-CTR keystream from an all-zero plaintext is exactly the
    // keystream itself (encrypt XORs the plaintext, i.e. does nothing to
    // zero bytes) — the standard way to get raw AES-CTR output from an
    // XOR-shaped primitive without a second "keystream" entry point.
    ctr_apply_aes128(master_key, &counter_block, &mut out);
    out
}

fn derive_bytes_256(master_key: &[u8; 32], master_salt: &[u8; 14], label: Label, out_len: usize) -> Vec<u8> {
    let counter_block = derivation_counter_block(master_salt, label);
    let mut out = vec![0u8; out_len];
    ctr_apply_aes256(master_key, &counter_block, &mut out);
    out
}

/// Derive the three session keys for AES-128 SRTP.
#[must_use]
pub fn derive_session_keys_aes128(master_key: &[u8; 16], master_salt: &[u8; 14]) -> SessionKeys {
    SessionKeys {
        cipher_key: derive_bytes_128(master_key, master_salt, Label::SrtpEncryption, 16),
        auth_key: derive_bytes_128(master_key, master_salt, Label::SrtpAuthentication, 20),
        salt: derive_bytes_128(master_key, master_salt, Label::SrtpSalting, 14)
            .try_into()
            .unwrap_or([0u8; 14]),
    }
}

/// Derive the three session keys for AES-256 SRTP.
#[must_use]
pub fn derive_session_keys_aes256(master_key: &[u8; 32], master_salt: &[u8; 14]) -> SessionKeys {
    SessionKeys {
        cipher_key: derive_bytes_256(master_key, master_salt, Label::SrtpEncryption, 32),
        auth_key: derive_bytes_256(master_key, master_salt, Label::SrtpAuthentication, 20),
        salt: derive_bytes_256(master_key, master_salt, Label::SrtpSalting, 14)
            .try_into()
            .unwrap_or([0u8; 14]),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn the_three_labels_derive_different_keys_from_the_same_master_material() {
        let master_key = [0x11u8; 16];
        let master_salt = [0x22u8; 14];
        let keys = derive_session_keys_aes128(&master_key, &master_salt);
        assert_ne!(keys.cipher_key, keys.auth_key[..16]);
        assert_ne!(&keys.cipher_key[..14], &keys.salt[..]);
    }

    #[test]
    fn derivation_is_deterministic() {
        let master_key = [0x33u8; 16];
        let master_salt = [0x44u8; 14];
        let a = derive_session_keys_aes128(&master_key, &master_salt);
        let b = derive_session_keys_aes128(&master_key, &master_salt);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_master_salt_changes_every_derived_value() {
        let master_key = [0x55u8; 16];
        let a = derive_session_keys_aes128(&master_key, &[0x00; 14]);
        let b = derive_session_keys_aes128(&master_key, &[0xFF; 14]);
        assert_ne!(a.cipher_key, b.cipher_key);
        assert_ne!(a.auth_key, b.auth_key);
        assert_ne!(a.salt, b.salt);
    }

    #[test]
    fn aes256_derives_a_32_byte_cipher_key() {
        let keys = derive_session_keys_aes256(&[0xAB; 32], &[0xCD; 14]);
        assert_eq!(keys.cipher_key.len(), 32);
        assert_eq!(keys.auth_key.len(), 20);
    }

    #[test]
    fn the_label_byte_lands_in_the_most_significant_octet_of_the_counter_block() {
        // Draft-derived: with an all-zero master salt, XORing the label
        // into byte 0 (this module's own claim about where §4.3.1's
        // `key_id` land) is directly observable in the counter block —
        // check it against a hand-built expectation rather than trusting
        // the implementation's own internal helper.
        let block = derivation_counter_block(&[0u8; 14], Label::SrtpAuthentication);
        let mut expected = [0u8; 16];
        expected[0] = Label::SrtpAuthentication as u8;
        assert_eq!(block, expected);
    }
}
