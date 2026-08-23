//! The `data:` protocol — RFC 2397 data URLs, with the reference's own
//! divergences from the RFC.
//!
//! # Grammar
//!
//! `data:[<mediatype>][;base64],<payload>` — but three things measured against
//! `ffmpeg 8.1` are not what RFC 2397 itself says:
//!
//! 1. **No percent-decoding.** `data:text/plain,hello%20world` yields the
//!    literal bytes `hello%20world`, not `hello world`. A decoder here would
//!    make `%2e%2e` mean something it should not for a scheme with no path
//!    semantics at all — see `vaco-protocol-file`'s `path` module for the same
//!    argument made about `file:`.
//! 2. **`;base64` is a literal, case-sensitive token.** `;BASE64` is treated as
//!    part of an (invalid) content type, not as the flag; the payload is then
//!    read as literal bytes.
//! 3. **The media type, when present, must contain `/`.** `data:x,hello` is
//!    refused ("Invalid content-type 'x'"); `data:,hello` (nothing before the
//!    comma at all) is accepted. See [`parse`] for the exact rule this
//!    collapses to.
//!
//! Base64 decoding is strict — see [`crate::base64`].
//!
//! # Security
//!
//! `data:` never opens another URL: the payload is the whole of what it needs,
//! already sitting in the URL string. There is no inner-URL surface here at
//! all, which is why this protocol's [`vaco_protocol_core::ProtocolDesc`] has
//! an empty `default_whitelist` and `nested_scheme: false`.

use vaco_io::{MediaSource, MemorySource};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

use crate::base64;

/// Split a `data:` URL's `rest` into its decoded payload.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a missing `,` delimiter, an invalid
/// content type, or (when `;base64` was present) invalid base64.
pub fn parse(rest: &str) -> Result<Vec<u8>> {
    // The header (mediatype + params) and the payload are split on the FIRST
    // comma. A payload that itself contains a comma is common (base64 does
    // not use `,`, but arbitrary literal bytes might) and must not be
    // re-split.
    let Some(comma) = rest.find(',') else {
        return Err(ProtocolError::Malformed {
            scheme: "data",
            detail: "no ',' delimiter in URI",
        });
    };
    let header = rest.get(..comma).unwrap_or("");
    let payload = rest.get(comma + 1..).unwrap_or("");

    // The content-type rule, measured (see the module docs): when `header` is
    // non-empty, the substring before its first `;` must contain `/`. An
    // entirely empty header — the bare `data:,payload` form — needs no type at
    // all. This single rule reproduces every case that was probed: `x`
    // (rejected), `;base64` (rejected: the part before `;` is empty but the
    // header itself is not), `foo/bar` and `foo/bar;base64` (accepted).
    if !header.is_empty() {
        let ctype = header.split(';').next().unwrap_or("");
        if !ctype.contains('/') {
            return Err(ProtocolError::Malformed {
                scheme: "data",
                detail: "invalid content-type before the ',' delimiter",
            });
        }
    }

    // The literal, case-sensitive `base64` token as one of the `;`-separated
    // parameters — it need not be the last one (`;base64;charset=utf-8`
    // measured to work).
    let is_base64 = header.split(';').skip(1).any(|p| p == "base64");

    if is_base64 {
        base64::decode(payload).map_err(|_| ProtocolError::Malformed {
            scheme: "data",
            detail: "invalid base64 in URI",
        })
    } else {
        // No percent-decoding: see the module docs, point 1.
        Ok(payload.as_bytes().to_vec())
    }
}

/// The `data:` protocol. Read-only: the reference lists it only under `Input`
/// protocols (`ffmpeg -protocols`), and a scheme whose entire content lives in
/// the URL string has no meaningful "write" side.
#[derive(Debug, Clone, Copy, Default)]
pub struct DataProtocol;

impl Protocol for DataProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let bytes = parse(&url.rest)?;
        Ok(Box::new(MemorySource::new(bytes)))
    }
}

/// The registry entry for `data:`.
pub static DATA_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "data",
    long_name: "data URI",
    // `data:` is read-only: `ffmpeg -protocols` lists it under `Input:` and
    // not under `Output:`, which is the whole point of a URI that *is* its
    // own payload. `LOCAL` is read+write because `file` and `pipe` are, so
    // this one field is overridden rather than a third const invented.
    flags: ProtocolFlags {
        writable: false,
        ..ProtocolFlags::LOCAL
    },
    default_whitelist: &[],
    options: None,
    proto: &DataProtocol,
};

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
    fn bare_minimal_form_needs_no_media_type() {
        // Measured: `data:,hello` -> literal `hello`.
        assert_eq!(parse(",hello").unwrap(), b"hello");
    }

    #[test]
    fn no_percent_decoding() {
        assert_eq!(parse("text/plain,hello%20world").unwrap(), b"hello%20world");
    }

    #[test]
    fn base64_flag_is_literal_and_case_sensitive() {
        assert_eq!(
            parse("application/octet-stream;base64,aGVsbG8gd29ybGQ=").unwrap(),
            b"hello world"
        );
        // Uppercase is not recognised as the flag, so the payload is literal.
        assert_eq!(parse("audio/wav;BASE64,aGVsbG8=").unwrap(), b"aGVsbG8=",);
    }

    #[test]
    fn base64_flag_need_not_be_last_param() {
        assert_eq!(
            parse("audio/wav;base64;charset=utf-8,aGVsbG8=").unwrap(),
            b"hello"
        );
    }

    #[test]
    fn missing_comma_is_malformed() {
        assert!(matches!(
            parse("justtext"),
            Err(ProtocolError::Malformed { scheme: "data", .. })
        ));
    }

    #[test]
    fn content_type_without_a_slash_is_rejected_unless_empty() {
        assert!(parse("x,hello").is_err());
        assert!(parse(";base64,aGk=").is_err());
        assert!(parse(",hello").is_ok());
        assert!(parse("foo/bar,hello").is_ok());
    }

    #[test]
    fn invalid_base64_is_malformed_not_a_panic() {
        assert!(parse("audio/wav;base64,not valid base64!!").is_err());
        assert!(parse("audio/wav;base64,aGVsbG8").is_err()); // unpadded
    }

    #[test]
    fn opens_through_the_protocol_trait() {
        let proto = DataProtocol;
        let url = vaco_protocol_core::split_url("data:,hi");
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let mut src = proto.open(&url, IoFlags::READ, &Dict::new(), &env).unwrap();
        let mut buf = [0u8; 2];
        src.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");
    }
}
