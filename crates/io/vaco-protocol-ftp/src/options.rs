//! `-h protocol=ftp`'s option surface, measured against `ffmpeg 8.1`:
//!
//! ```text
//! ftp AVOptions:
//!   -timeout           <int>        ED......... set timeout of socket I/O operations (from -1 to INT_MAX) (default -1)
//!   -ftp-write-seekable <boolean>    E.......... control seekability of connection during encoding (default false)
//!   -ftp-anonymous-password <string>     ED......... password for anonymous login. E-mail address should be used.
//!   -ftp-user          <string>     ED......... user for FTP login. Overridden by whatever is in the URL.
//!   -ftp-password      <string>     ED......... password for FTP login. Overridden by whatever is in the URL.
//! ```
//!
//! `-ftp-user`/`-ftp-password` say "Overridden by whatever is in the URL" in
//! their own help text — [`crate::control::credentials`] applies that
//! precedence.

use vaco_opts::Options;

/// `-h protocol=ftp`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "ftp", help = "FTP (RFC 959)")]
pub struct FtpOptions {
    /// Microseconds, matching `tcp:`'s own `-timeout` convention (the
    /// reference's help text for this option does not state units, but the
    /// range and default are identical in shape to `tcp:`'s, which does).
    #[opt(
        name = "timeout",
        help = "set timeout of socket I/O operations",
        default = -1_i64,
        range = -1_i64..=i32::MAX as i64,
        flags(decoding, encoding)
    )]
    pub timeout: i64,

    /// Write-only: whether `create`'s `MediaSink` reports itself seekable.
    #[opt(
        name = "ftp-write-seekable",
        help = "control seekability of connection during encoding",
        default = false,
        flags(encoding)
    )]
    pub write_seekable: bool,

    /// Password sent when the resolved user is `anonymous` and no explicit
    /// password was otherwise given. Measured default when this is also
    /// unset: the literal `nopassword`, not an email address.
    #[opt(
        name = "ftp-anonymous-password",
        help = "password for anonymous login. E-mail address should be used.",
        default = String::new(),
        default_repr = "nopassword",
        flags(decoding, encoding)
    )]
    pub anonymous_password: String,

    /// Overridden by userinfo in the URL.
    #[opt(
        name = "ftp-user",
        help = "user for FTP login. Overridden by whatever is in the URL.",
        default = String::new(),
        default_repr = "anonymous",
        flags(decoding, encoding)
    )]
    pub user: String,

    /// Overridden by userinfo in the URL.
    #[opt(
        name = "ftp-password",
        help = "password for FTP login. Overridden by whatever is in the URL.",
        default = String::new(),
        default_repr = "",
        flags(decoding, encoding)
    )]
    pub password: String,
}
