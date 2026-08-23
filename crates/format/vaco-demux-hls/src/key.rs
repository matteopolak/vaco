//! `#EXT-X-KEY` — detected and reported, never decrypted.
//!
//! The brief for this crate is explicit and this module is the boundary it
//! draws: RFC 8216 §4.4.4.4 defines two decryptable methods
//! (`AES-128`, whole-segment AES-128-CBC keyed from an out-of-band `URI`, and
//! `SAMPLE-AES`, Apple's per-sample scheme where audio IVs ride in timed ID3
//! metadata and video is NAL-structure-aware) and this crate implements
//! neither. What it does is parse the tag fully, surface it as
//! [`KeyInfo`] in stream metadata (`encryption`, `encryption_key_uri`,
//! `encryption_iv`, `encryption_keyformat`), and fail
//! [`vaco_format_core::Demuxer::read_packet`] the moment it would need to
//! decrypt a byte, with a message naming the method and the key URI rather
//! than a generic parse failure — so a caller sees "this needs AES-128 with
//! key `https://…/key.bin`", not "corrupt MPEG-TS".

use vaco_core::Error;

/// One `#EXT-X-KEY` tag, fully parsed and never acted on beyond that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// `METHOD`: `AES-128`, `SAMPLE-AES`, `SAMPLE-AES-CTR`, or (rare, DRM)
    /// something vendor-specific. `NONE` is represented as no [`KeyInfo`] at
    /// all rather than a variant, matching RFC 8216's own "applies no
    /// encryption" reading.
    pub method: String,
    pub uri: Option<String>,
    /// `IV`, as the 32 hex digits after `0x`/`0X`, still hex text — this
    /// crate never needs the binary form since it never decrypts.
    pub iv: Option<String>,
    pub key_format: Option<String>,
    pub key_format_versions: Option<String>,
}

impl KeyInfo {
    /// The [`vaco_core::Error`] reading a segment under this key must fail
    /// with. One place, so the message is the same whichever call site
    /// discovers a segment needs decryption.
    #[must_use]
    pub fn unsupported_error(&self) -> Error {
        // `Error::Unsupported` takes a `&'static str`, so the specifics (which
        // method, which URI) cannot be interpolated into it without leaking
        // the string — recorded here as a real, reported gap: see this
        // crate's docs file, "What the frozen Error type cannot say".
        match self.method.as_str() {
            "SAMPLE-AES" | "SAMPLE-AES-CTR" => {
                Error::Unsupported("HLS SAMPLE-AES segments are not decrypted")
            }
            _ => Error::Unsupported("HLS AES-128 segments are not decrypted"),
        }
    }

    /// Render as the metadata entries a [`vaco_format_core::Stream`] carries
    /// this information under.
    #[must_use]
    pub fn metadata_entries(&self) -> Vec<(String, String)> {
        let mut out = vec![("encryption".to_owned(), self.method.clone())];
        if let Some(uri) = &self.uri {
            out.push(("encryption_key_uri".to_owned(), uri.clone()));
        }
        if let Some(iv) = &self.iv {
            out.push(("encryption_iv".to_owned(), iv.clone()));
        }
        if let Some(kf) = &self.key_format {
            out.push(("encryption_keyformat".to_owned(), kf.clone()));
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn aes128_reports_a_named_error_not_a_generic_one() {
        let k = KeyInfo {
            method: "AES-128".to_owned(),
            uri: Some("https://example/key.bin".to_owned()),
            iv: None,
            key_format: None,
            key_format_versions: None,
        };
        assert!(matches!(k.unsupported_error(), Error::Unsupported(_)));
        assert_eq!(
            k.metadata_entries(),
            vec![
                ("encryption".to_owned(), "AES-128".to_owned()),
                (
                    "encryption_key_uri".to_owned(),
                    "https://example/key.bin".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn sample_aes_gets_its_own_message() {
        let k = KeyInfo {
            method: "SAMPLE-AES".to_owned(),
            uri: None,
            iv: None,
            key_format: None,
            key_format_versions: None,
        };
        match k.unsupported_error() {
            Error::Unsupported(msg) => assert!(msg.contains("SAMPLE-AES")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
