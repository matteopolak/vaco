//! The trusted root store: `webpki-roots`, optionally extended by `-ca_file`.
//!
//! `-ca_file`'s certificates are **appended** to the built-in set, never
//! substituted for it — matching the reference's own `-ca_file` semantics
//! (adding a private CA does not stop a build from trusting the public web),
//! and matching `vaco-protocol-http`'s crate docs' note that `webpki-roots`
//! is Mozilla's own bundle, repackaged, and already widely trusted.

use rustls::RootCertStore;
use vaco_protocol_core::{ProtocolError, Result};

/// The built-in root store alone, cloned fresh each call.
///
/// `RootCertStore` is not `Clone`-cheap in the sense of sharing storage — it
/// owns its trust anchors — but `webpki_roots::TLS_SERVER_ROOTS` is a
/// `'static` slice, so rebuilding from it is a fixed, small cost, not a
/// network fetch or a parse.
#[must_use]
pub fn default_roots() -> RootCertStore {
    webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect()
}

/// The default roots plus every certificate in `ca_pem`, a PEM file's
/// contents.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `ca_pem` names no parsable certificate at
/// all (an empty or garbage `-ca_file` is almost certainly a caller mistake
/// worth surfacing, rather than a silent "no roots added"). A `ca_pem` that
/// parses *some* certificates and fails on a later one still returns the
/// roots recovered from the ones before it — see
/// [`rustls::RootCertStore::add_parsable_certificates`]'s own "ignore
/// unparsable" contract, which this function inherits rather than tightens.
pub fn roots_with_ca_pem(ca_pem: &str) -> Result<RootCertStore> {
    let ders = crate::pem::extract_der_blocks(ca_pem, "CERTIFICATE")?;
    if ders.is_empty() {
        return Err(ProtocolError::Malformed {
            scheme: "tls",
            detail: "ca_file contains no PEM CERTIFICATE block",
        });
    }
    let mut store = default_roots();
    let certs = ders
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from);
    let (_added, _ignored) = store.add_parsable_certificates(certs);
    Ok(store)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn default_roots_are_non_empty() {
        assert!(!default_roots().is_empty());
    }

    #[test]
    fn empty_ca_file_is_an_error() {
        assert!(roots_with_ca_pem("").is_err());
        assert!(roots_with_ca_pem("not a certificate").is_err());
    }

    #[test]
    fn a_garbage_der_block_does_not_panic() {
        // A syntactically valid PEM wrapper around bytes that are not a
        // certificate at all: `add_parsable_certificates` must reject it
        // without touching the roots already loaded, and this function must
        // not panic either way.
        let text = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        let _ = roots_with_ca_pem(text);
    }
}
