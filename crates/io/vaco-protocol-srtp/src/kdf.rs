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
//! **Scope cut:** only `key_derivation_rate = 0` is supported — the
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
/// value from: `(key_id XOR master_salt) || 0x0000`.
///
/// **Fixed twice, 2026-08-29, against a real independent peer — the second
/// fix is the one that actually interoperates.** §4.3.1 states `key_id =
/// <label> || r` where `r` has "the same length as" the 48-bit packet
/// index, i.e. `key_id` is `1 + 6 = 7` octets (56 bits), not 6 — a detail
/// easy to misread as "label plus whatever is left of a 48-bit `key_id`"
/// instead of "label plus a 48-bit `r`". `key_id` and `master_salt` are
/// then "aligned so that their least significant bits agree" (right-
/// aligned) before the XOR, so this 7-octet `key_id` occupies the *last*
/// seven octets of the 14-octet `master_salt` — indices 7 through 13, not
/// 8 through 13. With `kdr = 0` (this module's only supported rate) fixing
/// `r = 0`, `label` lands at exactly index 7 and the remaining six octets
/// (8-13) stay zero.
///
/// This module's first attempt `XOR`ed `label` into byte index 0 (the
/// *most* significant octet — the wrong end of "right-aligned" entirely).
/// The **first fix** moved it to index 8, reasoning `key_id` was 6 octets;
/// still wrong, by exactly one octet, for the reason above. Both versions
/// were self-consistent and passed every test this module had, because
/// RFC 3711 publishes no worked numeric example and this module's own
/// tests re-assert the same claim the code makes rather than an
/// independent one (see this module's provenance note). Neither wrong
/// version was distinguishable from the other, or from correct, without
/// an independent oracle:
///
/// - `vaco-mux-whip`'s real interop pass against `mediamtx` 1.20.1
///   completed a real DTLS handshake and exported real keying material
///   with the *first* fix (index 8) in place, and every resulting SRTP
///   packet was still silently dropped — `mediamtx` (`pion/srtp`, an
///   independent implementation) closed the session with "deadline
///   exceeded while waiting tracks" and never accepted one.
/// - `libsrtp` itself (the reference C implementation; checked directly
///   via its `pylibsrtp` binding, D17 applied to open-source code that is
///   not `FFmpeg`, so with no clean-room restriction) was then used to
///   cross-check a hand-built RTP packet end to end: given the same master
///   key/salt, `libsrtp`'s own `protect()` output matched this module's
///   *only* once the label moved to index 7. Index 8 produced a
///   completely different (and, per the failed `mediamtx` session,
///   non-interoperating) key.
fn derivation_counter_block(master_salt: &[u8; 14], label: Label) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..14].copy_from_slice(master_salt);
    // `key_id` (`label || r`, `r = 0`) is 7 octets, right-aligned against
    // the 14-octet salt field: `label` lands at index 7, the first of the
    // trailing seven.
    let Some(byte) = block.get_mut(7) else {
        return block;
    };
    *byte ^= label as u8;
    block
}

fn derive_bytes_128(
    master_key: &[u8; 16],
    master_salt: &[u8; 14],
    label: Label,
    out_len: usize,
) -> Vec<u8> {
    let counter_block = derivation_counter_block(master_salt, label);
    let mut out = vec![0u8; out_len];
    // AES-CTR keystream from an all-zero plaintext is exactly the
    // keystream itself (encrypt XORs the plaintext, i.e. does nothing to
    // zero bytes) — the standard way to get raw AES-CTR output from an
    // XOR-shaped primitive without a second "keystream" entry point.
    ctr_apply_aes128(master_key, &counter_block, &mut out);
    out
}

fn derive_bytes_256(
    master_key: &[u8; 32],
    master_salt: &[u8; 14],
    label: Label,
    out_len: usize,
) -> Vec<u8> {
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
    fn the_label_byte_lands_at_index_7_once_right_aligned_per_rfc_3711() {
        // RFC 3711 §4.3.1: `key_id = <label> || r`, where `r` is defined to
        // have "the same length as" the 48-bit packet index — so `key_id`
        // is 7 octets (56 bits), not 6. `key_id` and `master_salt` are then
        // "aligned so that their least significant bits agree" (right-
        // aligned) before the XOR. With `kdr = 0` (`r = 0`), `label` is
        // therefore the first of the trailing *seven* octets of the
        // 14-octet salt field — index 7 of 0..=13 — not index 0 and not
        // index 8. Checked against a hand-built expectation rather than
        // trusting the implementation's own internal helper; index 7 is
        // the placement a real independent peer (`libsrtp`, via
        // `pylibsrtp`) confirmed byte-for-byte, after index 0 and index 8
        // both failed a real `mediamtx` publish.
        let block = derivation_counter_block(&[0u8; 14], Label::SrtpAuthentication);
        let mut expected = [0u8; 16];
        expected[7] = Label::SrtpAuthentication as u8;
        assert_eq!(block, expected);
    }

    #[test]
    fn the_label_byte_xors_with_a_nonzero_salt_at_index_7_only() {
        // With a non-trivial salt and a non-zero label, every other byte of
        // the 14-octet salt region must pass through untouched — only
        // index 7 changes, proving the XOR neither spills into neighbouring
        // octets nor lands at the previously-suspected index 8. `label`
        // must be non-zero here (unlike `SrtpEncryption = 0x00`) or an XOR
        // at the wrong index would be indistinguishable from no XOR at all.
        let salt = [0xAAu8; 14];
        let block = derivation_counter_block(&salt, Label::SrtpSalting);
        for (i, (&s, &b)) in salt.iter().zip(block.iter().take(14)).enumerate() {
            if i == 7 {
                assert_eq!(b, s ^ Label::SrtpSalting as u8);
            } else {
                assert_eq!(b, s, "byte {i} should be untouched");
            }
        }
        assert_eq!(&block[14..], &[0u8, 0u8]);
    }
}
