//! URL parsing, the `CONNECT` request/response, and the auth-retry dance.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use vaco_protocol_core::{ProtocolEnv, ProtocolError, Result};
use vaco_protocol_socket::url::HostPort;

/// `[user:pass@]proxy-host:proxy-port` and the tunnel target, split from a
/// `httpproxy:` URL's `rest`.
///
/// `crypto:file:x`-style nesting does not apply here: `rest` is
/// `proxy-authority/target`, a single `/`-separated pair, not a further
/// scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyUrl {
    pub proxy: HostPort,
    pub auth: Option<(String, String)>,
    pub target: HostPort,
}

/// Split `rest` (everything after `httpproxy:`) into the proxy address,
/// optional basic-auth credentials, and the tunnel target.
///
/// # Errors
/// [`ProtocolError::Malformed`] if either half is not a parseable
/// `host:port`.
pub fn parse(rest: &str) -> Result<ProxyUrl> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (authority, target) = rest.split_once('/').ok_or(ProtocolError::Malformed {
        scheme: "httpproxy",
        detail: "expected proxy-host:port/target-host:port",
    })?;

    let (auth, host_port) = match authority.split_once('@') {
        Some((userinfo, hp)) => {
            let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            (Some((user.to_owned(), pass.to_owned())), hp)
        }
        None => (None, authority),
    };

    let proxy = parse_host_port(host_port, "expected proxy-host:port")?;
    let target = parse_host_port(target, "expected target-host:port")?;
    Ok(ProxyUrl {
        proxy,
        auth,
        target,
    })
}

fn parse_host_port(s: &str, detail: &'static str) -> Result<HostPort> {
    let (host, port_str) = s.rsplit_once(':').ok_or(ProtocolError::Malformed {
        scheme: "httpproxy",
        detail,
    })?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| ProtocolError::Malformed {
            scheme: "httpproxy",
            detail,
        })?;
    Ok(HostPort {
        host: host.to_owned(),
        port,
    })
}

/// RFC 4648 §4 standard base64 with padding. Small enough, and specific
/// enough to `Proxy-Authorization`'s exact alphabet, that this is a local
/// copy rather than a new dependency — `vaco-protocol-http`'s `headers.rs`
/// has the same one, independently, for the identical `Authorization: Basic`
/// case.
fn base64_standard(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = *chunk.first().unwrap_or(&0);
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let idx = |shift: u32| {
            let i = usize::try_from((n >> shift) & 0x3f).unwrap_or(0);
            char::from(*ALPHABET.get(i).unwrap_or(&b'A'))
        };
        out.push(idx(18));
        out.push(idx(12));
        out.push(if chunk.len() > 1 { idx(6) } else { '=' });
        out.push(if chunk.len() > 2 { idx(0) } else { '=' });
    }
    out
}

fn request_line(target: &HostPort, proxy: &HostPort, auth: Option<&(String, String)>) -> String {
    use std::fmt::Write as _;

    let mut req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n",
        target.host, target.port, proxy.host, proxy.port
    );
    if let Some((user, pass)) = auth {
        let creds = base64_standard(format!("{user}:{pass}").as_bytes());
        let _ = write!(req, "Proxy-Authorization: Basic {creds}\r\n");
    }
    req.push_str("\r\n");
    req
}

/// The parsed status line and whether a `Proxy-Authenticate: Basic` header
/// was present (only consulted on a `407`).
#[derive(Debug)]
pub struct ProxyResponse {
    pub status: u16,
    pub basic_challenge: bool,
}

/// Parse an already-read header block (status line plus headers, `\r\n`
/// between each, ending in the blank-line terminator already stripped or
/// not — either is fine, since nothing after the last real header is
/// inspected). Pure and I/O-free so it can be fuzzed directly; the fuzz
/// target for this crate hands it arbitrary bytes.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a status line that is not `HTTP/x.y
/// NNN ...`.
pub fn parse_response(block: &str) -> Result<ProxyResponse> {
    let mut lines = block.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(ProtocolError::Malformed {
            scheme: "httpproxy",
            detail: "proxy sent a malformed HTTP status line",
        })?;

    let basic_challenge = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("proxy-authenticate")
                && value.to_ascii_lowercase().contains("basic")
        })
    });
    Ok(ProxyResponse {
        status,
        basic_challenge,
    })
}

/// Bytes a response header block may reasonably use — a fixed bound rather
/// than growing without limit, since the source is a network peer.
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;

/// Read exactly the response headers, one byte at a time, stopping the
/// instant the blank line is seen.
///
/// **Deliberately not `BufReader`**: `stream` is the same connection this
/// function's caller hands back as the tunnel on success, and a `BufReader`
/// that read ahead past the header block would silently swallow the tunnel's
/// first bytes into a buffer nothing downstream ever drains — a real
/// correctness gap for a fast peer that pipelines tunnel data right behind
/// `200 ...\r\n\r\n` in the same segment, not a hypothetical one. Reading
/// one byte at a time is slower per call, but a response header block is at
/// most a few hundred bytes, so that cost is not worth trading correctness
/// for.
fn read_header_block(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if buf.len() >= MAX_RESPONSE_HEADER_BYTES {
            return Err(ProtocolError::Malformed {
                scheme: "httpproxy",
                detail: "proxy response headers exceeded the size limit",
            });
        }
        if stream.read(&mut byte)? == 0 {
            return Err(ProtocolError::Malformed {
                scheme: "httpproxy",
                detail: "proxy closed the connection before completing its response",
            });
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse the proxy's response to one `CONNECT` attempt.
///
/// # Errors
/// [`ProtocolError::Malformed`] for a status line that is not `HTTP/x.y
/// NNN ...`; propagates I/O failure otherwise.
fn read_response(stream: &mut TcpStream) -> Result<ProxyResponse> {
    let block = read_header_block(stream)?;
    parse_response(&block)
}

/// Connect to `proxy`, applying the whitelist check by hand (see the crate
/// docs for why this cannot go through the registry), and complete the
/// `CONNECT` handshake for `target`. Retries once, on a fresh connection,
/// with `Proxy-Authorization: Basic` when the first attempt is refused with
/// `407` and a `Basic` challenge and `auth` credentials are available —
/// exactly the sequence measured against a local capture (see the crate
/// docs).
///
/// # Errors
/// [`ProtocolError::Denied`] if `"tcp"` is not permitted by `env`;
/// [`ProtocolError::Malformed`] if the proxy's response cannot be parsed as
/// HTTP or is a non-2xx status this function cannot recover from;
/// propagates I/O failure otherwise.
pub fn dial(url: &ProxyUrl, timeout: Option<Duration>, env: &ProtocolEnv<'_>) -> Result<TcpStream> {
    env.check_scheme("tcp")?;

    let mut stream = vaco_protocol_socket::addr::connect(&url.proxy, timeout)?;
    let first = request_line(&url.target, &url.proxy, None);
    stream.write_all(first.as_bytes())?;
    let resp = read_response(&mut stream)?;

    if (200..300).contains(&resp.status) {
        return Ok(stream);
    }

    if resp.status == 407 && resp.basic_challenge && url.auth.is_some() {
        // Measured: the retry is a brand-new TCP connection, not a second
        // request on the same one.
        let mut retry = vaco_protocol_socket::addr::connect(&url.proxy, timeout)?;
        let second = request_line(&url.target, &url.proxy, url.auth.as_ref());
        retry.write_all(second.as_bytes())?;
        let resp2 = read_response(&mut retry)?;
        if (200..300).contains(&resp2.status) {
            return Ok(retry);
        }
        return Err(ProtocolError::Malformed {
            scheme: "httpproxy",
            detail: "proxy refused the CONNECT request even after authenticating",
        });
    }

    Err(ProtocolError::Malformed {
        scheme: "httpproxy",
        detail: "proxy refused the CONNECT request",
    })
}

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
    fn parses_proxy_and_target_without_auth() {
        let u = parse("127.0.0.1:8080/example.com:80").unwrap();
        assert_eq!(u.proxy.host, "127.0.0.1");
        assert_eq!(u.proxy.port, 8080);
        assert_eq!(u.target.host, "example.com");
        assert_eq!(u.target.port, 80);
        assert!(u.auth.is_none());
    }

    #[test]
    fn parses_userinfo() {
        let u = parse("bob:secret@proxy.example:3128/example.com:443").unwrap();
        assert_eq!(u.auth, Some(("bob".to_owned(), "secret".to_owned())));
        assert_eq!(u.proxy.host, "proxy.example");
        assert_eq!(u.proxy.port, 3128);
    }

    #[test]
    fn rejects_a_target_missing_a_port() {
        assert!(parse("proxy:8080/example.com").is_err());
    }

    #[test]
    fn base64_matches_the_measured_worked_example() {
        // Measured: `user:pass` -> `dXNlcjpwYXNz`.
        assert_eq!(base64_standard(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn request_line_names_the_proxy_in_host_not_the_target() {
        let target = HostPort {
            host: "example.com".to_owned(),
            port: 80,
        };
        let proxy = HostPort {
            host: "127.0.0.1".to_owned(),
            port: 18080,
        };
        let req = request_line(&target, &proxy, None);
        // Exact transcript captured against a local loopback listener.
        assert_eq!(
            req,
            "CONNECT example.com:80 HTTP/1.1\r\nHost: 127.0.0.1:18080\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn request_line_with_auth_matches_the_measured_retry() {
        let target = HostPort {
            host: "example.com".to_owned(),
            port: 80,
        };
        let proxy = HostPort {
            host: "127.0.0.1".to_owned(),
            port: 18084,
        };
        let auth = ("user".to_owned(), "pass".to_owned());
        let req = request_line(&target, &proxy, Some(&auth));
        assert_eq!(
            req,
            "CONNECT example.com:80 HTTP/1.1\r\nHost: 127.0.0.1:18084\r\nConnection: close\r\n\
             Proxy-Authorization: Basic dXNlcjpwYXNz\r\n\r\n"
        );
    }

    // The two loopback-listener end-to-end tests that were here
    // (`dial_completes_against_a_local_listener_that_answers_200`,
    // `dial_retries_with_auth_after_a_407_basic_challenge`) moved to
    // `tests/loopback.rs`: `cargo xtask time-gate` scans every `src/` file
    // (including inline `#[cfg(test)]` modules) for `std::thread::spawn`,
    // which those tests need for the accepting side of the listener, but
    // deliberately does not scan `tests/` — a real integration-test
    // directory is exactly where a thread genuinely driven by the test
    // harness, not by shipped code, belongs.

    #[test]
    fn dial_is_denied_without_tcp_on_the_whitelist() {
        let url = ProxyUrl {
            proxy: HostPort {
                host: "127.0.0.1".to_owned(),
                port: 1,
            },
            auth: None,
            target: HostPort {
                host: "example.com".to_owned(),
                port: 80,
            },
        };
        let registry = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["httpproxy"]);
        let err = dial(&url, None, &env).unwrap_err();
        assert!(matches!(err, ProtocolError::Denied { .. }));
    }
}
