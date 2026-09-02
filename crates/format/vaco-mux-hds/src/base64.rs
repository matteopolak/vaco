//! RFC 4648 §4 standard base64 encoding — the `Manifest`'s own `<metadata>`
//! element wraps the AMF0 `onMetaData` blob this way.
//!
//! Hand-rolled rather than a new workspace dependency (D10): one alphabet
//! table and one 3-bytes-in/4-chars-out loop, the same amount of code
//! `vaco-protocol-http::headers`'s own `base64_standard` already carries for
//! `Authorization: Basic`, and several other crates in this workspace
//! (`vaco-protocol-local::data`, `vaco-protocol-icecast::request`,
//! `vaco-demux-rtsp`) each hand-roll their own copy for the same reason —
//! this crate follows the same, already-established convention rather than
//! introducing the first `base64` crate dependency.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[must_use]
pub fn encode(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        let c0 = b0 >> 2;
        let c1 = ((b0 & 0b0000_0011) << 4) | (b1 >> 4);
        let c2 = ((b1 & 0b0000_1111) << 2) | (b2 >> 6);
        let c3 = b2 & 0b0011_1111;

        let alphabet =
            |i: u8| char::from(ALPHABET.get(usize::from(i & 0x3f)).copied().unwrap_or(b'A'));
        out.push(alphabet(c0));
        out.push(alphabet(c1));
        out.push(if chunk.len() > 1 { alphabet(c2) } else { '=' });
        out.push(if chunk.len() > 2 { alphabet(c3) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_4648_worked_example() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }
}
