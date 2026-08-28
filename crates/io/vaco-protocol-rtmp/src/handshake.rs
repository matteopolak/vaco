//! The byte exchange (C0/C1/C2 against S0/S1/S2) that must complete before
//! either side has a chunk stream at all.
//!
//! # Two schemes, two very different provenances
//!
//! The **plain** handshake is in Adobe's own published specification
//! (`adobe-rtmp-spec-1.0` §5.2): C0/S0 is one version byte; C1/S1/C2/S2 are
//! 1536-byte signatures the peer echoes back, unauthenticated.
//!
//! The **digest** (a.k.a. "complex") handshake real deployments actually
//! negotiate is not in that document at all — Adobe's spec stops at the
//! plain form. It embeds an HMAC-SHA256 digest in C1/S1 at a
//! peer-recoverable offset, keyed by two fixed constants, and adds a
//! challenge/response step in C2/S2 that proves each side actually parsed
//! the other's signature. This module implements it from
//! `rtmpe-cleanroom-spec`, an independent clean-room write-up, cross-checked
//! against a second independent source (see that source's registration in
//! `provenance/sources.toml`) for the digest-offset formula and the 32-byte
//! `RANDOM_CRUD` constant, which agreed byte-for-byte between the two.
//!
//! **What is unverified**: [`crypto::hmac_sha256`] is checked against
//! RFC 4231's own test vectors, and the offset/constant values are
//! corroborated by two independent sources — but the digest handshake as a
//! *whole* (this exact byte layout, this exact key material, in this exact
//! order) has only been checked against itself here (a client encodes a C1,
//! decodes it back, and the digest still validates), never against a real
//! Flash Media Server or `nginx-rtmp-module`. If a real server rejects this
//! handshake, the offset formula or a key constant is the first place to
//! recheck.

use vaco_protocol_core::{ProtocolError, Result};

use crate::crypto::hmac_sha256;
use crate::rng::Filler;

/// Every handshake signature (C1/S1/C2/S2) is this many bytes.
pub const SIG_SIZE: usize = 1536;

/// The one C0/S0 byte: RTMP version 3, the only version this crate (or any
/// deployed RTMP client) speaks.
pub const VERSION: u8 = 3;

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

/// `Genuine Adobe Flash Player 001` — the client-side digest key (30 bytes).
const GENUINE_FP_CONST: &[u8] = b"Genuine Adobe Flash Player 001";
/// `Genuine Adobe Flash Media Server 001` — the server-side digest key (36
/// bytes).
const GENUINE_FMS_CONST: &[u8] = b"Genuine Adobe Flash Media Server 001";
/// The fixed 32-byte suffix appended to whichever `GENUINE_*_CONST` derives
/// the C2/S2 *response*-signing key, as opposed to the C1/S1 digest key.
#[rustfmt::skip]
const RANDOM_CRUD: [u8; 32] = [
    0xf0, 0xee, 0xc2, 0x4a, 0x80, 0x68, 0xbe, 0xe8, 0x2e, 0x00, 0xd0, 0xd1, 0x02, 0x9e, 0x7e, 0x57,
    0x6e, 0xec, 0x5d, 0x2d, 0x29, 0x80, 0x6f, 0xab, 0x93, 0xb8, 0xe6, 0x36, 0xcf, 0xeb, 0x31, 0xae,
];

/// Which half of the 1528-byte body carries the digest scheme's digest
/// field. A peer's scheme is a property of the bytes it sent, not something
/// either side announces — [`find_digest`] tries both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// Offset computed from bytes 8..12; digest lands in 12..772.
    Zero,
    /// Offset computed from bytes 772..776; digest lands in 776..1536.
    One,
}

impl Scheme {
    const ALL: [Self; 2] = [Self::Zero, Self::One];

    const fn offset_source(self) -> (usize, usize) {
        match self {
            Self::Zero => (8, 12),
            Self::One => (772, 776),
        }
    }

    const fn digest_base(self) -> usize {
        match self {
            Self::Zero => 12,
            Self::One => 776,
        }
    }
}

fn digest_offset(scheme: Scheme, sig: &[u8; SIG_SIZE]) -> Option<usize> {
    let (start, end) = scheme.offset_source();
    let bytes = sig.get(start..end)?;
    let sum: u32 = bytes.iter().map(|&b| u32::from(b)).sum();
    Some(scheme.digest_base() + (sum % 728) as usize)
}

/// Compute the HMAC-SHA256 digest of `sig` with the 32 bytes at `offset`
/// excluded from the input, per the digest scheme's definition.
fn compute_digest(sig: &[u8; SIG_SIZE], offset: usize, key: &[u8]) -> Option<[u8; 32]> {
    let before = sig.get(..offset)?;
    let after = sig.get(offset.checked_add(32)?..)?;
    let mut msg = Vec::new();
    msg.extend_from_slice(before);
    msg.extend_from_slice(after);
    Some(hmac_sha256(key, &msg))
}

/// Find which scheme (if either) `sig`'s embedded digest validates under,
/// trying both — real peers do not announce which one they used.
///
/// # Returns
/// `Some((scheme, digest))` for the first scheme whose embedded digest
/// matches a freshly computed one; `None` if neither does (a peer that
/// spoke the plain handshake instead, or sent something else entirely).
#[must_use]
pub fn find_digest(sig: &[u8; SIG_SIZE], key: &[u8]) -> Option<(Scheme, [u8; 32])> {
    for scheme in Scheme::ALL {
        let offset = digest_offset(scheme, sig)?;
        let expected = compute_digest(sig, offset, key)?;
        if sig.get(offset..offset.checked_add(32)?) == Some(expected.as_slice()) {
            return Some((scheme, expected));
        }
    }
    None
}

fn write_at(sig: &mut [u8; SIG_SIZE], offset: usize, bytes: &[u8]) -> Option<()> {
    let dst = sig.get_mut(offset..offset.checked_add(bytes.len())?)?;
    dst.copy_from_slice(bytes);
    Some(())
}

/// Build C0+C1 for the **plain** handshake: version byte 0x03, then a
/// 1536-byte signature with a zero version field (which is itself the
/// marker for "plain, not digest") and unauthenticated filler.
#[must_use]
pub fn build_plain_c0_c1() -> Vec<u8> {
    let mut out = vec![VERSION];
    let mut sig = [0u8; SIG_SIZE];
    // time (bytes 0-3): this implementation always starts at 0, matching
    // several real clients that never bothered to fill in a real uptime —
    // the field is informational and no known server rejects a zero.
    // bytes 4-7 (zero): the plain-handshake marker.
    let mut filler = Filler::new();
    if let Some(rest) = sig.get_mut(8..) {
        filler.fill(rest);
    }
    out.extend_from_slice(&sig);
    out
}

/// Build C2 for the **plain** handshake: an exact echo of `s1`.
#[must_use]
pub fn build_plain_c2(s1: &[u8; SIG_SIZE]) -> [u8; SIG_SIZE] {
    *s1
}

/// Whether `s2` is the echo the plain handshake requires: its last 1528
/// bytes (everything after the 8-byte time/version header this
/// implementation never varies within a session) match what we sent as C1.
///
/// This crate does not require this check to pass before treating the
/// handshake as complete — several real servers rewrite the timestamp
/// fields rather than echoing them byte-for-byte — but a caller that wants
/// the stricter check has it.
#[must_use]
pub fn plain_s2_echoes_our_c1(our_c1: &[u8; SIG_SIZE], s2: &[u8; SIG_SIZE]) -> bool {
    our_c1.get(8..) == s2.get(8..)
}

/// Build C0+C1 for the **digest** handshake under `scheme`.
///
/// # Errors
/// Never, in practice — `scheme`'s offset always lands inside the 1536-byte
/// signature by construction (`% 728` bounds it). Returns
/// [`ProtocolError::Malformed`] only if that invariant is somehow violated,
/// so a bug here fails loudly instead of silently sending a bad digest.
pub fn build_digest_c0_c1(scheme: Scheme) -> Result<Vec<u8>> {
    let mut out = vec![VERSION];
    let mut sig = [0u8; SIG_SIZE];
    // A nonzero version field is what invites a peer to attempt digest
    // validation at all; several real clients use their own build number
    // here; this one is arbitrary and cosmetic; see the module docs.
    if let Some(version_field) = sig.get_mut(4..8) {
        version_field.copy_from_slice(&[0x80, 0x00, 0x07, 0x02]);
    }
    let mut filler = Filler::new();
    if let Some(rest) = sig.get_mut(8..) {
        filler.fill(rest);
    }
    let offset =
        digest_offset(scheme, &sig).ok_or_else(|| malformed("digest offset out of range"))?;
    let digest = compute_digest(&sig, offset, GENUINE_FP_CONST)
        .ok_or_else(|| malformed("digest offset out of range"))?;
    write_at(&mut sig, offset, &digest).ok_or_else(|| malformed("digest offset out of range"))?;
    out.extend_from_slice(&sig);
    Ok(out)
}

/// Validate `s1` under the digest scheme (trying both offset schemes) and
/// build C2's challenge/response signature.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `s1`'s digest does not validate under
/// either scheme — the peer is not speaking the digest handshake (it may
/// still be speaking the plain one; a caller that wants to fall back should
/// retry with [`build_plain_c0_c1`] on a fresh connection, since a
/// handshake cannot switch schemes mid-flight on Adobe's transport).
pub fn build_digest_c2(s1: &[u8; SIG_SIZE]) -> Result<[u8; SIG_SIZE]> {
    let (_, server_digest) = find_digest(s1, GENUINE_FMS_CONST)
        .ok_or_else(|| malformed("S1's digest did not validate under either scheme"))?;

    let mut key_material = Vec::new();
    key_material.extend_from_slice(GENUINE_FP_CONST);
    key_material.extend_from_slice(&RANDOM_CRUD);
    let temp_key = hmac_sha256(&key_material, &server_digest);

    let mut c2 = [0u8; SIG_SIZE];
    Filler::new().fill(&mut c2);
    let prefix_len = SIG_SIZE - 32;
    let signature = {
        let prefix = c2
            .get(..prefix_len)
            .ok_or_else(|| malformed("handshake signature size invariant violated"))?;
        hmac_sha256(&temp_key, prefix)
    };
    write_at(&mut c2, prefix_len, &signature)
        .ok_or_else(|| malformed("handshake signature size invariant violated"))?;
    Ok(c2)
}

/// Validate that `s2` is a genuine response to the C1 we sent (embedding
/// `our_c1_digest`) under the digest scheme — the mutual half of the
/// challenge/response, confirming the peer actually processed our
/// signature rather than echoing an old one.
#[must_use]
pub fn verify_digest_s2(our_c1_digest: &[u8; 32], s2: &[u8; SIG_SIZE]) -> bool {
    let mut key_material = Vec::new();
    key_material.extend_from_slice(GENUINE_FMS_CONST);
    key_material.extend_from_slice(&RANDOM_CRUD);
    let temp_key = hmac_sha256(&key_material, our_c1_digest);

    let prefix_len = SIG_SIZE - 32;
    let Some(prefix) = s2.get(..prefix_len) else {
        return false;
    };
    let expected = hmac_sha256(&temp_key, prefix);
    s2.get(prefix_len..) == Some(expected.as_slice())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    fn to_sig(bytes: &[u8]) -> [u8; SIG_SIZE] {
        <[u8; SIG_SIZE]>::try_from(bytes).unwrap()
    }

    #[test]
    fn plain_c1_has_the_zero_version_marker() {
        let c0_c1 = build_plain_c0_c1();
        assert_eq!(c0_c1.len(), 1 + SIG_SIZE);
        assert_eq!(c0_c1[0], VERSION);
        assert_eq!(&c0_c1[5..9], &[0, 0, 0, 0]);
    }

    #[test]
    fn plain_c2_is_an_exact_echo_of_s1() {
        let mut s1 = [0u8; SIG_SIZE];
        Filler::new().fill(&mut s1);
        let c2 = build_plain_c2(&s1);
        assert_eq!(c2, s1);
    }

    #[test]
    fn plain_s2_echo_check_accepts_a_real_echo_and_rejects_a_stranger() {
        let c0_c1 = build_plain_c0_c1();
        let our_c1 = to_sig(&c0_c1[1..]);
        assert!(plain_s2_echoes_our_c1(&our_c1, &our_c1));
        let mut other = [0u8; SIG_SIZE];
        Filler::new().fill(&mut other);
        assert!(!plain_s2_echoes_our_c1(&our_c1, &other));
    }

    #[test]
    fn digest_c1_validates_under_its_own_scheme() {
        for scheme in Scheme::ALL {
            let c0_c1 = build_digest_c0_c1(scheme).unwrap();
            let sig = to_sig(&c0_c1[1..]);
            let (found_scheme, _) = find_digest(&sig, GENUINE_FP_CONST).unwrap();
            assert_eq!(found_scheme, scheme);
        }
    }

    #[test]
    fn digest_c1_does_not_validate_under_the_wrong_key() {
        let c0_c1 = build_digest_c0_c1(Scheme::Zero).unwrap();
        let sig = to_sig(&c0_c1[1..]);
        assert!(find_digest(&sig, GENUINE_FMS_CONST).is_none());
    }

    #[test]
    fn plain_c1_does_not_spuriously_validate_as_a_digest_c1() {
        // A purely random signature should not validate under either
        // scheme against either key — this would only happen by a
        // 2^-256 accident.
        let c0_c1 = build_plain_c0_c1();
        let sig = to_sig(&c0_c1[1..]);
        assert!(find_digest(&sig, GENUINE_FP_CONST).is_none());
        assert!(find_digest(&sig, GENUINE_FMS_CONST).is_none());
    }

    #[test]
    fn full_digest_handshake_round_trips_end_to_end_against_a_simulated_server() {
        // This crate has no server role (every `vaco-protocol-*` crate in
        // this workspace is a client), so the test plays the server's side
        // by hand: build S1 the way a server would (same shape as C1, but
        // keyed with `GENUINE_FMS_CONST`), then check both halves of the
        // challenge-response the way each side of a real handshake would.
        for scheme in Scheme::ALL {
            let mut s1 = [0u8; SIG_SIZE];
            Filler::new().fill(&mut s1);
            let s1_offset = digest_offset(scheme, &s1).unwrap();
            let server_digest = compute_digest(&s1, s1_offset, GENUINE_FMS_CONST).unwrap();
            write_at(&mut s1, s1_offset, &server_digest).unwrap();

            // The client's half: process S1, produce C2.
            let c2 = build_digest_c2(&s1).unwrap();
            let (found_scheme, extracted) = find_digest(&s1, GENUINE_FMS_CONST).unwrap();
            assert_eq!(found_scheme, scheme);
            assert_eq!(extracted, server_digest);

            // The server's half: verify C2 against the S1 digest it sent,
            // the same computation `verify_digest_s2` does but with the
            // client's key material (`GENUINE_FP_CONST`), since that is
            // what a real server checks C2 with.
            let mut key_material = Vec::new();
            key_material.extend_from_slice(GENUINE_FP_CONST);
            key_material.extend_from_slice(&RANDOM_CRUD);
            let temp_key = hmac_sha256(&key_material, &server_digest);
            let expected = hmac_sha256(&temp_key, c2.get(..SIG_SIZE - 32).unwrap());
            assert_eq!(c2.get(SIG_SIZE - 32..).unwrap(), expected);
        }
    }

    #[test]
    fn build_digest_c2_rejects_a_plain_s1() {
        let c0_c1 = build_plain_c0_c1();
        let s1 = to_sig(&c0_c1[1..]);
        assert!(build_digest_c2(&s1).is_err());
    }

    proptest::proptest! {
        #[test]
        fn digest_offset_always_lands_inside_the_signature(
            raw in proptest::collection::vec(proptest::prelude::any::<u8>(), SIG_SIZE)
        ) {
            let bytes = to_sig(&raw);
            for scheme in Scheme::ALL {
                if let Some(offset) = digest_offset(scheme, &bytes) {
                    proptest::prop_assert!(offset + 32 <= SIG_SIZE);
                }
            }
        }
    }
}
