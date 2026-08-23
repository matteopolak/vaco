//! RFC 4648 base64, written here rather than adopted.
//!
//! `data:` is the only user, the alphabet is twelve lines, and D10 makes a new
//! dependency a reviewed decision — not worth it for this. Measured against
//! `ffmpeg 8.1`'s `data:` protocol (see [`crate::data`]): the reference is
//! **strict**. No whitespace tolerance, no URL-safe alphabet, and padding must
//! be exactly right — `data:audio/wav;base64,aGVsbG8` (unpadded, `ffmpeg -i`)
//! is refused with "Invalid data found when processing input", and
//! `data:audio/wav;base64,aGVs bG8=` (one embedded space) is refused with
//! "Invalid base64 in URI". This module matches both: [`decode`] rejects any
//! byte outside the standard alphabet plus `=`, and rejects a length that is
//! not a multiple of four.

/// The standard (`+`/`/`) alphabet, RFC 4648 §4. Index is the 6-bit value.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode `data` as standard base64 with `=` padding.
///
/// Not needed by [`crate::data`] (which only ever decodes), but kept for the
/// round-trip property test and because a base64 module that cannot produce
/// its own fixtures is a module nobody can write a regression seed for.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);

        let i0 = usize::try_from((n >> 18) & 0x3f).unwrap_or(0);
        let i1 = usize::try_from((n >> 12) & 0x3f).unwrap_or(0);
        let i2 = usize::try_from((n >> 6) & 0x3f).unwrap_or(0);
        let i3 = usize::try_from(n & 0x3f).unwrap_or(0);

        out.push(char::from(*ALPHABET.get(i0).unwrap_or(&b'A')));
        out.push(char::from(*ALPHABET.get(i1).unwrap_or(&b'A')));
        out.push(if chunk.len() > 1 {
            char::from(*ALPHABET.get(i2).unwrap_or(&b'A'))
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(*ALPHABET.get(i3).unwrap_or(&b'A'))
        } else {
            '='
        });
    }
    out
}

/// Reverse [`ALPHABET`]: byte value -> 6-bit index, or `255` for "not in the
/// alphabet". A lookup table rather than a `match` because every byte value is
/// a candidate and a table makes that total by construction.
// `slice::get`/`get_mut` are not yet const-stable on this toolchain, so the
// usual indexing-free style is unavailable here. Both indices are provably in
// bounds without it: `i < ALPHABET.len() == 64`, and every element of
// `ALPHABET` is an ASCII byte, so `b as usize < 256 == table.len()` always.
#[allow(
    clippy::indexing_slicing,
    reason = "both indices are bounded by the fixed sizes of ALPHABET (64) and table (256); see above"
)]
const fn decode_table() -> [u8; 256] {
    let mut table = [255u8; 256];
    let mut i = 0;
    while i < ALPHABET.len() {
        table[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
}

const DECODE_TABLE: [u8; 256] = decode_table();

/// Decode standard base64, `=`-padded, no embedded whitespace.
///
/// Strict on purpose: see the module docs for what the reference itself
/// rejects. A permissive decoder here would accept URLs the reference refuses,
/// which is a correctness divergence in the direction that matters least
/// (accepting more) but is still wrong.
///
/// # Errors
/// [`Base64Error`] if `s` contains a character outside the standard alphabet
/// (or `=`), if its length is not a multiple of four, or if padding appears
/// anywhere but the last one or two characters of the final group.
pub fn decode(s: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(Base64Error::BadLength);
    }
    let mut out = Vec::new();
    for group in bytes.chunks(4) {
        let quad = decode_group(group)?;
        out.extend_from_slice(&quad);
    }
    Ok(out)
}

/// Decode one four-character group, returning 1-3 output bytes depending on
/// how much padding it carries.
fn decode_group(group: &[u8]) -> Result<Vec<u8>, Base64Error> {
    // `chunks(4)` on a length that is a multiple of four always yields exactly
    // four, but the compiler cannot see that, so index defensively rather than
    // asserting it (indexing_slicing is denied workspace-wide).
    let &[c0, c1, c2, c3] = group else {
        return Err(Base64Error::BadLength);
    };

    let pad2 = c2 == b'=';
    let pad3 = c3 == b'=';
    // `=` may only appear as the trailing one or two characters: `a=b=` and
    // `=abc` are both malformed, not merely unusual.
    if (c0 == b'=' || c1 == b'=') || (pad2 && !pad3) {
        return Err(Base64Error::BadPadding);
    }

    let v0 = lookup(c0)?;
    let v1 = lookup(c1)?;
    let n0 = (v0 << 2) | (v1 >> 4);

    if pad2 {
        return Ok(vec![n0]);
    }
    let v2 = lookup(c2)?;
    let n1 = (v1 << 4) | (v2 >> 2);

    if pad3 {
        return Ok(vec![n0, n1]);
    }
    let v3 = lookup(c3)?;
    let n2 = (v2 << 6) | v3;
    Ok(vec![n0, n1, n2])
}

fn lookup(b: u8) -> Result<u8, Base64Error> {
    let v = *DECODE_TABLE.get(usize::from(b)).unwrap_or(&255);
    if v == 255 {
        return Err(Base64Error::BadChar(b));
    }
    Ok(v)
}

/// Why [`decode`] refused the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base64Error {
    /// Length is not a multiple of four.
    BadLength,
    /// A byte outside the standard alphabet and outside `=`.
    BadChar(u8),
    /// `=` appeared somewhere other than the last one or two characters of the
    /// final group.
    BadPadding,
}

impl std::fmt::Display for Base64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadLength => f.write_str("base64 length is not a multiple of four"),
            Self::BadChar(b) => write!(f, "byte {b:#04x} is not in the base64 alphabet"),
            Self::BadPadding => f.write_str("`=` in the wrong position"),
        }
    }
}

impl std::error::Error for Base64Error {}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_match_rfc_4648() {
        // RFC 4648 §10.
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, enc) in cases {
            assert_eq!(encode(raw), *enc, "{raw:?}");
            assert_eq!(decode(enc).unwrap(), *raw, "{enc}");
        }
    }

    #[test]
    fn measured_reference_vector_decodes() {
        // `ffmpeg -i "data:application/octet-stream;base64,aGVsbG8gd29ybGQ="`
        // yields the literal bytes `hello world` (see the module docs).
        assert_eq!(decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
    }

    #[test]
    fn unpadded_input_is_refused() {
        // Measured: the reference rejects `aGVsbG8` (7 chars, no padding) with
        // "Invalid data found when processing input".
        assert_eq!(decode("aGVsbG8").unwrap_err(), Base64Error::BadLength);
    }

    #[test]
    fn embedded_whitespace_is_refused() {
        // Measured: "Invalid base64 in URI" for a payload with a space
        // spliced in. Same length as the valid `aGVsbG8=` (multiple of four),
        // so this exercises the alphabet check rather than the length check.
        assert!(matches!(
            decode("aGVs G8="),
            Err(Base64Error::BadChar(b' '))
        ));
    }

    #[test]
    fn misplaced_padding_is_refused() {
        assert_eq!(decode("=GVs").unwrap_err(), Base64Error::BadPadding);
        assert_eq!(decode("aG=s").unwrap_err(), Base64Error::BadPadding);
        assert_eq!(decode("a=s=").unwrap_err(), Base64Error::BadPadding);
    }

    #[test]
    fn every_alphabet_byte_round_trips() {
        let all: Vec<u8> = (0..=255u8).collect();
        let enc = encode(&all);
        assert_eq!(decode(&enc).unwrap(), all);
    }

    proptest::proptest! {
        #[test]
        fn encode_then_decode_is_the_identity(data: Vec<u8>) {
            let enc = encode(&data);
            proptest::prop_assert_eq!(decode(&enc).unwrap(), data);
        }

        #[test]
        fn decode_never_panics_on_arbitrary_text(s in ".{0,200}") {
            let _ = decode(&s);
        }
    }
}
