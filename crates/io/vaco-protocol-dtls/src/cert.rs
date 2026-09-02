//! Loading a configured certificate/key, or generating an ephemeral
//! self-signed one when none is configured.
//!
//! # Why a self-signed default, not an error
//!
//! DTLS almost never authenticates via a CA chain the way HTTPS does: a
//! WebRTC/WHIP peer presents a certificate whose *fingerprint* was already
//! exchanged out-of-band (SDP `a=fingerprint`), and the DTLS handshake only
//! has to prove the peer holds the private key matching that fingerprint —
//! any certificate will do, so both browsers and `ffmpeg`'s own `dtls.c`
//! (measured: `ffmpeg -listen 1 -f data -i dtls://127.0.0.1:0 -f null -`
//! attempts a real handshake rather than refusing for "no certificate
//! configured") generate one on the fly when the caller supplies none.
//! Refusing to open `dtls:` without an explicit certificate would refuse the
//! common case this protocol exists for.

use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::{X509, X509NameBuilder};
use vaco_protocol_core::{ProtocolError, Result};

fn openssl_err(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: "dtls",
        detail,
    }
}

/// Generate a fresh ECDSA P-256 self-signed certificate, valid for a decade
/// — comfortably longer than any single process's lifetime, so clock skew
/// between peers is never the reason a handshake fails.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the underlying OpenSSL calls fail (out of
/// entropy, or an internal allocation failure — not a caller input error,
/// since nothing here is caller-controlled).
pub fn generate_self_signed() -> Result<(X509, PKey<Private>)> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)
        .map_err(|_| openssl_err("could not select the P-256 curve"))?;
    let ec_key =
        EcKey::generate(&group).map_err(|_| openssl_err("could not generate an EC key pair"))?;
    let pkey =
        PKey::from_ec_key(ec_key).map_err(|_| openssl_err("could not wrap the EC key pair"))?;

    let mut name_builder =
        X509NameBuilder::new().map_err(|_| openssl_err("could not start a certificate name"))?;
    name_builder
        .append_entry_by_text("CN", "vaco")
        .map_err(|_| openssl_err("could not set the certificate's common name"))?;
    let name = name_builder.build();

    let mut serial =
        BigNum::new().map_err(|_| openssl_err("could not allocate a serial number"))?;
    serial
        .rand(64, MsbOption::MAYBE_ZERO, false)
        .map_err(|_| openssl_err("could not randomise the serial number"))?;
    let serial = serial
        .to_asn1_integer()
        .map_err(|_| openssl_err("could not encode the serial number"))?;

    let mut builder = X509::builder().map_err(|_| openssl_err("could not start a certificate"))?;
    builder
        .set_version(2)
        .map_err(|_| openssl_err("could not set the certificate version"))?;
    builder
        .set_serial_number(&serial)
        .map_err(|_| openssl_err("could not set the certificate serial number"))?;
    builder
        .set_subject_name(&name)
        .map_err(|_| openssl_err("could not set the certificate subject"))?;
    builder
        .set_issuer_name(&name)
        .map_err(|_| openssl_err("could not set the certificate issuer"))?;
    builder
        .set_pubkey(&pkey)
        .map_err(|_| openssl_err("could not attach the public key"))?;
    let not_before = Asn1Time::days_from_now(0)
        .map_err(|_| openssl_err("could not set the certificate's start date"))?;
    builder
        .set_not_before(&not_before)
        .map_err(|_| openssl_err("could not set the certificate's start date"))?;
    let not_after = Asn1Time::days_from_now(3650)
        .map_err(|_| openssl_err("could not set the certificate's expiry date"))?;
    builder
        .set_not_after(&not_after)
        .map_err(|_| openssl_err("could not set the certificate's expiry date"))?;
    builder
        .sign(&pkey, MessageDigest::sha256())
        .map_err(|_| openssl_err("could not self-sign the certificate"))?;
    let cert = builder.build();

    Ok((cert, pkey))
}

/// Parse a PEM-encoded certificate and private key pair, from whichever of
/// `-cert_pem`/`-key_pem` (a literal string) or `-cert_file`/`-key_file` (a
/// path already read by the caller) supplied it.
///
/// # Errors
/// [`ProtocolError::Malformed`] if either PEM block fails to parse.
pub fn parse_pem(cert_pem: &str, key_pem: &str) -> Result<(X509, PKey<Private>)> {
    let cert = X509::from_pem(cert_pem.as_bytes())
        .map_err(|_| openssl_err("cert_pem/cert_file is not a valid PEM certificate"))?;
    let pkey = PKey::private_key_from_pem(key_pem.as_bytes())
        .map_err(|_| openssl_err("key_pem/key_file is not a valid PEM private key"))?;
    Ok((cert, pkey))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn self_signed_generation_round_trips_through_pem() {
        let (cert, key) = generate_self_signed().unwrap();
        let cert_pem = String::from_utf8(cert.to_pem().unwrap()).unwrap();
        let key_pem = String::from_utf8(key.private_key_to_pem_pkcs8().unwrap()).unwrap();
        let (cert2, _key2) = parse_pem(&cert_pem, &key_pem).unwrap();
        assert_eq!(
            cert.subject_name().to_der().unwrap(),
            cert2.subject_name().to_der().unwrap()
        );
    }

    #[test]
    fn two_generated_certificates_are_never_identical() {
        // Distinct keys and serial numbers: reusing one across connections
        // would make every session's fingerprint the same, which is exactly
        // the property a WHIP/WebRTC signalling exchange depends on being
        // unique per session.
        let (cert1, _) = generate_self_signed().unwrap();
        let (cert2, _) = generate_self_signed().unwrap();
        assert_ne!(cert1.to_pem().unwrap(), cert2.to_pem().unwrap());
    }

    #[test]
    fn garbage_pem_is_an_error_not_a_panic() {
        assert!(parse_pem("not a certificate", "not a key").is_err());
    }
}
