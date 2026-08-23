//! `-h protocol=tls`'s option surface — the parts this crate implements.
//!
//! Names and the `verify`/`tls_verify` default are copied from
//! `ffmpeg -h protocol=tls` (8.1), read as observed behaviour of a shipped
//! binary (D6/D7/D17). See the crate docs for the full list of options this
//! crate accepts but does not implement (client certificates, `-listen`,
//! `-http_proxy`, `-external_sock`, `-use_srtp`, `-mtu`) and why.

use vaco_opts::Options;

/// `-h protocol=tls`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "tls", help = "TLS transport")]
pub struct TlsOptions {
    /// Verify the peer certificate. **Defaults to `false`, matching the
    /// reference exactly** — see [`crate::verify`]'s module docs for what
    /// that default still does and does not check.
    #[opt(
        name = "verify",
        alias = "tls_verify",
        help = "Verify the peer certificate",
        default = false,
        flags(param)
    )]
    pub verify: bool,

    /// A PEM file of additional trusted CA certificates, appended to the
    /// built-in `webpki-roots` set (never replacing it).
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
    /// in the URL. Only consulted when `verify` is set.
    #[opt(
        name = "verifyhost",
        help = "Verify against a specific hostname",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub verifyhost: String,
}
