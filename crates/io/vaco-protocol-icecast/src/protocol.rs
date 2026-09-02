//! [`IcecastProtocol`] — the `icecast:` scheme, URL parsing, and the
//! registry entry.

use std::io::{Read, Write};

use vaco_io::{MediaSink, MediaSource, WriterSink};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};
use vaco_protocol_socket::url::HostPort;
use vaco_protocol_tls::TlsOptions;

use crate::options::IcecastOptions;
use crate::request::{self, Target};

/// Measured: an `icecast:` URL with no explicit port connects to port `80`
/// (`443` under `-tls 1`) — the reference routes the actual request through
/// its `http:`/`https:` protocol internally, and inherits *that* protocol's
/// default port, not the conventional Icecast server port `8000`. Confirmed
/// with `ffmpeg -v debug`: `[tcp @ ...] Address 127.0.0.1 port 80` for a
/// bare host, and `port 443` once `-tls 1` is added.
const DEFAULT_PORT: u16 = 80;
const DEFAULT_TLS_PORT: u16 = 443;

/// `[user[:pass]@]host[:port]/path`, split from `url.rest`. `pub` (and
/// `parse_url` with it) so the fuzz target for this crate can drive the URL
/// parser directly with arbitrary bytes.
#[derive(Debug)]
pub struct IcecastUrl {
    pub host: HostPort,
    userinfo: Option<(String, Option<String>)>,
    pub path: String,
}

/// # Errors
/// [`ProtocolError::Malformed`] if `rest` names no parseable host.
pub fn parse_url(rest: &str, tls: bool) -> Result<IcecastUrl> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hostport) = match authority.split_once('@') {
        Some((info, hp)) => {
            let (user, pass) = match info.split_once(':') {
                Some((u, p)) => (u.to_owned(), Some(p.to_owned())),
                None => (info.to_owned(), None),
            };
            (Some((user, pass)), hp)
        }
        None => (None, authority),
    };
    let default_port = if tls { DEFAULT_TLS_PORT } else { DEFAULT_PORT };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(default_port)),
        None => (hostport, default_port),
    };
    if host.is_empty() {
        return Err(ProtocolError::Malformed {
            scheme: "icecast",
            detail: "expected icecast://host[:port]/path",
        });
    }
    Ok(IcecastUrl {
        host: HostPort {
            host: host.to_owned(),
            port,
        },
        userinfo,
        path: format!("/{path}"),
    })
}

/// Resolve source-client credentials per the measured precedence: URL
/// userinfo wins outright (measured via the reference's own debug line,
/// `Overwriting -password <pass> with URI password!`); otherwise the
/// username defaults to the literal `source` (measured by base64-decoding
/// the `Authorization` header sent with no userinfo in the URL) and the
/// password comes from `-password`, or empty if that too is unset.
fn credentials(url: &IcecastUrl, opts: &IcecastOptions) -> (String, String) {
    let user = url
        .userinfo
        .as_ref()
        .map(|(u, _)| u.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "source".to_owned());
    let password = url
        .userinfo
        .as_ref()
        .and_then(|(_, p)| p.clone())
        .unwrap_or_else(|| opts.password.clone());
    (user, password)
}

fn options(opts: &Dict) -> Result<IcecastOptions> {
    let mut parsed = IcecastOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "icecast",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

/// Send the request headers and, for modern (non-legacy) mode, block for the
/// server's `100 Continue` before returning — measured: a fake server that
/// accepts the connection, reads the headers, and answers nothing never
/// receives a body from the reference. Legacy mode sends the body
/// immediately with no such wait.
///
/// # Errors
/// Propagates I/O failure; [`ProtocolError::Malformed`] if modern mode's
/// wait sees anything other than a `100` status (the untested case: what the
/// reference does on e.g. an immediate `401` here has not been captured
/// against a real Icecast server — see the crate docs).
fn handshake<S: Read + Write>(
    stream: &mut S,
    opts: &IcecastOptions,
    target: &Target<'_>,
) -> Result<()> {
    let headers = request::build_headers(opts, target);
    stream.write_all(headers.as_bytes())?;
    let (_, expect_continue) = request::method(opts);
    if expect_continue {
        let block = vaco_protocol_dial::read_header_block(
            stream,
            "icecast",
            "server closed the connection before answering 100-continue",
        )?;
        let text = String::from_utf8_lossy(&block);
        let status =
            request::parse_status_line(text.as_bytes()).ok_or(ProtocolError::Malformed {
                scheme: "icecast",
                detail: "server sent a malformed HTTP status line",
            })?;
        if status != 100 {
            return Err(ProtocolError::Malformed {
                scheme: "icecast",
                detail: "server did not answer 100-continue",
            });
        }
    }
    Ok(())
}

/// The `icecast:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct IcecastProtocol;

impl Protocol for IcecastProtocol {
    fn open(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        // Output-only: `-h protocol=icecast` lists every option `E`
        // (encoding) only, and `-protocols` lists `icecast` under `Output:`
        // and not `Input:`. Same stub shape as `vaco-protocol-local`'s
        // `md5:`.
        Err(ProtocolError::Unsupported {
            scheme: "icecast",
            operation: "reading (icecast: is an output-only protocol)",
        })
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let parsed = options(opts)?;
        let ice_url = parse_url(&url.rest, parsed.tls)?;
        let (user, password) = credentials(&ice_url, &parsed);
        let target = Target {
            path: &ice_url.path,
            host: &format!("{}:{}", ice_url.host.host, ice_url.host.port),
            user: &user,
            password: &password,
        };

        // The SOURCE/PUT handshake is inherently duplex (write headers,
        // then — for modern mode — read a 100-continue before the body), so
        // this dials directly rather than going through the registry.
        let sink: Box<dyn MediaSink> = if parsed.tls {
            let mut stream =
                vaco_protocol_dial::dial_tls(&ice_url.host, None, env, &TlsOptions::default())?;
            handshake(&mut stream, &parsed, &target)?;
            Box::new(WriterSink::new(stream))
        } else {
            let mut stream = vaco_protocol_dial::dial_tcp(&ice_url.host, None, env)?;
            handshake(&mut stream, &parsed, &target)?;
            Box::new(WriterSink::new(stream))
        };
        Ok(sink)
    }
}

fn icecast_schema() -> &'static Schema {
    schema_of::<IcecastOptions>()
}

/// The registry entry for `icecast:`.
pub static ICECAST_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "icecast",
    long_name: "Icecast protocol",
    // `-protocols` lists `icecast` only under `Output:`.
    flags: ProtocolFlags {
        network: true,
        // It never opens a further URL of its own (unlike `md5:`) — the
        // dial target comes entirely from this URL and these options.
        nested_scheme: false,
        server_capable: false,
        readable: false,
        writable: true,
    },
    // Measured: `[icecast @ ...] No default whitelist set` — empty, like
    // `crypto`/`tls`/`httpproxy`/`ftp`. The reference's *internal* routing
    // through its own `http`/`https` protocol for the actual request (seen
    // in its debug log as a nested `Setting default whitelist
    // 'http,https,tls,rtp,tcp,udp,crypto,httpproxy...'`) is a C
    // implementation detail of how it issues the HTTP request, not a
    // documented `icecast:` grant — a caller still needs `tcp`/`tls` on an
    // explicit whitelist, matching every other directly-dialling protocol in
    // this workspace.
    default_whitelist: &[],
    options: Some(icecast_schema),
    proto: &IcecastProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_host_and_defaults_port_80() {
        let u = parse_url("//127.0.0.1/mystream.mp3", false).unwrap();
        assert_eq!(u.host.host, "127.0.0.1");
        assert_eq!(u.host.port, 80);
        assert_eq!(u.path, "/mystream.mp3");
    }

    #[test]
    fn tls_defaults_port_443() {
        let u = parse_url("//127.0.0.1/mystream.mp3", true).unwrap();
        assert_eq!(u.host.port, 443);
    }

    #[test]
    fn explicit_port_overrides_the_tls_default() {
        let u = parse_url("//127.0.0.1:8000/mystream.mp3", true).unwrap();
        assert_eq!(u.host.port, 8000);
    }

    #[test]
    fn parses_userinfo() {
        let u = parse_url("//bob:secret@host/f", false).unwrap();
        assert_eq!(
            u.userinfo,
            Some(("bob".to_owned(), Some("secret".to_owned())))
        );
    }

    #[test]
    fn credentials_default_username_is_source() {
        let url = parse_url("//host/f", false).unwrap();
        let opts = IcecastOptions::default();
        assert_eq!(
            credentials(&url, &opts),
            ("source".to_owned(), String::new())
        );
    }

    #[test]
    fn url_userinfo_overrides_password_option() {
        let url = parse_url("//bob:secret@host/f", false).unwrap();
        let opts = IcecastOptions {
            password: "otherpass".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            credentials(&url, &opts),
            ("bob".to_owned(), "secret".to_owned())
        );
    }

    #[test]
    fn password_option_is_used_when_url_has_no_userinfo() {
        let url = parse_url("//host/f", false).unwrap();
        let opts = IcecastOptions {
            password: "opt-pass".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            credentials(&url, &opts),
            ("source".to_owned(), "opt-pass".to_owned())
        );
    }

    #[test]
    fn default_whitelist_is_empty() {
        assert!(ICECAST_PROTOCOL.default_whitelist.is_empty());
    }

    #[test]
    fn open_is_unsupported() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel);
        let url = vaco_protocol_core::split_url("icecast://h/f");
        let err = IcecastProtocol
            .open(&url, IoFlags::READ, &Dict::new(), &env)
            .err()
            .unwrap();
        assert!(matches!(err, ProtocolError::Unsupported { .. }));
    }

    #[test]
    fn create_is_denied_without_tcp_on_the_whitelist() {
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["icecast"]);
        let url = vaco_protocol_core::split_url("icecast://127.0.0.1:1/f");
        let err = IcecastProtocol
            .create(&url, IoFlags::WRITE, &Dict::new(), &env)
            .err()
            .unwrap();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }
}
