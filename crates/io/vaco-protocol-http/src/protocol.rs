//! [`HttpProtocol`]: the `Protocol` implementation, and where redirects are
//! resolved through the whitelist.
//!
//! # The security property this module exists to hold
//!
//! A `Location` header is a URL chosen by whoever is on the other end of the
//! socket — exactly as untrusted as a URL a playlist file names (see
//! `vaco-protocol-core`'s crate docs on `ProtocolEnv`). So a redirect is
//! handled as a **new open**, not as a transport-layer detail:
//!
//! 1. `ureq` is configured with `max_redirects(0)` (`crate::transport`) — it
//!    never follows one itself.
//! 2. On a `3xx` response, [`crate::url::resolve_location`] turns the
//!    `Location` value into a full URL string (pure string logic, no trust
//!    decision).
//! 3. That string is checked against `env` — the *same* [`ProtocolEnv`] this
//!    open was itself granted, so a redirect can only reach what this open's
//!    own whitelist context already permits. A same-scheme redirect
//!    (`http:`→`https:`) continues this function's own loop, bounded by
//!    `-max_redirects`. A redirect that leaves the `http`/`https` family
//!    entirely — most importantly `file:` — is hard-routed through
//!    [`ProtocolRegistry::open_parsed`], the one function every top-level
//!    open in the whole project goes through, so it is refused by the exact
//!    mechanism that refuses it anywhere else.
//!
//! This is measured against the reference directly: redirecting a local test
//! server's response to `file:///etc/passwd` produces
//! `Protocol 'file' not on whitelist '...'` / `Invalid argument` from
//! `ffprobe` itself (see `docs/io/vaco-protocol-http.md`), confirming this is
//! observed behaviour, not a design choice this crate is alone in making.
//! `tests/redirect_whitelist.rs` reproduces the same refusal against this
//! crate, without a real network.

use vaco_io::{MediaSink, MediaSource, PeekSource};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    Access, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result,
    Url, split_url,
};

use crate::options::HttpOptions;
use crate::source::HttpSource;
use crate::{headers, transport, url as http_url};

/// Redirect status codes this crate follows. `304 Not Modified` and `305 Use
/// Proxy` are not in this set — neither is meaningful for a one-shot GET with
/// no prior cache state, and both would need bespoke handling this crate has
/// no use for.
fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// The reference's own default nested whitelist for `http`, per
/// `ffprobe -v debug`'s `Setting default whitelist '...'` line — reproduced
/// as data (D9: interface facts are free) even though this build does not yet
/// register every one of these schemes. An unregistered scheme in this list
/// is inert: `ProtocolRegistry::find` reports `Unknown` for it exactly as it
/// would for any other unimplemented scheme, so listing it here grants
/// nothing it does not already have to earn by being registered.
const DEFAULT_WHITELIST: &[&str] = &[
    "http",
    "https",
    "tls",
    "rtp",
    "tcp",
    "udp",
    "crypto",
    "httpproxy",
    "data",
];

/// The `http:`/`https:` protocol implementation.
///
/// One `struct`, two [`ProtocolDesc`] registrations ([`HTTP_PROTOCOL`],
/// [`HTTPS_PROTOCOL`]) — the scheme name a request opened under is read from
/// the `Url` `open` receives, not baked into the instance, so there is
/// nothing to keep in sync between the two registrations.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpProtocol;

impl HttpProtocol {
    fn options(opts: &Dict) -> Result<HttpOptions> {
        let mut parsed = HttpOptions::default();
        parsed
            .apply_dict(opts)
            .map_err(|_| ProtocolError::Malformed {
                scheme: "http",
                detail: "bad option value",
            })?;
        Ok(parsed)
    }
}

impl Protocol for HttpProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts_dict: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let opts = Self::options(opts_dict)?;
        let timeout = env.rw_timeout;

        let mut target = http_url::request_target(url).map_err(|_| ProtocolError::Malformed {
            scheme: "http",
            detail: "http:/https: cannot be the outer half of a +-joined nested scheme",
        })?;

        let mut redirects: u32 = 0;
        let max_redirects = u32::try_from(opts.max_redirects).unwrap_or(u32::MAX);

        loop {
            let (credentials, clean_target) = http_url::split_userinfo(&target);
            let start = u64::try_from(opts.offset).unwrap_or(0);
            let response = {
                let range = if matches!(opts.seekable(), crate::options::Seekable::Never) {
                    None
                } else {
                    let end_exclusive = (opts.end_offset > 0)
                        .then_some(u64::try_from(opts.end_offset).unwrap_or(0));
                    Some(headers::RequestRange {
                        start,
                        end_exclusive,
                    })
                };
                let creds = credentials.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
                let hdrs = headers::build(&opts, range, creds);
                transport::send("GET", &clean_target, &hdrs, timeout)?
            };

            let status = response.status().as_u16();
            if !is_redirect(status) {
                let source = HttpSource::from_first_response(
                    clean_target,
                    credentials,
                    opts,
                    timeout,
                    response,
                    start,
                )
                .map_err(ProtocolError::from)?;
                // `HttpSource` implements `RawSource`, one thin call per
                // request; `PeekSource` supplies the probe window every
                // `MediaSource` must offer, exactly as
                // `vaco_protocol_file::FileProtocol::open` wraps `FileSource`.
                // Started at what the source actually resolved to, which is
                // not necessarily `start` (a server may satisfy a `Range`
                // request from a different offset than requested).
                let actual_start = source.start_position();
                return Ok(Box::new(PeekSource::new(source).with_start(actual_start)));
            }

            redirects += 1;
            if redirects > max_redirects {
                return Err(ProtocolError::Malformed {
                    scheme: "http",
                    detail: "too many redirects",
                });
            }

            let Some(location) = response
                .headers()
                .get(ureq::http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                return Err(ProtocolError::Malformed {
                    scheme: "http",
                    detail: "redirect response carried no usable Location header",
                });
            };
            let next = http_url::resolve_location(&clean_target, location);
            let next_parsed = split_url(&next);
            let next_scheme = next_parsed.effective_scheme();

            // The security check: the redirect target must be permitted by
            // the *same* environment this open itself was granted, whatever
            // scheme it names.
            env.check_scheme(next_scheme)?;

            if next_scheme.eq_ignore_ascii_case("http") || next_scheme.eq_ignore_ascii_case("https")
            {
                // Same family: keep following it ourselves, bounded by
                // `redirects`/`max_redirects` above.
                target = next;
                continue;
            }

            // A different protocol family entirely. Hand off to the one
            // function every top-level open goes through, so `file:`,
            // `data:`, or anything else is subject to exactly the checks a
            // top-level open of that URL would be.
            return env
                .registry
                .open_parsed(&next_parsed, flags, opts_dict, env);
        }
    }

    fn check(&self, url: &Url, env: &ProtocolEnv<'_>) -> Result<Access> {
        // A `HEAD`-shaped probe: open for reading and immediately let it
        // drop. `http:` write support is not implemented (see the crate
        // docs), so `write` is always `false` here regardless of what the
        // server might actually accept.
        let opts = Dict::new();
        match self.open(url, IoFlags::READ, &opts, env) {
            Ok(_source) => Ok(Access {
                read: true,
                write: false,
            }),
            Err(_) => Ok(Access {
                read: false,
                write: false,
            }),
        }
    }

    fn create(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        Err(ProtocolError::Unsupported {
            scheme: "http",
            operation: "create (POST/PUT output is not implemented; see the crate docs)",
        })
    }
}

/// The registry entry for `http:`.
pub static HTTP_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "http",
    long_name: "HTTP",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
    },
    default_whitelist: DEFAULT_WHITELIST,
    options: Some(http_schema),
    proto: &HttpProtocol,
};

/// The registry entry for `https:`. Same implementation as `http:` — the
/// scheme actually opened is read from the `Url` `open` receives.
pub static HTTPS_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "https",
    long_name: "HTTPS",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
    },
    default_whitelist: DEFAULT_WHITELIST,
    options: Some(http_schema),
    proto: &HttpProtocol,
};

fn http_schema() -> &'static Schema {
    schema_of::<HttpOptions>()
}
