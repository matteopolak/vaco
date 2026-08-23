//! The socket-and-TLS layer. **Not portable** — this is the one module a
//! `fetch`-based sibling would replace entirely; see the crate's `lib.rs` docs
//! for what stays shared instead.
//!
//! Everything here is a thin, mechanical wrapping of `ureq`: build one
//! process-wide [`Agent`] with the crypto provider D14.2 requires, turn our
//! own header list into an `http::Request`, and turn `ureq::Error` into
//! [`vaco_protocol_core::ProtocolError`] preserving as much of the original
//! failure shape (in particular the `io::ErrorKind`) as `ureq`'s own error
//! type carries.

use std::io::ErrorKind;
use std::sync::OnceLock;
use std::time::Duration as StdDuration;

use ureq::config::Config;
use ureq::http::{Request, Response};
use ureq::tls::{TlsConfig, TlsProvider};
use ureq::{Agent, Body};
use vaco_protocol_core::{ProtocolError, Result};

/// The shared agent. Built once per process: the TLS configuration (crypto
/// provider, root store) is invariant across every open of this protocol, and
/// rebuilding it per request would repeat a non-trivial amount of setup for
/// no behavioural difference. Per-request behaviour that *does* vary
/// (timeouts) is layered on with [`Agent::configure_request`] rather than
/// baked into this agent.
fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        // The crypto provider is `vaco-protocol-tls`'s to build, not ours
        // (D11 — see that crate's docs, "Who owns rustls"): this is the same
        // `Arc<rustls::crypto::CryptoProvider>` its own `tls:` connections
        // use, not an independently constructed one.
        let crypto = vaco_protocol_tls::crypto::shared_provider();
        let tls_config = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .unversioned_rustls_crypto_provider(crypto)
            .build();
        let config = Config::builder()
            // We follow redirects ourselves, through the whitelist
            // (`crate::protocol`) — `ureq` must never do it silently.
            .max_redirects(0)
            // We inspect 3xx/4xx/5xx ourselves rather than getting them back
            // as `Err(Error::StatusCode(_))`, which would throw away the
            // response headers (`Location`, `Content-Range`) we need to read.
            .http_status_as_error(false)
            .tls_config(tls_config)
            .build();
        Agent::new_with_config(config)
    })
}

/// Send one request and return the raw response, without following redirects
/// or interpreting the status.
///
/// `timeout` is [`vaco_protocol_core::ProtocolEnv::rw_timeout`] when the
/// caller has one; applied as ureq's per-request global timeout (connect +
/// send + receive-headers, not the whole body read, which can legitimately
/// run far longer than one "operation").
///
/// # Errors
/// [`ProtocolError::Io`], wrapping a [`std::io::Error`] whose `kind()` is
/// preserved from `ureq` where `ureq` itself preserves it (a refused
/// connection stays [`ErrorKind::ConnectionRefused`], for instance).
pub fn send(
    method: &str,
    target: &str,
    headers: &[(String, String)],
    timeout: Option<StdDuration>,
) -> Result<Response<Body>> {
    let mut builder = Request::builder().method(method).uri(target);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let request = builder.body(()).map_err(|e| ProtocolError::Malformed {
        scheme: "http",
        detail: malformed_reason(&e),
    })?;

    let agent = agent();
    let configured = agent.configure_request(request);
    let configured = match timeout {
        Some(t) => configured.timeout_global(Some(t)),
        None => configured,
    };
    let request = configured.build();

    agent.run(request).map_err(map_ureq_error)
}

/// As [`send`], but with a request body streamed from `body`.
///
/// Built from [`ureq::SendBody::from_reader`] rather than a byte slice with a
/// known length: `ureq` reports no `Content-Length` it was not given one for,
/// so it sends `Transfer-Encoding: chunked` — which is the point of
/// `crate::source::HttpSink` buffering the whole body itself rather than
/// letting a caller hand it a pre-sized buffer (see that type's docs for why
/// this is "chunked on the wire" rather than "streamed while being written").
///
/// # Errors
/// As [`send`].
pub fn send_body(
    method: &str,
    target: &str,
    headers: &[(String, String)],
    timeout: Option<StdDuration>,
    body: &mut dyn std::io::Read,
) -> Result<Response<Body>> {
    let mut builder = Request::builder().method(method).uri(target);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let request = builder
        .body(ureq::SendBody::from_reader(body))
        .map_err(|e| ProtocolError::Malformed {
            scheme: "http",
            detail: malformed_reason(&e),
        })?;

    let agent = agent();
    let configured = agent.configure_request(request);
    let configured = match timeout {
        Some(t) => configured.timeout_global(Some(t)),
        None => configured,
    };
    let request = configured.build();

    agent.run(request).map_err(map_ureq_error)
}

/// `http::Error` carries no `'static` classification we can match on, so a
/// build failure is reported generically rather than with the underlying
/// text — which may itself embed attacker-controlled header content we do
/// not want to echo uninspected.
const fn malformed_reason(_e: &ureq::http::Error) -> &'static str {
    "invalid method or header for an HTTP request"
}

fn map_ureq_error(e: ureq::Error) -> ProtocolError {
    use ureq::Error as E;
    let io = match e {
        E::Io(io) => io,
        E::Timeout(_) => std::io::Error::new(ErrorKind::TimedOut, "http request timed out"),
        E::HostNotFound => std::io::Error::new(ErrorKind::NotFound, "host not found"),
        E::ConnectionFailed => std::io::Error::other("connection failed"),
        other => std::io::Error::other(other),
    };
    ProtocolError::from(io)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    /// No network access: refusing a connection to a closed local port is a
    /// purely local kernel round trip, never touches the internet, and is
    /// the one connection outcome guaranteed available in CI (plan 19 §4 /
    /// this brief's "never make a network request in a unit test" — binding
    /// to `port 0` and never accepting is the same idea applied to the
    /// *failure* path instead of the success path).
    #[test]
    fn connection_refused_keeps_its_error_kind() {
        // Bind, then drop immediately: the port is very likely still refusing
        // by the time we connect, and if the OS recycles it fast enough to
        // race this, the failure mode is a flaky `Ok`, never a wrong panic.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let target = format!("http://{addr}/");
        let err = send("GET", &target, &[], Some(StdDuration::from_millis(500))).unwrap_err();
        match err {
            ProtocolError::Io(vaco_core::Error::Io(io)) => {
                assert_eq!(io.kind(), ErrorKind::ConnectionRefused);
            }
            other => panic!("expected a connection-refused io error, got {other:?}"),
        }
    }
}
