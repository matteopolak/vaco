//! The rendezvous cookie: a 32-bit value both peers compute independently
//! and compare to decide which one becomes Initiator — `draft-sharabayko-srt-01`
//! §4.3.2, quoted from the fetched IETF datatracker rendering:
//!
//! > "Each party generates a cookie value (a 32-bit number) based on the
//! > host, port, and current time with 1 minute accuracy. This value is
//! > scrambled using an MD5 sum calculation. The cookie values are then
//! > compared with one another." ... "When one party's cookie value is
//! > greater than its peer's, it wins the cookie contest and becomes
//! > Initiator (the other party becomes the Responder)."
//!
//! **What the fetched draft text does not state, and this module cannot
//! therefore claim to match**: the exact byte layout MD5 is computed over
//! (field order, separators, endianness of the embedded values) and which
//! bytes of the 16-byte MD5 digest become the 32-bit cookie. With no
//! reference SRT peer reachable on this machine (`lib.rs`'s own docs), a
//! byte-for-byte match to a real implementation's cookie cannot be
//! verified here regardless of what this module picks — the self-hosted
//! rendezvous test in `session.rs` only needs both sides of *this crate*
//! to agree, which any consistent choice satisfies.
//!
//! This module's own choice, stated plainly rather than left implicit: hash
//! `local_ip_bytes || local_port (BE u16) || peer_ip_bytes || peer_port (BE
//! u16) || minute_bucket (BE u64, Unix seconds / 60)` with MD5 (via
//! `vaco-hash`, D11 — no second `md-5` dependency), and take the first 4
//! bytes of the digest, big-endian, as the cookie. Interop with a real peer
//! needs this reconciled against that peer's own actual byte layout once
//! one is reachable — tracked, not hidden.

use vaco_hash::HashAlgo;

/// Compute this side's rendezvous cookie.
///
/// `now_unix_secs` is injected rather than read from the clock internally,
/// so [`compute`] is a pure function callers (and tests) can reproduce
/// exactly — `session.rs` is the one place that reads
/// `vaco_time::unix_nanos()`.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "a 60-second bucket is an exact quotient by design, not a lossy scale-down"
)]
pub fn compute(local_ip: &[u8], local_port: u16, peer_ip: &[u8], peer_port: u16, now_unix_secs: u64) -> u32 {
    let minute_bucket = now_unix_secs / 60;
    let mut preimage = Vec::new();
    preimage.extend_from_slice(local_ip);
    preimage.extend_from_slice(&local_port.to_be_bytes());
    preimage.extend_from_slice(peer_ip);
    preimage.extend_from_slice(&peer_port.to_be_bytes());
    preimage.extend_from_slice(&minute_bucket.to_be_bytes());

    let Some(hex) = HashAlgo::Md5.digest_hex(&preimage) else {
        // `HashAlgo::Md5` is always `Some` (checked in `vaco-hash`'s own
        // tests) — this arm exists so a future change to that guarantee is
        // a defined fallback, not a panic, in a crate this workspace
        // forbids `unwrap`/`expect` in.
        return 0;
    };
    let bytes = hex.as_bytes().get(0..8).unwrap_or(&[]);
    let mut cookie = 0u32;
    for chunk in bytes.chunks(2) {
        let byte = std::str::from_utf8(chunk)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        cookie = (cookie << 8) | u32::from(byte);
    }
    cookie
}

/// Who wins the cookie contest — `draft` §4.3.2: strictly greater wins;
/// equal cookies (host/port/minute-bucket coincide on both sides, e.g. a
/// loopback self-test) have no winner under the draft's own rule as quoted,
/// so this returns `None` rather than picking one arbitrarily.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contest {
    LocalWins,
    PeerWins,
    Tie,
}

#[must_use]
pub const fn resolve(local_cookie: u32, peer_cookie: u32) -> Contest {
    if local_cookie > peer_cookie {
        Contest::LocalWins
    } else if peer_cookie > local_cookie {
        Contest::PeerWins
    } else {
        Contest::Tie
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-consistency: this module's own chosen construction is
    /// deterministic and order-sensitive (the one property `session.rs`'s
    /// rendezvous state machine actually depends on) — not a draft-derived
    /// check, since the draft does not state the preimage layout (see
    /// module docs).
    #[test]
    fn deterministic_and_endpoint_sensitive() {
        let a = compute(&[127, 0, 0, 1], 4000, &[127, 0, 0, 1], 5000, 1_700_000_000);
        let b = compute(&[127, 0, 0, 1], 4000, &[127, 0, 0, 1], 5000, 1_700_000_000);
        assert_eq!(a, b, "same inputs must produce the same cookie");

        let c = compute(&[127, 0, 0, 1], 4001, &[127, 0, 0, 1], 5000, 1_700_000_000);
        assert_ne!(a, c, "a different local port must change the cookie");
    }

    #[test]
    fn one_minute_bucket_is_stable_within_the_minute() {
        // 120 is minute-aligned (120 / 60 == 2); 179 is the last second of
        // that same bucket, 180 the first of the next.
        let a = compute(&[10, 0, 0, 1], 1, &[10, 0, 0, 2], 2, 120);
        let b = compute(&[10, 0, 0, 1], 1, &[10, 0, 0, 2], 2, 179);
        assert_eq!(a, b, "same 60-second bucket must produce the same cookie");
        let c = compute(&[10, 0, 0, 1], 1, &[10, 0, 0, 2], 2, 180);
        assert_ne!(a, c, "crossing a minute boundary must change the cookie");
    }

    #[test]
    fn contest_resolves_strictly_greater_wins() {
        assert_eq!(resolve(5, 3), Contest::LocalWins);
        assert_eq!(resolve(3, 5), Contest::PeerWins);
        assert_eq!(resolve(4, 4), Contest::Tie);
    }
}
