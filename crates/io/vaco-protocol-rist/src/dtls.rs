//! §6 (DTLS Support) — `VSF TR-06-2:2022` pp. 27-28, built on
//! `vaco-protocol-dtls` (PR-12b/#562, landed 2026-08-28).
//!
//! # What was blocking this
//!
//! The crate docs (until this module landed) named §6 "blocked and deferred
//! to PR-12" because no native Rust DTLS stack existed and `rustls` has no
//! DTLS support. `vaco-protocol-dtls` removed that blocker: a real DTLS 1.2
//! handshake over `openssl` 0.10.81 (vendored OpenSSL 3.6.3), already
//! interop-verified against `ffmpeg 8.1` acting as a `-listen 1` DTLS peer.
//! This module is the integration.
//!
//! # §6.1 — Session Establishment
//!
//! > "There shall be one single DTLS session carrying the RFC 8086 tunnel
//! > packets described earlier in this document. Once negotiation is
//! > complete, the RIST sender shall use RIST Simple Profile as per VSF
//! > TR-06-1 over the RFC 8086 tunnel, as described in Section 5."
//!
//! This is a materially different use of DTLS from WHIP's: WHIP uses the
//! handshake only to derive SRTP keying material (RFC 5764,
//! [`vaco_protocol_dtls::connect::export_srtp_keying_material`]) and then
//! carries media as SRTP, never as DTLS application data. RIST Main Profile
//! carries the [`crate::gre`]-framed tunnel bytes **as** DTLS application
//! data, directly, for the life of the session. That needs nothing this
//! crate has to build: `vaco_protocol_dtls::connect::DtlsStream` is
//! `openssl::ssl::SslStream<UdpTransport>`, which already implements
//! [`std::io::Read`]/[`std::io::Write`] one DTLS record per call (the same
//! "one read is one recv" shape `vaco-protocol-socket`'s UDP source uses) —
//! a [`crate::gre::GreHeader`]-prefixed packet serializes to bytes and is
//! written straight into the stream with no additional framing, and reads
//! back as the identical bytes on the peer, because DTLS already preserves
//! datagram boundaries (RFC 6347). See `tests/dtls_session.rs`
//! for the two-sided proof of that claim — a genuine handshake between two
//! independently driven `DtlsStream`s, not a mock.
//!
//! "The roles of DTLS Server and Client are independent of the roles of
//! RIST Sender and Receiver" — this crate imposes no coupling between them
//! either: which side calls `vaco_protocol_dtls::listen`/`connect` is a
//! deployment choice, orthogonal to which side calls
//! [`crate::gre::GreHeader::serialize`] first.
//!
//! # §6.2 — Supported DTLS Cipher Suites
//!
//! [`REQUIRED_CIPHER_SUITES`] is the mandatory five, and
//! [`negotiated_cipher_is_required_suite`] checks a completed handshake's
//! actual negotiated cipher against them — real post-handshake enforcement,
//! not a table nobody consults.
//!
//! **Known gap, filed rather than worked around:** §6.2 also requires "RIST
//! devices shall provide a means for the user to disable individual cipher
//! suites". Enforcing that means influencing *which* suite gets negotiated,
//! which needs an OpenSSL cipher-list string
//! (`SslContextBuilder::set_cipher_list`) applied before the handshake —
//! and `vaco_protocol_dtls::options::DtlsOptions`/`context::build` expose no
//! such knob today (checked directly: `context::build` never calls
//! `set_cipher_list` or `set_ciphersuites`, matching `ffmpeg -h
//! protocol=dtls`'s own option table, which has none either). Adding one is
//! a change to a crate this package does not own — flagged as a follow-up
//! rather than reached into. Until then, this module can *observe* whether
//! a completed handshake landed on a compliant suite; it cannot yet steer
//! the negotiation away from a non-compliant one.
//!
//! # §6.3 — Certificate Configuration
//!
//! Already fully expressible through `vaco_protocol_dtls::options::DtlsOptions`
//! with no gap: `cert_file`/`key_file`/`cert_pem`/`key_pem` (§6.3's "DTLS
//! server should be configured with a certificate file... issued by a CA,
//! or a self-signed one"), `verify` (§6.3's "DTLS client may validate the
//! authenticity of the certificate... shall be a user-configurable option"),
//! `ca_file` (§6.3's "certified list of CAs"), and an ephemeral self-signed
//! certificate generated automatically when none is configured
//! (`vaco_protocol_dtls::cert`) for §6.3's self-signed case. §6.3's mutual
//! (client-certificate) authentication is symmetric in this same option
//! surface — `cert_file`/`key_file` apply to either handshake role, matching
//! "The DTLS server may validate the client certificate" being phrased as
//! optional in the same way as the server-validation case this crate's
//! `verify` option already covers.
//!
//! # Not built here
//!
//! §6.4 (TLS-SRP, RFC 5054) is scoped out for the same reason Annex D was
//! filed as #657 rather than built alongside PSK: the spec states RIST
//! devices "may implement TLS-SRP... as an alternative to using
//! certificates" (optional, not mandatory), and it is a genuinely separate
//! authentication mechanism with its own cipher suites
//! (`TLS_SRP_SHA_WITH_AES_128_CBC_SHA`/`_256_CBC_SHA`) that OpenSSL does not
//! support without a non-default build flag — comparable in size to the
//! DTLS integration itself, not a wiring detail on top of it.

use vaco_protocol_dtls::connect::DtlsStream;

/// One of §6.2's five mandatory suites: the IANA/RFC name the spec itself
/// uses, paired with the name OpenSSL reports for the identical suite
/// post-handshake (`SslCipherRef::name`) — checked against this host's
/// OpenSSL 3.6.3 build via `openssl ciphers -v`, not guessed from a
/// naming-convention table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredCipherSuite {
    /// The name `TR-06-2` §6.2 lists, e.g. `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`.
    pub iana_name: &'static str,
    /// OpenSSL's name for the same suite, e.g. `ECDHE-ECDSA-AES128-GCM-SHA256`.
    pub openssl_name: &'static str,
}

/// `TR-06-2` §6.2's exact five, in the spec's own listed order.
///
/// The last entry, `TLS_RSA_WITH_NULL_SHA256`, authenticates but does not
/// encrypt at all (`NULL` cipher — zero confidentiality). The spec mandates
/// *support* for it, not that it be preferred or even enabled by default;
/// callers deciding cipher policy should treat its presence here as "the
/// spec requires devices be able to speak this if asked", not as a
/// recommendation.
pub const REQUIRED_CIPHER_SUITES: &[RequiredCipherSuite] = &[
    RequiredCipherSuite {
        iana_name: "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
        openssl_name: "ECDHE-ECDSA-AES128-GCM-SHA256",
    },
    RequiredCipherSuite {
        iana_name: "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
        openssl_name: "ECDHE-RSA-AES128-GCM-SHA256",
    },
    RequiredCipherSuite {
        iana_name: "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
        openssl_name: "ECDHE-ECDSA-AES256-GCM-SHA384",
    },
    RequiredCipherSuite {
        iana_name: "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        openssl_name: "ECDHE-RSA-AES256-GCM-SHA384",
    },
    RequiredCipherSuite {
        iana_name: "TLS_RSA_WITH_NULL_SHA256",
        openssl_name: "NULL-SHA256",
    },
];

/// Look up the OpenSSL name for one of §6.2's mandatory suites by its IANA
/// name. `None` for anything not in [`REQUIRED_CIPHER_SUITES`] — this is a
/// lookup over the mandatory five, not a general IANA-to-OpenSSL cipher
/// database.
#[must_use]
pub fn openssl_name_for(iana_name: &str) -> Option<&'static str> {
    REQUIRED_CIPHER_SUITES
        .iter()
        .find(|suite| suite.iana_name == iana_name)
        .map(|suite| suite.openssl_name)
}

/// Whether `stream`'s already-negotiated cipher is one of §6.2's five
/// mandatory suites.
///
/// This is real post-handshake enforcement a caller can act on (refuse to
/// carry traffic over a session that landed outside the mandatory set), not
/// a table that nothing consults — see the module docs for the one piece of
/// §6.2 this does *not* yet cover (steering negotiation away from a
/// non-compliant suite, which needs a knob `vaco-protocol-dtls` does not
/// expose today).
#[must_use]
pub fn negotiated_cipher_is_required_suite(stream: &DtlsStream) -> bool {
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the UFCS form names `openssl::ssl::SslCipherRef` by path, which needs `openssl` \
                  as a direct dependency of this crate — D11 reserves that to vaco-protocol-dtls \
                  alone; this crate only ever names the type through that crate's re-export"
    )]
    stream
        .ssl()
        .current_cipher()
        .map(|cipher| cipher.name())
        .is_some_and(|name| {
            REQUIRED_CIPHER_SUITES
                .iter()
                .any(|s| s.openssl_name == name)
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn looks_up_every_mandatory_suite_by_its_iana_name() {
        for suite in REQUIRED_CIPHER_SUITES {
            assert_eq!(openssl_name_for(suite.iana_name), Some(suite.openssl_name));
        }
    }

    #[test]
    fn rejects_a_suite_the_spec_does_not_mandate() {
        assert_eq!(openssl_name_for("TLS_AES_128_GCM_SHA256"), None);
    }
}
