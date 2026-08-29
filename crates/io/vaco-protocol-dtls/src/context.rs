//! Building an `openssl::ssl::SslContext` from [`DtlsOptions`].
//!
//! One function, shared by both the client (`connect`) and server (`listen`)
//! paths — the only difference between them is which of [`openssl::ssl::Ssl::connect`]
//! / [`openssl::ssl::Ssl::accept`] the caller drives afterwards.

use openssl::ssl::{SslContext, SslContextBuilder, SslMethod, SslVerifyMode};
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::X509;
use vaco_protocol_core::{ProtocolError, Result};

use crate::options::DtlsOptions;

/// The single default SRTP protection profile this crate offers when
/// `-use_srtp` is set. RFC 5764 lets a handshake negotiate any number of
/// profiles; offering exactly one keeps this crate's own surface small and
/// matches every other option here in only implementing what has a caller
/// today (`vaco-protocol-srtp` already implements this profile's cipher —
/// AES-CTR + HMAC-SHA1-80 — independently, so the two agree without either
/// depending on the other).
const SRTP_PROFILE: &str = "SRTP_AES128_CM_SHA1_80";

fn openssl_err(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: "dtls",
        detail,
    }
}

/// The certificate/key a caller configured, or a freshly generated
/// self-signed one — see [`crate::cert`]'s module docs for why generating
/// one is the right default rather than an error.
fn resolve_identity(opts: &DtlsOptions, cert_file_pem: Option<&str>, key_file_pem: Option<&str>) -> Result<(X509, openssl::pkey::PKey<openssl::pkey::Private>)> {
    if !opts.cert_pem.is_empty() && !opts.key_pem.is_empty() {
        return crate::cert::parse_pem(&opts.cert_pem, &opts.key_pem);
    }
    if let (Some(cert_pem), Some(key_pem)) = (cert_file_pem, key_file_pem) {
        return crate::cert::parse_pem(cert_pem, key_pem);
    }
    crate::cert::generate_self_signed()
}

/// Build the `SslContext` `-verify`/`-cert_pem`/`-use_srtp`/`-mtu` describe.
///
/// `cert_file_pem`/`key_file_pem` are the already-read contents of
/// `-cert_file`/`-key_file`, if set — read by the caller so this function
/// never touches the filesystem itself (same separation `vaco-protocol-tls`
/// keeps between `read_ca_file` and `client_config`).
///
/// # Errors
/// [`ProtocolError::Malformed`] if the configured certificate/key do not
/// parse, if `-ca_file` was set but named no parsable certificate, or if
/// building the `SslContext` itself failed.
pub fn build(
    opts: &DtlsOptions,
    cert_file_pem: Option<&str>,
    key_file_pem: Option<&str>,
    ca_file_pem: Option<&str>,
) -> Result<SslContext> {
    let mut builder = SslContextBuilder::new(SslMethod::dtls())
        .map_err(|_| openssl_err("could not create a DTLS context"))?;

    let (cert, pkey) = resolve_identity(opts, cert_file_pem, key_file_pem)?;
    builder
        .set_certificate(&cert)
        .map_err(|_| openssl_err("could not attach the certificate"))?;
    builder
        .set_private_key(&pkey)
        .map_err(|_| openssl_err("could not attach the private key"))?;

    if opts.verify {
        builder.set_verify(SslVerifyMode::PEER);
        if let Some(ca_pem) = ca_file_pem {
            let mut store = X509StoreBuilder::new()
                .map_err(|_| openssl_err("could not start a certificate store"))?;
            let ca_certs = X509::stack_from_pem(ca_pem.as_bytes())
                .map_err(|_| openssl_err("ca_file is not a valid PEM certificate bundle"))?;
            if ca_certs.is_empty() {
                return Err(openssl_err("ca_file contains no PEM CERTIFICATE block"));
            }
            for ca_cert in ca_certs {
                store
                    .add_cert(ca_cert)
                    .map_err(|_| openssl_err("could not add a certificate to the trust store"))?;
            }
            builder.set_cert_store(store.build());
        }
        // No ca_file: fall through to OpenSSL's own default (system) store,
        // exactly like `-verify 1` with no `-ca_file` in `vaco-protocol-tls`.
    } else {
        // Matches the reference's own measured default (`-verify`/`-tls_verify`
        // default false): the handshake still authenticates the peer
        // cryptographically (a passive attacker cannot complete it without
        // the matching private key), it just does not check the
        // certificate's trust chain or hostname. See `crate` docs.
        builder.set_verify(SslVerifyMode::NONE);
    }

    if opts.use_srtp {
        builder
            .set_tlsext_use_srtp(SRTP_PROFILE)
            .map_err(|_| openssl_err("could not enable the use_srtp extension"))?;
    }

    Ok(builder.build())
}

/// Apply `-mtu` to a freshly created `Ssl`, if set (`0` leaves OpenSSL's own
/// default).
///
/// # Errors
/// [`ProtocolError::Malformed`] if OpenSSL rejected the value.
pub fn apply_mtu(ssl: &mut openssl::ssl::SslRef, mtu: i32) -> Result<()> {
    if mtu <= 0 {
        return Ok(());
    }
    let mtu = u32::try_from(mtu).unwrap_or(u32::MAX);
    ssl.set_mtu(mtu)
        .map_err(|_| openssl_err("could not set the DTLS MTU"))
}
