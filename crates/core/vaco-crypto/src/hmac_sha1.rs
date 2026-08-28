//! HMAC-SHA1 — RFC 2104's generic construction over the `Sha1` primitive
//! `vaco-hash` owns, added for RFC 3711 (SRTP) §4.2's authentication tag
//! (the default `HMAC_SHA1_80`: the full 20-byte HMAC output, truncated
//! to the leftmost 10 bytes / 80 bits).
//!
//! `hmac_sha1` returns the full 20-byte tag; truncation is the caller's
//! own choice (SRTP's `n_tag` and RTMP's/other consumers' needs differ),
//! not baked in here — mirrors [`crate::ctr_apply_aes128`] owning only the
//! generic keystream primitive, not any one protocol's IV construction.

use hmac::{Hmac, Mac};
use vaco_hash::sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// Compute the 20-byte HMAC-SHA1 of `data` under `key`.
///
/// # Panics
///
/// Never, in practice: `HmacSha1::new_from_slice` only rejects a key
/// length by construction for MACs with a fixed key size, and HMAC
/// accepts any key length (RFC 2104), so the `expect` below is
/// unreachable rather than a real fallibility this function is hiding.
#[must_use]
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    #[allow(clippy::expect_used, reason = "HMAC accepts any key length by RFC 2104 definition")]
    let mut mac = HmacSha1::new_from_slice(key).expect("HMAC-SHA1 accepts any key length");
    mac.update(data);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 20];
    out.copy_from_slice(&result);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
            .collect()
    }

    // --- RFC-vector-derived: RFC 2202's own HMAC-SHA1 test cases 1-3,
    // cross-checked against Python's stdlib hmac+hashlib before being
    // trusted as this test's expected values (not merely recalled from
    // the RFC's rendered page — see this crate's own commit message).

    #[test]
    fn rfc2202_case_1() {
        let key = hex(&"0b".repeat(20));
        let tag = hmac_sha1(&key, b"Hi There");
        assert_eq!(hex_out(&tag), "b617318655057264e28bc0b6fb378c8ef146be00");
    }

    #[test]
    fn rfc2202_case_2() {
        let tag = hmac_sha1(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex_out(&tag), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn rfc2202_case_3() {
        let key = hex(&"aa".repeat(20));
        let data = hex(&"dd".repeat(50));
        let tag = hmac_sha1(&key, &data);
        assert_eq!(hex_out(&tag), "125d7342b9ac11cd91a39af48aa17b4f63f175d3");
    }

    fn hex_out(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }
}
