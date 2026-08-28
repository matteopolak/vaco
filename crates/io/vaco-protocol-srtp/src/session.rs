//! RFC 3711 §4.1.1's per-packet keystream IV, §4.2's authentication tag,
//! and a small [`SrtpContext`] that ties key derivation, IV construction,
//! encryption, and the rollover counter together for one SSRC.

use crate::kdf::SessionKeys;
use vaco_crypto::{ctr_apply_aes128, hmac_sha1};

/// §4.1.1: `IV = (salt * 2^16) XOR (SSRC * 2^64) XOR (index * 2^16)`.
///
/// Byte layout of the 16-byte, big-endian result (index 0 = most
/// significant byte): `salt` occupies bytes 0-13 (its full 112 bits,
/// `* 2^16` leaving the low 16 bits as the two zero bytes 14-15 before any
/// XOR); `SSRC` (32 bits) is `XORed` into bytes 4-7 (`* 2^64` places it 64
/// bits from the right, i.e. 8 bytes up from the low end of the 16-byte
/// block); the 48-bit packet index is `XORed` into bytes 8-13 (`* 2^16`
/// places it 16 bits from the right, i.e. bytes 14-15 stay whatever the
/// salt left there).
#[must_use]
pub fn build_iv(salt: &[u8; 14], ssrc: u32, index: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[..14].copy_from_slice(salt);
    for (slot, b) in iv.iter_mut().skip(4).zip(ssrc.to_be_bytes()) {
        *slot ^= b;
    }
    // index is 48 bits; place its low 48 bits into bytes 8..14. Skipping
    // the top 2 (always-zero, for a 48-bit value) bytes of the 8-byte
    // big-endian representation via `.skip(2)` on the byte iterator
    // itself avoids indexing the fixed-size array with a non-constant
    // offset.
    for (slot, b) in iv.iter_mut().skip(8).zip(index.to_be_bytes().into_iter().skip(2)) {
        *slot ^= b;
    }
    iv
}

/// §4.2: the authenticated portion is the packet up to (not including)
/// the auth tag, with the 4-byte ROC appended (the ROC is never
/// transmitted on the wire, which is exactly why a receiver has to track
/// it out of band rather than reading it from the packet).
#[must_use]
pub fn compute_auth_tag(auth_key: &[u8], authenticated_portion: &[u8], roc: u32, tag_len: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(authenticated_portion);
    buf.extend_from_slice(&roc.to_be_bytes());
    let full_tag = hmac_sha1(auth_key, &buf);
    full_tag.into_iter().take(tag_len.min(20)).collect()
}

/// The default profile's tag length (`AES_CM_128_HMAC_SHA1_80`): 80 bits.
pub const DEFAULT_TAG_LEN: usize = 10;

/// Rollover-counter tracking for one SSRC's receive side.
///
/// **Scope cut, stated up front:** this is the simple case from RFC 3711
/// Appendix A.3's own description — "if the received sequence number
/// wraps around from a high value to a low one, increment the ROC" — not
/// Appendix A's full guessing algorithm, which also has to handle
/// packets that arrive far out of order across a rollover boundary. A
/// real deployment with heavy reordering near a rollover would need that
/// fuller algorithm; nothing in this crate's own tests exercises that
/// case.
#[derive(Debug, Clone, Copy, Default)]
pub struct RolloverTracker {
    roc: u32,
    highest_seq: Option<u16>,
}

impl RolloverTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self { roc: 0, highest_seq: None }
    }

    /// Observe one arriving sequence number, returning the 48-bit packet
    /// index (`ROC << 16 | seq`) to use for it, and updating the ROC if
    /// this sequence number indicates a rollover.
    pub fn observe(&mut self, seq: u16) -> u64 {
        if let Some(highest) = self.highest_seq {
            // A large negative jump (high -> low) means the counter
            // wrapped past 0xFFFF.
            if highest > 0x8000 && seq < 0x8000 && highest - seq > 0x8000 {
                self.roc = self.roc.wrapping_add(1);
            }
        }
        if self.highest_seq.is_none_or(|h| seq > h || (h > 0x8000 && seq < 0x8000)) {
            self.highest_seq = Some(seq);
        }
        (u64::from(self.roc) << 16) | u64::from(seq)
    }
}

/// Ties key derivation, IV construction and the rollover counter together
/// for one direction of one SSRC's SRTP stream.
#[derive(Debug)]
pub struct SrtpContext {
    keys: SessionKeys,
    ssrc: u32,
    tag_len: usize,
    rollover: RolloverTracker,
}

impl SrtpContext {
    #[must_use]
    pub const fn new(keys: SessionKeys, ssrc: u32) -> Self {
        Self { keys, ssrc, tag_len: DEFAULT_TAG_LEN, rollover: RolloverTracker::new() }
    }

    /// Encrypt `payload` in place and append an authentication tag,
    /// returning the full authenticated ciphertext (`header || ciphertext
    /// || tag`, ready to send as the SRTP packet's payload region).
    ///
    /// `header_and_payload` is the plaintext RTP header followed by the
    /// plaintext payload — the header itself is never encrypted (§4.1),
    /// only authenticated.
    #[must_use]
    pub fn protect(&mut self, seq: u16, header_len: usize, header_and_payload: &[u8]) -> Vec<u8> {
        let index = self.rollover.observe(seq);
        let iv = build_iv(&self.keys.salt, self.ssrc, index);
        let mut buf = header_and_payload.to_vec();
        #[allow(clippy::indexing_slicing, reason = "header_len is caller-supplied and bounded by header_and_payload.len() in this crate's own callers")]
        let payload = &mut buf[header_len..];
        apply_keystream(&self.keys.cipher_key, &iv, payload);
        let roc = self.rollover.current_roc();
        let tag = compute_auth_tag(&self.keys.auth_key, &buf, roc, self.tag_len);
        buf.extend_from_slice(&tag);
        buf
    }

    /// Verify the authentication tag and decrypt in place, returning the
    /// plaintext `header || payload` (tag stripped) or `None` if
    /// authentication failed.
    #[must_use]
    pub fn unprotect(&mut self, seq: u16, header_len: usize, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < header_len + self.tag_len {
            return None;
        }
        let (authenticated, tag) = packet.split_at(packet.len() - self.tag_len);
        let index = self.rollover.observe(seq);
        let roc = self.rollover.current_roc();
        let expected = compute_auth_tag(&self.keys.auth_key, authenticated, roc, self.tag_len);
        if !constant_time_eq(&expected, tag) {
            return None;
        }
        let iv = build_iv(&self.keys.salt, self.ssrc, index);
        let mut plaintext = authenticated.to_vec();
        #[allow(clippy::indexing_slicing, reason = "header_len already checked against packet.len() above")]
        let payload = &mut plaintext[header_len..];
        apply_keystream(&self.keys.cipher_key, &iv, payload);
        Some(plaintext)
    }
}

impl RolloverTracker {
    const fn current_roc(self) -> u32 {
        self.roc
    }
}

fn apply_keystream(cipher_key: &[u8], iv: &[u8; 16], data: &mut [u8]) {
    // Scope cut: only AES-128 wired through this helper today (matching
    // `derive_session_keys_aes128`'s 16-byte cipher key) — AES-256 SRTP
    // would need the AES-256 sibling here too, not built since nothing in
    // this crate's own tests exercises it yet.
    if let Ok(key) = <[u8; 16]>::try_from(cipher_key) {
        ctr_apply_aes128(&key, iv, data);
    }
}

/// Not a timing-side-channel-hardened implementation in the strong sense
/// (no SIMD, no verified-constant-time assembly), but avoids the obvious
/// short-circuit-on-first-mismatch shape a plain `==` on `Vec<u8>` has.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::kdf::derive_session_keys_aes128;

    #[test]
    fn build_iv_places_ssrc_and_index_at_the_documented_byte_offsets() {
        let salt = [0u8; 14];
        let iv = build_iv(&salt, 0xDEAD_BEEF, 0x0000_0000_0001);
        assert_eq!(&iv[4..8], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(&iv[8..14], &[0, 0, 0, 0, 0, 1]);
        assert_eq!(&iv[14..16], &[0, 0]);
    }

    #[test]
    fn different_ssrc_or_index_produces_a_different_iv() {
        let salt = [0x11u8; 14];
        let base = build_iv(&salt, 1, 1);
        assert_ne!(build_iv(&salt, 2, 1), base);
        assert_ne!(build_iv(&salt, 1, 2), base);
    }

    #[test]
    fn rollover_tracker_increments_roc_on_a_high_to_low_wrap() {
        let mut tracker = RolloverTracker::new();
        assert_eq!(tracker.observe(65530), 65530);
        assert_eq!(tracker.observe(65535), 65535);
        // Wraps past 0xFFFF back to a low sequence number.
        let index = tracker.observe(5);
        assert_eq!(index, (1u64 << 16) | 5);
    }

    #[test]
    fn rollover_tracker_does_not_increment_for_ordinary_reordering() {
        let mut tracker = RolloverTracker::new();
        tracker.observe(100);
        tracker.observe(102);
        let index = tracker.observe(101); // mild reorder, not a wrap
        assert_eq!(index, 101);
    }

    #[test]
    fn protect_then_unprotect_round_trips_the_plaintext() {
        let keys = derive_session_keys_aes128(&[0x01; 16], &[0x02; 14]);
        let mut sender = SrtpContext::new(keys.clone(), 0x1234_5678);
        let mut receiver = SrtpContext::new(keys, 0x1234_5678);

        let header = [0x80u8, 96, 0, 1, 0, 0, 0, 1, 0x12, 0x34, 0x56, 0x78];
        let mut plaintext = header.to_vec();
        plaintext.extend_from_slice(b"secret media payload");

        let protected = sender.protect(1, header.len(), &plaintext);
        assert_ne!(&protected[header.len()..protected.len() - DEFAULT_TAG_LEN], b"secret media payload");

        let recovered = receiver.unprotect(1, header.len(), &protected).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn unprotect_rejects_a_tampered_packet() {
        let keys = derive_session_keys_aes128(&[0x03; 16], &[0x04; 14]);
        let mut sender = SrtpContext::new(keys.clone(), 42);
        let mut receiver = SrtpContext::new(keys, 42);

        let header = [0x80u8, 8, 0, 1, 0, 0, 0, 1, 0, 0, 0, 42];
        let mut plaintext = header.to_vec();
        plaintext.extend_from_slice(b"payload");
        let mut protected = sender.protect(1, header.len(), &plaintext);

        let last = protected.len() - 1;
        protected[last] ^= 0xFF; // flip a bit in the auth tag itself

        assert!(receiver.unprotect(1, header.len(), &protected).is_none());
    }

    #[test]
    fn unprotect_rejects_a_packet_with_a_flipped_payload_bit() {
        let keys = derive_session_keys_aes128(&[0x05; 16], &[0x06; 14]);
        let mut sender = SrtpContext::new(keys.clone(), 7);
        let mut receiver = SrtpContext::new(keys, 7);

        let header = [0x80u8, 8, 0, 1, 0, 0, 0, 1, 0, 0, 0, 7];
        let mut plaintext = header.to_vec();
        plaintext.extend_from_slice(b"payload");
        let mut protected = sender.protect(1, header.len(), &plaintext);

        protected[header.len()] ^= 0x01; // flip a bit in the ciphertext

        assert!(receiver.unprotect(1, header.len(), &protected).is_none());
    }
}
