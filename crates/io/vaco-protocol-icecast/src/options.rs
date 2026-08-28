//! `-h protocol=icecast`'s option surface, measured against `ffmpeg 8.1`:
//!
//! ```text
//! icecast AVOptions:
//!   -ice_genre         <string>     E.......... set stream genre
//!   -ice_name          <string>     E.......... set stream description
//!   -ice_description   <string>     E.......... set stream description
//!   -ice_url           <string>     E.......... set stream website
//!   -ice_public        <boolean>    E.......... set if stream is public (default false)
//!   -user_agent        <string>     E.......... override User-Agent header
//!   -password          <string>     E.......... set password
//!   -content_type      <string>     E.......... set content-type, MUST be set if not audio/mpeg
//!   -legacy_icecast    <boolean>    E.......... use legacy SOURCE method, for Icecast < v2.4 (default false)
//!   -tls               <boolean>    E.......... use a TLS connection (default false)
//! ```
//!
//! Every option is `E`-only (encoding/write), matching `icecast:` being
//! output-only (`-protocols` lists it under `Output:` and not `Input:`).

use vaco_opts::Options;

/// `-h protocol=icecast`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "icecast", help = "Icecast source client")]
pub struct IcecastOptions {
    #[opt(
        name = "ice_genre",
        help = "set stream genre",
        default = String::new(),
        default_repr = "",
        flags(encoding)
    )]
    pub genre: String,

    /// Measured: this becomes the `Ice-Name` header, despite the help text
    /// saying "set stream description" — the same help text `-ice_description`
    /// has. The header name, not the help text, is what a server sees.
    #[opt(
        name = "ice_name",
        help = "set stream description",
        default = String::new(),
        default_repr = "",
        flags(encoding)
    )]
    pub name: String,

    #[opt(
        name = "ice_description",
        help = "set stream description",
        default = String::new(),
        default_repr = "",
        flags(encoding)
    )]
    pub description: String,

    #[opt(
        name = "ice_url",
        help = "set stream website",
        default = String::new(),
        default_repr = "",
        flags(encoding)
    )]
    pub url: String,

    #[opt(
        name = "ice_public",
        help = "set if stream is public",
        default = false,
        flags(encoding)
    )]
    pub public: bool,

    #[opt(
        name = "user_agent",
        help = "override User-Agent header",
        default = String::new(),
        default_repr = "Lavf/<version>",
        flags(encoding)
    )]
    pub user_agent: String,

    /// Overridden by userinfo in the URL — measured: `[icecast @ ...]
    /// Overwriting -password <pass> with URI password!` is logged when both
    /// are given.
    #[opt(
        name = "password",
        help = "set password",
        default = String::new(),
        default_repr = "",
        flags(encoding)
    )]
    pub password: String,

    #[opt(
        name = "content_type",
        help = "set content-type, MUST be set if not audio/mpeg",
        default = String::new(),
        default_repr = "audio/mpeg",
        flags(encoding)
    )]
    pub content_type: String,

    /// `SOURCE` (measured: no `Expect: 100-continue`, body sent immediately)
    /// versus the modern default, `PUT` (measured: `Expect: 100-continue`
    /// sent, and the body is genuinely held back until a `100` response —
    /// confirmed by a fake server that never answers `100`, which then never
    /// receives the body at all).
    #[opt(
        name = "legacy_icecast",
        help = "use legacy SOURCE method, for Icecast < v2.4",
        default = false,
        flags(encoding)
    )]
    pub legacy: bool,

    #[opt(
        name = "tls",
        help = "use a TLS connection",
        default = false,
        flags(encoding)
    )]
    pub tls: bool,
}
