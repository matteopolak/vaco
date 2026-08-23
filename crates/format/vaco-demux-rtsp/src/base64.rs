//! A small standard-alphabet Base64 codec (RFC 4648 §4).
//!
//! No `base64` crate is declared workspace-wide (D10 makes every dependency
//! adoption a reviewed decision, and none has reviewed one for this
//! project) — `vaco-protocol-tls`'s `pem` module already set the precedent
//! of writing a small codec locally rather than adding one for a single
//! caller. This crate needs it twice: HTTP-tunnelled RTSP wraps every
//! message in Base64 (`crate::http_tunnel`), and RTSP Basic authentication
//! (`crate::auth`) is `base64(username:password)`, same as HTTP's.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode `data` as standard Base64, with `=` padding.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);

        let sextet = |shift: u32| -> u8 {
            let idx = usize::try_from((n >> shift) & 0x3F).unwrap_or(0);
            ALPHABET.get(idx).copied().unwrap_or(b'A')
        };
        let c0 = sextet(18);
        let c1 = sextet(12);
        let c2 = sextet(6);
        let c3 = sextet(0);

        out.push(c0 as char);
        out.push(c1 as char);
        out.push(if chunk.len() > 1 { c2 as char } else { '=' });
        out.push(if chunk.len() > 2 { c3 as char } else { '=' });
    }
    out
}

fn value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode standard Base64, tolerating (and ignoring) any whitespace and a
/// missing/short/malformed trailing `=` padding — several RTSP-over-HTTP
/// servers this crate was checked against send Base64 with no padding at
/// all. Never panics on malformed input: an invalid character or an
/// incomplete final group simply stops decoding at that point.
#[must_use]
pub fn decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = value(byte) else { break };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xFF).unwrap_or(0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc_4648_test_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn decodes_the_rfc_4648_test_vectors() {
        assert_eq!(decode(""), b"");
        assert_eq!(decode("Zg=="), b"f");
        assert_eq!(decode("Zm8="), b"fo");
        assert_eq!(decode("Zm9v"), b"foo");
        assert_eq!(decode("Zm9vYg=="), b"foob");
        assert_eq!(decode("Zm9vYmE="), b"fooba");
        assert_eq!(decode("Zm9vYmFy"), b"foobar");
    }

    #[test]
    fn decode_tolerates_missing_padding_and_whitespace() {
        assert_eq!(decode("Zm9v\r\n"), b"foo");
        assert_eq!(decode("Zg"), b"f");
    }

    proptest::proptest! {
        #[test]
        fn round_trips_arbitrary_bytes(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let encoded = encode(&data);
            proptest::prop_assert_eq!(decode(&encoded), data);
        }

        #[test]
        fn decode_never_panics(s in ".{0,500}") {
            let _ = decode(&s);
        }
    }
}
