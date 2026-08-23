//! Certificate verification policy, and the reference's measured default.
//!
//! # The default is `verify = false`, and here is exactly what that means
//!
//! `ffmpeg -h protocol=tls` prints `-tls_verify <boolean> ... (default
//! false)` — measured, not assumed (D6/D7/D17; see the crate docs for the
//! exact probe). Matching a security-relevant default from a reference
//! implementation is worth pausing on, so here is the precise, deliberately
//! narrow thing this crate does when `verify` is left at that default:
//!
//! * The certificate's **trust chain is not checked** (it may be
//!   self-signed, expired, or issued by an unknown CA).
//! * The certificate's **hostname is not checked** against the connection
//!   target.
//! * The TLS handshake's **cryptographic signature is still verified** —
//!   [`PermissiveVerifier`] calls [`rustls::crypto::verify_tls12_signature`]/
//!   [`verify_tls13_signature`](rustls::crypto::verify_tls13_signature)
//!   against the offered certificate's own public key, using the shared
//!   provider's supported algorithms. A passive attacker cannot silently
//!   splice bytes into the stream — they would need to also possess (or
//!   forge, which the signature check catches) a private key matching
//!   *some* certificate the client accepts, which is exactly a chain- and
//!   host-blind man-in-the-middle: possible, and exactly what `-verify 1`
//!   closes off, but not the same failure mode as no TLS at all.
//!
//! This is the same shape several other TLS libraries' own
//! "accept-invalid-certs" escape hatches take (checking the handshake
//! signature but not the certificate's claims) — chosen here because it is
//! the closest honest match to what "verify the peer certificate: false"
//! plausibly means in the reference's own OpenSSL-backed implementation
//! (`SSL_VERIFY_NONE` disables chain/host checking; the TLS record layer's
//! own cryptographic integrity is a property of the protocol, not of that
//! flag), not because this crate independently decided a softer default was
//! safe. **A caller that wants real certificate validation must pass
//! `-verify 1` (or `-tls_verify 1`) explicitly.**

use std::sync::Arc;

use rustls::DigitallySignedStruct;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, Error as TlsError, RootCertStore, SignatureScheme};
use vaco_protocol_core::{ProtocolError, Result};

use crate::options::TlsOptions;

/// Cryptographically checks the handshake signature; does not check the
/// certificate's trust chain or hostname. See the module docs.
#[derive(Debug)]
pub struct PermissiveVerifier {
    provider: Arc<CryptoProvider>,
}

impl PermissiveVerifier {
    #[must_use]
    pub const fn new(provider: Arc<CryptoProvider>) -> Self {
        Self { provider }
    }
}

impl ServerCertVerifier for PermissiveVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build the `rustls::ClientConfig` `-verify`/`-ca_file` describe.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `-ca_file` was set but named no parsable
/// certificate, or if building the standard verifier failed (an internal
/// `rustls` configuration error — not a certificate content problem, which
/// is decided per-connection, not per-config).
pub fn client_config(opts: &TlsOptions, ca_pem: Option<&str>) -> Result<Arc<ClientConfig>> {
    let provider = crate::crypto::shared_provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| ProtocolError::Malformed {
            scheme: "tls",
            detail: "no TLS protocol version is enabled in this build",
        })?;

    let config = if opts.verify {
        let roots: RootCertStore = match ca_pem {
            Some(pem) => crate::roots::roots_with_ca_pem(pem)?,
            None => crate::roots::default_roots(),
        };
        let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|_| ProtocolError::Malformed {
                scheme: "tls",
                detail: "could not build the certificate verifier",
            })?;
        builder.with_webpki_verifier(verifier).with_no_client_auth()
    } else {
        let verifier = Arc::new(PermissiveVerifier::new(provider));
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth()
    };
    Ok(Arc::new(config))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn unverified_config_builds() {
        let opts = TlsOptions::default();
        assert!(client_config(&opts, None).is_ok());
    }

    #[test]
    fn verified_config_builds_with_default_roots() {
        let opts = TlsOptions {
            verify: true,
            ..TlsOptions::default()
        };
        assert!(client_config(&opts, None).is_ok());
    }

    #[test]
    fn verified_config_with_a_bad_ca_file_is_an_error_not_a_panic() {
        let opts = TlsOptions {
            verify: true,
            ..TlsOptions::default()
        };
        assert!(client_config(&opts, Some("not a certificate")).is_err());
    }
}
