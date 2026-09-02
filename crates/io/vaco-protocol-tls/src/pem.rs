//! A small, permissive PEM reader: extract `-----BEGIN <label>-----` blocks
//! and base64-decode their contents.
//!
//! Written here rather than adopted (matching `vaco-protocol-local`'s own
//! `base64.rs`, which gives the general reasoning: D10 makes a new dependency
//! a reviewed decision, and neither `rustls-pemfile` nor a general `base64`
//! crate is pre-declared for this workspace — see the brief's "no new
//! dependencies" rule). Only what a `-ca_file` value needs: certificate
//! blocks. **Unlike `vaco-protocol-local`'s `data:` decoder, this one is
//! deliberately lenient** — real-world PEM files wrap base64 at 64 columns
//! with `\n` (sometimes `\r\n`), and a decoder that rejected embedded
//! whitespace the way the `data:` URI decoder correctly does (matching the
//! reference's own strictness there) would reject every PEM file anyone has
//! ever produced.

use vaco_protocol_core::{ProtocolError, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn value(b: u8) -> Option<u8> {
    ALPHABET.iter().position(|&c| c == b).map(|i| i as u8)
}

/// Decode standard base64, ignoring any ASCII whitespace and any trailing
/// `=` padding wherever it falls (real PEM producers pad correctly, but a
/// lenient reader should not choke on one that padded a line early).
///
/// # Errors
/// [`ProtocolError::Malformed`] on a byte outside the base64 alphabet
/// (whitespace and `=` excepted) or a truncated final group.
fn decode(input: &str) -> Result<Vec<u8>> {
    let mut digits: Vec<u8> = Vec::new();
    for b in input.bytes() {
        if b.is_ascii_whitespace() || b == b'=' {
            continue;
        }
        let Some(v) = value(b) else {
            return Err(ProtocolError::Malformed {
                scheme: "tls",
                detail: "PEM body contains a byte outside the base64 alphabet",
            });
        };
        digits.push(v);
    }

    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        let n = chunk.len();
        if n < 2 {
            return Err(ProtocolError::Malformed {
                scheme: "tls",
                detail: "PEM body ends mid base64 group",
            });
        }
        let get = |i: usize| chunk.get(i).copied().unwrap_or(0);
        let word = (u32::from(get(0)) << 18)
            | (u32::from(get(1)) << 12)
            | (u32::from(get(2)) << 6)
            | u32::from(get(3));
        out.push((word >> 16) as u8);
        if n > 2 {
            out.push((word >> 8) as u8);
        }
        if n > 3 {
            out.push(word as u8);
        }
    }
    Ok(out)
}

/// Extract every `-----BEGIN <label>-----`...`-----END <label>-----` block
/// matching `label` and base64-decode each one's body.
///
/// Blocks that do not match `label` (a private key section in a file that
/// also carries a certificate, for instance) are skipped rather than
/// erroring — a caller asking only for `"CERTIFICATE"` should not fail
/// because the same file also has a key in it.
///
/// # Errors
/// [`ProtocolError::Malformed`] if a matched block's body fails to
/// base64-decode. A block that never finds its `-----END ...-----` line is
/// silently ignored rather than erroring, since it may simply be a truncated
/// trailing block after the last one this caller wanted.
pub fn extract_der_blocks(pem: &str, label: &str) -> Result<Vec<Vec<u8>>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find(&begin) {
        let Some(after_begin) = rest.get(start.saturating_add(begin.len())..) else {
            break;
        };
        let Some(end_at) = after_begin.find(&end) else {
            break;
        };
        let Some(body) = after_begin.get(..end_at) else {
            break;
        };
        out.push(decode(body)?);
        let Some(tail) = after_begin.get(end_at.saturating_add(end.len())..) else {
            break;
        };
        rest = tail;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
-----BEGIN CERTIFICATE-----
aGVsbG8gd29ybGQ=
-----END CERTIFICATE-----
";

    #[test]
    fn decodes_a_wrapped_block() {
        let blocks = extract_der_blocks(SAMPLE, "CERTIFICATE").unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], b"hello world");
    }

    #[test]
    fn ignores_blocks_of_a_different_label() {
        let text = "-----BEGIN PRIVATE KEY-----\naGk=\n-----END PRIVATE KEY-----\n";
        let blocks = extract_der_blocks(text, "CERTIFICATE").unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn multiple_blocks_all_decode() {
        let text = format!("{SAMPLE}{SAMPLE}");
        let blocks = extract_der_blocks(&text, "CERTIFICATE").unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn whitespace_inside_the_body_is_tolerated() {
        let text =
            "-----BEGIN CERTIFICATE-----\naGVs\r\nbG8g\nd29ybGQ=\n-----END CERTIFICATE-----\n";
        let blocks = extract_der_blocks(text, "CERTIFICATE").unwrap();
        assert_eq!(blocks[0], b"hello world");
    }

    #[test]
    fn an_unterminated_block_is_ignored_not_errored() {
        let text = "-----BEGIN CERTIFICATE-----\naGVsbG8=\n";
        let blocks = extract_der_blocks(text, "CERTIFICATE").unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn invalid_alphabet_is_an_error_not_a_panic() {
        let text = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
        assert!(extract_der_blocks(text, "CERTIFICATE").is_err());
    }

    #[test]
    fn no_input_never_panics() {
        for s in ["", "-----BEGIN", "-----BEGIN CERTIFICATE-----", "\u{0}"] {
            let _ = extract_der_blocks(s, "CERTIFICATE");
        }
    }
}
