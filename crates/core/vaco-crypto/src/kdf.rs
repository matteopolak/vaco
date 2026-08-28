//! PBKDF2-HMAC-SHA256 key derivation — RFC 8018 §5.2's algorithm, over the
//! `Sha256` primitive `vaco-hash` owns.
//!
//! # Provenance note
//!
//! RFC 8018 (PKCS #5 v2.1, which defines PBKDF2) contains **no test
//! vectors of its own** — checked directly against the fetched RFC text
//! (its table of contents lists only Appendices A/B/C/D/E, none titled
//! "Test Vectors"; a full-text search for "Test Vector" and for "6070"
//! both return zero matches), not assumed from the RFC's reputation. RFC
//! 6070 is the usual source of PBKDF2 test vectors, but only for
//! HMAC-SHA1; `VSF TR-06-2` needs HMAC-SHA256. RFC 7914 (`scrypt`) §11
//! gives genuine PBKDF2-HMAC-SHA256 vectors as a building-block check for
//! its own algorithm, so those are used here instead.

use vaco_hash::sha2::Sha256;

/// Derive `out_len` bytes via PBKDF2-HMAC-SHA256.
#[must_use]
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out_len: usize) -> Vec<u8> {
    let mut out = vec![0u8; out_len];
    pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out);
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

    // --- RFC-vector-derived: RFC 7914 §11's own PBKDF2-HMAC-SHA256
    // vectors (algorithm-level -- not RIST-specific, but genuinely
    // independent evidence the generic KDF is implemented correctly).

    #[test]
    fn rfc7914_vector_c_1_dklen_64() {
        let out = pbkdf2_hmac_sha256(b"passwd", b"salt", 1, 64);
        assert_eq!(
            out,
            hex("55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783")
        );
    }

    #[test]
    fn rfc7914_vector_c_80000_dklen_64() {
        let out = pbkdf2_hmac_sha256(b"Password", b"NaCl", 80_000, 64);
        assert_eq!(
            out,
            hex("4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56a1d425a1225833549adb841b51c9b3176a272bdebba1d078478f62b397f33c8d")
        );
    }

    // --- draft-derived: `VSF TR-06-2:2022` Annex B's own worked example
    // (passphrase "Reliable Internet Stream Transport", nonce 0x52495354,
    // 1024 iterations) -- the expected keys are independently re-derived
    // via Python's stdlib `hashlib.pbkdf2_hmac` with these exact inputs
    // before being trusted, not merely read off the spec's rendered page
    // (the same discipline as this crate's sibling `vaco-protocol-rist`'s
    // NACK-bitmask test).

    #[test]
    fn tr_06_2_annex_b_128_bit_key() {
        let key = pbkdf2_hmac_sha256(
            b"Reliable Internet Stream Transport",
            &hex("52495354"),
            1024,
            16,
        );
        assert_eq!(key, hex("1c2b0cfc90ae2638fea78c7fb2977047"));
    }

    #[test]
    fn tr_06_2_annex_b_256_bit_key() {
        let key = pbkdf2_hmac_sha256(
            b"Reliable Internet Stream Transport",
            &hex("52495354"),
            1024,
            32,
        );
        assert_eq!(
            key,
            hex("1c2b0cfc90ae2638fea78c7fb297704718bff7f4052743001a9b7ebb51cc9f1c")
        );
    }
}
