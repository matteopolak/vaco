//! The `http:`/`https:` option surface.
//!
//! # Where the values came from
//!
//! Not from a plan and not from memory: from `ffprobe -h protocol=http` on the
//! pinned reference (8.1), and from `ffprobe -v debug http://…` against a local
//! `http.server` whose request log is readable — read as black-box observed
//! behaviour of a shipped binary, which is exactly what D6/D7 permit. The
//! default request line and headers, the `Range` behaviour, the redirect
//! whitelist message, the 404/connection-refused error shapes and the
//! `-offset`/`-end_offset` → `Range: bytes=start-end` mapping were all measured
//! this way; see `docs/io/vaco-protocol-http.md` for the exact commands.
//!
//! # What is deliberately not here
//!
//! `-http_proxy`, `-listen`/`-resource`/`-reply_code` (server mode),
//! `-post_data` (one-shot CLI body injection — `Protocol::create`'s
//! [`MediaSink::write`](vaco_io::MediaSink::write) calls are this crate's
//! streaming equivalent), `-send_expect_100` (100-continue handshaking) and
//! `-request_size`/`-initial_request_size` (chunked readahead sizing) are
//! write-side-adjacent, proxy, or server-mode surface not implemented here.
//! `-chunked_post` and `-content_type` **are** implemented (`crate::post`) —
//! D5's "zero muxers" no longer means "nothing calls `Protocol::create`" now
//! that this crate's own `create()` does something, though nothing *else* in
//! the project calls it yet either. See the crate docs for the full list of
//! scoped-out options and `crate::post`'s docs for what "chunked POST" means
//! here specifically.

use vaco_opts::{ConstDesc, Options};

/// Named constants for `auth_type`. See the reference's own naming.
pub const AUTH_TYPE_CONSTS: &[ConstDesc] = &[
    ConstDesc::new("none", "No auth method set, autodetect", "auth_type", 0),
    ConstDesc::new("basic", "HTTP basic authentication", "auth_type", 1),
];

/// `-auth_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthType {
    /// Autodetect. We do not probe a `WWW-Authenticate` challenge (that would
    /// need a failed round trip first); autodetect behaves as `None` until the
    /// caller asks for `Basic` explicitly, or the URL carries `user:pass@`.
    #[default]
    None = 0,
    /// Send `Authorization: Basic <base64(user:pass)>` up front.
    Basic = 1,
}

impl AuthType {
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Basic,
            _ => Self::None,
        }
    }
}

/// Options of the `http:`/`https:` protocols. Names are interface facts (D9).
///
/// Declaration order follows `ffprobe -h protocol=http`'s listing, for the
/// entries this crate implements — the reference's own order is not
/// alphabetical and reproducing it makes an `-h protocol=http` diff readable.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option name, matching vaco-format-core's FormatOptions"
)]
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "http", help = "HTTP/HTTPS transport")]
pub struct HttpOptions {
    /// `auto`: probe seekability from the first response (206 vs 200).
    /// `true`/`false` force the answer regardless of what the server says;
    /// see [`Seekable`] and the crate docs' "what a server that ignores Range
    /// looks like" section for the failure mode a forced `true` accepts.
    #[opt(
        name = "seekable",
        help = "control seekability of connection",
        default = -1,
        range = -1..=1,
        flags(decoding)
    )]
    pub seekable: i32,

    /// Raw `key: value\r\n`-separated header block. Measured: it can override
    /// a built-in default header (same name, case-insensitive) or add a new
    /// one; it never removes one outright.
    #[opt(
        name = "headers",
        help = "set custom HTTP headers, can override built in default headers",
        default = String::new(),
        default_repr = "",
        flags(decoding)
    )]
    pub headers: String,

    /// Overrides the default `User-Agent`. `vaco-protocol-http` has no
    /// business claiming to be `Lavf/…`, so our own default is a Vaco string
    /// rather than a copy of the reference's (D7: interface facts are free,
    /// the reference's own product string is not one).
    #[opt(
        name = "user_agent",
        help = "override User-Agent header",
        default = String::new(),
        default_repr = "Vaco/<version>",
        flags(decoding)
    )]
    pub user_agent: String,

    /// Overrides the `Referer` header. Absent by default, matching the
    /// reference (no `Referer` line appeared in any measured request).
    #[opt(
        name = "referer",
        help = "override referer header",
        default = String::new(),
        default_repr = "",
        flags(decoding)
    )]
    pub referer: String,

    /// `Connection: keep-alive` and connection reuse across opens of the same
    /// host, versus the default `Connection: close` (a fresh TCP connection,
    /// and for HTTPS a fresh TLS handshake, per request). Measured: the
    /// `Connection` header value flips exactly on this option.
    #[opt(
        name = "multiple_requests",
        help = "use persistent connections",
        default = false,
        flags(decoding)
    )]
    pub multiple_requests: bool,

    /// Newline-delimited `name=value[; …]` lines, `Set-Cookie`-field syntax.
    /// Sent back as a single `Cookie:` header; see [`crate::parse::cookie_header`].
    #[opt(
        name = "cookies",
        help = "set cookies to be sent in applicable future requests, use newline delimited \
                Set-Cookie HTTP field value syntax",
        default = String::new(),
        default_repr = "",
        flags(decoding)
    )]
    pub cookies: String,

    /// `Icy-MetaData: 1`. We request it for fidelity; parsing the interleaved
    /// ICY metadata out of the body is not implemented (see crate docs).
    #[opt(
        name = "icy",
        help = "request ICY metadata",
        default = true,
        flags(decoding)
    )]
    pub icy: bool,

    /// HTTP authentication. `none` (autodetect) or `basic`.
    #[opt(
        name = "auth_type",
        help = "HTTP authentication type",
        unit = "auth_type",
        consts = AUTH_TYPE_CONSTS,
        default = 0,
        default_repr = "none",
        range = 0..=1,
        flags(decoding)
    )]
    pub auth_type: i32,

    /// Initial byte offset. Folded into the first `Range` request rather than
    /// satisfied by reading and discarding — that is the entire point of a
    /// ranged read.
    #[opt(
        name = "offset",
        help = "initial byte offset",
        default = 0_i64,
        range = 0_i64..=i64::MAX,
        flags(decoding)
    )]
    pub offset: i64,

    /// Upper bound (exclusive) on the byte range requested. Zero means
    /// unbounded. Measured: `-offset 100 -end_offset 200` produced
    /// `Range: bytes=100-199` — `end_offset` is the byte one past the last one
    /// requested, matching a half-open range.
    #[opt(
        name = "end_offset",
        help = "try to limit the request to bytes preceding this offset",
        default = 0_i64,
        range = 0_i64..=i64::MAX,
        flags(decoding)
    )]
    pub end_offset: i64,

    /// Auto-reconnect after the connection drops before EOF.
    #[opt(
        name = "reconnect",
        help = "auto reconnect after disconnect before EOF",
        default = false,
        flags(decoding)
    )]
    pub reconnect: bool,

    /// Auto-reconnect at EOF, for a source that may still grow (a live
    /// segment being appended to).
    #[opt(
        name = "reconnect_at_eof",
        help = "auto reconnect at EOF",
        default = false,
        flags(decoding)
    )]
    pub reconnect_at_eof: bool,

    /// Auto-reconnect when the *connect* attempt itself fails (TCP refused,
    /// TLS handshake failure) rather than only when an established stream
    /// drops.
    #[opt(
        name = "reconnect_on_network_error",
        help = "auto reconnect in case of tcp/tls error during connect",
        default = false,
        flags(decoding)
    )]
    pub reconnect_on_network_error: bool,

    /// Comma-separated HTTP status codes to reconnect on (e.g. `503,504`).
    /// Parsed by [`crate::parse::reconnect_codes`].
    #[opt(
        name = "reconnect_on_http_error",
        help = "list of http status codes to reconnect on",
        default = String::new(),
        default_repr = "",
        flags(decoding)
    )]
    pub reconnect_on_http_error: String,

    /// Auto-reconnect a forward-only (non-seekable) stream. Without this, a
    /// dropped non-seekable stream cannot be resumed at all (there is no
    /// position to resume *from* in the protocol sense, but we track the byte
    /// count read and re-request from there).
    #[opt(
        name = "reconnect_streamed",
        help = "auto reconnect streamed / non seekable streams",
        default = false,
        flags(decoding)
    )]
    pub reconnect_streamed: bool,

    /// Cap on the exponential backoff between one reconnect attempt and the
    /// next, in seconds.
    #[opt(
        name = "reconnect_delay_max",
        help = "max reconnect delay in seconds after which to give up",
        default = 120,
        range = 0..=4294,
        flags(decoding)
    )]
    pub reconnect_delay_max: i32,

    /// Cap on the number of reconnect attempts. `-1` means unlimited.
    #[opt(
        name = "reconnect_max_retries",
        help = "the max number of times to retry a connection",
        default = -1,
        range = -1..=i32::MAX,
        flags(decoding)
    )]
    pub reconnect_max_retries: i32,

    /// Cap on the *total* time spent across every reconnect wait, in seconds.
    #[opt(
        name = "reconnect_delay_total_max",
        help = "max total reconnect delay in seconds after which to give up",
        default = 256,
        range = 0..=4294,
        flags(decoding)
    )]
    pub reconnect_delay_total_max: i32,

    /// Honour a numeric `Retry-After` header on a reconnect-eligible response
    /// instead of our own backoff schedule.
    #[opt(
        name = "respect_retry_after",
        help = "respect the Retry-After header when retrying connections",
        default = true,
        flags(decoding)
    )]
    pub respect_retry_after: bool,

    /// Below this many bytes, a forward seek reads and discards instead of
    /// opening a new connection. Zero (the reference default, and ours)
    /// disables the optimisation: every seek is a new request. See
    /// `docs/io/vaco-protocol-http.md` for the measured evidence that the
    /// reference itself defaults to "always reconnect".
    #[opt(
        name = "short_seek_size",
        help = "threshold to favor readahead over seek",
        default = 0,
        range = 0..=i32::MAX,
        flags(decoding)
    )]
    pub short_seek_size: i32,

    /// Redirect hops permitted before giving up. `0` means a redirect
    /// response is itself an error — measured directly against the reference.
    #[opt(
        name = "max_redirects",
        help = "Maximum number of redirects",
        default = 8,
        range = 0..=i32::MAX,
        flags(decoding)
    )]
    pub max_redirects: i32,

    /// Use chunked transfer-encoding for `Protocol::create`'s POST body.
    /// Measured default: `true`. `false` (a fixed-length, `Content-Length`
    /// POST) is accepted but not implemented — see the crate docs.
    #[opt(
        name = "chunked_post",
        help = "use chunked transfer-encoding for posts",
        default = true,
        flags(encoding)
    )]
    pub chunked_post: bool,

    /// `Content-Type` for a POST body. Empty means: send none (matching the
    /// reference's own "unset" default — measured: no `-h`-visible default
    /// value is printed for this option).
    #[opt(
        name = "content_type",
        help = "set a specific content type for the POST messages",
        default = String::new(),
        default_repr = "",
        flags(param)
    )]
    pub content_type: String,
}

impl HttpOptions {
    /// `seekable` as a three-way choice, rather than the raw `-1/0/1` the
    /// option table stores (`vaco-opts` has no dedicated tri-state base).
    #[must_use]
    pub const fn seekable(&self) -> Seekable {
        match self.seekable {
            0 => Seekable::Never,
            1 => Seekable::Always,
            _ => Seekable::Auto,
        }
    }

    #[must_use]
    pub const fn auth_type(&self) -> AuthType {
        AuthType::from_i32(self.auth_type)
    }
}

/// The resolved form of `-seekable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seekable {
    /// Decide from the first response: `206` is seekable, `200` is not.
    Auto,
    /// Never send `Range`; always [`vaco_io::Seekability::None`].
    Never,
    /// Always attempt `Range`, even after a `200` response. A server that
    /// keeps ignoring `Range` on a later seek is reported as an I/O error
    /// rather than silently served from the wrong offset — see
    /// [`crate::source::HttpSource::seek`].
    Always,
}
