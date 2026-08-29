//! `-h protocol=dtls`'s option surface — the parts this crate implements.
//!
//! Names, types and defaults are copied from `ffmpeg -h protocol=dtls` (8.1),
//! read as observed behaviour of a shipped binary (D6/D7/D17):
//!
//! ```text
//! dtls AVOptions:
//!   -listen            <int>        ED......... Listen for incoming connections (from 0 to 1) (default 0)
//!   -http_proxy        <string>     ED......... Set proxy to tunnel through
//!   -external_sock     <boolean>    ED......... Use external socket (default false)
//!   -use_srtp          <boolean>    ED......... Enable use_srtp DTLS extension (default false)
//!   -mtu               <int>        ED......... Maximum Transmission Unit (from 0 to INT_MAX) (default 0)
//!   -cert_pem          <string>     ED......... Certificate PEM string
//!   -key_pem           <string>     ED......... Private key PEM string
//!   -ca_file           <string>     ED......... Certificate Authority database file
//!   -cafile            <string>     ED......... Certificate Authority database file
//!   -tls_verify        <boolean>    ED......... Verify the peer certificate (default false)
//!   -verify            <boolean>    ED......... Verify the peer certificate (default false)
//!   -cert_file         <string>     ED......... Certificate file
//!   -cert              <string>     ED......... Certificate file
//!   -key_file          <string>     ED......... Private key file
//!   -key               <string>     ED......... Private key file
//!   -verifyhost        <string>     ED......... Verify against a specific hostname
//! ```
//!
//! Unlike `-h protocol=tls`, there is no separate `-listen_timeout` in this
//! table at all — measured, not assumed. `-listen` waiting for a peer is
//! therefore bounded by [`vaco_protocol_core::ProtocolEnv::rw_timeout`]
//! here (`None` waits indefinitely), the same generic knob every other
//! protocol in this workspace uses, rather than a second option this crate
//! would otherwise have to invent.
//!
//! See the crate docs for what this crate accepts but does not implement
//! (`-http_proxy`, `-external_sock`) and why.

use vaco_opts::Options;

/// `-h protocol=dtls`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "dtls", help = "DTLS transport")]
pub struct DtlsOptions {
    /// `-listen 1`: bind and accept a peer instead of connecting to one.
    /// Also settable via [`vaco_protocol_core::IoFlags::listen`] — either is
    /// honoured, matching `vaco-protocol-socket`'s `tcp:`/`unix:`.
    #[opt(
        name = "listen",
        help = "Listen for incoming connections",
        default = 0,
        range = 0..=1,
        flags(param)
    )]
    pub listen: i32,

    /// Enable the `use_srtp` DTLS extension (RFC 5764) and offer
    /// `SRTP_AES128_CM_SHA1_80`. Callers that need the exported keying
    /// material for SRTP itself use
    /// [`crate::connect::export_srtp_keying_material`] after the handshake
    /// completes — SRTP framing/encryption is `vaco-protocol-srtp`'s job,
    /// not this crate's.
    #[opt(
        name = "use_srtp",
        help = "Enable use_srtp DTLS extension",
        default = false,
        flags(param)
    )]
    pub use_srtp: bool,

    /// DTLS handshake fragment size. `0` (the default) leaves OpenSSL's own
    /// default in place.
    #[opt(
        name = "mtu",
        help = "Maximum Transmission Unit",
        default = 0,
        range = 0..=i32::MAX,
        flags(param)
    )]
    pub mtu: i32,

    /// A literal certificate, PEM-encoded. Takes priority over `cert_file`.
    #[opt(
        name = "cert_pem",
        help = "Certificate PEM string",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub cert_pem: String,

    /// The certificate's private key, PEM-encoded. Takes priority over
    /// `key_file`.
    #[opt(
        name = "key_pem",
        help = "Private key PEM string",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub key_pem: String,

    /// A certificate file, read from the local filesystem — see the crate
    /// docs' security note (same class of option as `vaco-protocol-tls`'s
    /// `ca_file`: never taken from a URL's own query string).
    #[opt(
        name = "cert_file",
        alias = "cert",
        help = "Certificate file",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub cert_file: String,

    /// The certificate file's private key, read from the local filesystem.
    #[opt(
        name = "key_file",
        alias = "key",
        help = "Private key file",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub key_file: String,

    /// Verify the peer certificate. **Defaults to `false`, matching the
    /// reference exactly** — DTLS peers routinely present a self-signed,
    /// ephemeral certificate (this crate generates one itself when none is
    /// configured — see `crate::cert`), trusted out-of-band via a fingerprint
    /// exchanged over signalling (SDP `a=fingerprint`) rather than a CA
    /// chain, so requiring `verify = true` by default would refuse the
    /// common case outright.
    #[opt(
        name = "verify",
        alias = "tls_verify",
        help = "Verify the peer certificate",
        default = false,
        flags(param)
    )]
    pub verify: bool,

    /// A PEM file of additional trusted CA certificates, appended to the
    /// system root store. Only consulted when `verify` is set.
    #[opt(
        name = "ca_file",
        alias = "cafile",
        help = "Certificate Authority database file",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub ca_file: String,

    /// Verify the peer certificate against this hostname instead of the one
    /// in the URL. Only consulted when `verify` is set — and, in practice,
    /// rarely useful for DTLS, whose peers are usually addressed by IP with
    /// no meaningful hostname to check (kept for interface parity with the
    /// reference and with `vaco-protocol-tls`).
    #[opt(
        name = "verifyhost",
        help = "Verify against a specific hostname",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub verifyhost: String,
}
