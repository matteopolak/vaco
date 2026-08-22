//! The security property this crate exists to hold: a redirect is a URL
//! chosen by the server, and it goes through the *same* whitelist gate every
//! other nested open does.
//!
//! Measured against the reference directly (`docs/io/vaco-protocol-http.md`):
//! redirecting `ffprobe` to `file:///etc/passwd` produces
//! `Protocol 'file' not on whitelist '...'` / `Invalid argument`, not a
//! silent local-file read. These tests reproduce the same refusal against
//! this crate, entirely over loopback.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

mod support;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{DenyReason, IoFlags, ProtocolEnv, ProtocolError, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_http::register(&mut r);
    r
}

/// `Box<dyn MediaSource>` is not `Debug`, so `Result::expect_err` is
/// unavailable — matches the pattern `vaco-protocol-core`'s own
/// `tests/whitelist.rs` uses for the same reason.
fn err_of(r: vaco_protocol_core::Result<Box<dyn vaco_io::MediaSource>>) -> ProtocolError {
    match r {
        Ok(_) => panic!("expected an error, got a source"),
        Err(e) => e,
    }
}

fn denied(err: ProtocolError) -> DenyReason {
    match err {
        ProtocolError::Denied { reason, .. } => reason,
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn a_redirect_to_file_is_refused_by_the_whitelist() {
    let server = support::spawn_redirect(302, "file:///etc/passwd");
    let r = registry();
    let cancel = CancelToken::new();
    // The environment a real CLI would build for a URL it did not itself
    // choose to treat as fully trusted (mirrors `-protocol_whitelist
    // http,https`) — `file` is not named, and `http`'s own default grant
    // (`DEFAULT_WHITELIST` in `crate::protocol`) does not include it either.
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http", "https"]);
    let url = format!("http://{}/start", server.addr);

    let err = err_of(r.open(&url, IoFlags::READ, &Dict::new(), &env));
    assert_eq!(denied(err), DenyReason::NotWhitelisted);
}

#[test]
fn a_redirect_to_an_unregistered_scheme_is_still_refused_not_silently_ignored() {
    // Even a scheme this build has never heard of must be refused by the
    // gate rather than reported as merely "unknown" — W1/W2 are checked
    // before the registry lookup (see `vaco-protocol-core`'s `resolve`), so
    // the whitelist's absence is what a caller sees, not a registry probe.
    let server = support::spawn_redirect(302, "gopher://example.invalid/x");
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http", "https"]);
    let url = format!("http://{}/start", server.addr);

    let err = err_of(r.open(&url, IoFlags::READ, &Dict::new(), &env));
    assert_eq!(denied(err), DenyReason::NotWhitelisted);
}

#[test]
fn a_same_family_redirect_is_followed_when_permitted() {
    // http -> https is exactly the case the reference's own default whitelist
    // exists to allow: both are in `crate::protocol::DEFAULT_WHITELIST`.
    let https_target = support::spawn(b"payload".to_vec(), support::Behavior::HonorRange);
    // We cannot actually terminate TLS in this test without a certificate, so
    // this test only proves the *scheme check* passes and the crate attempts
    // the follow-on connection (which then fails at the TLS layer against a
    // plain-HTTP loopback server) — a `Denied` here would be the bug this
    // test exists to catch; a transport-level failure reaching the TLS
    // handshake is expected and fine.
    let redirect = support::spawn_redirect(302, &format!("https://{}/x", https_target.addr));
    let r = registry();
    let cancel = CancelToken::new();
    // Bounded: a plain-HTTP server answering what it thinks is a TLS
    // handshake can otherwise leave the client waiting far longer than an
    // OS-level TCP timeout — this test only needs to observe that we *tried*
    // to reach the whitelisted https target, not that the attempt succeeds.
    let env = ProtocolEnv::new(&r, &cancel)
        .with_whitelist(&["http", "https"])
        .with_rw_timeout(std::time::Duration::from_secs(2));
    let url = format!("http://{}/start", redirect.addr);

    match r.open(&url, IoFlags::READ, &Dict::new(), &env) {
        Ok(_) => panic!("expected the TLS handshake against a plain-HTTP server to fail"),
        Err(ProtocolError::Denied { .. }) => {
            panic!("the whitelist must not refuse an https redirect that is explicitly granted")
        }
        Err(_) => {} // any transport-level error is the expected outcome here
    }
}

#[test]
fn max_redirects_zero_refuses_the_first_redirect() {
    let target = support::spawn(b"unused".to_vec(), support::Behavior::HonorRange);
    let server = support::spawn_redirect(302, &format!("http://{}/x", target.addr));
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let mut opts = Dict::new();
    opts.set("max_redirects", "0");
    let url = format!("http://{}/start", server.addr);

    let err = err_of(r.open(&url, IoFlags::READ, &opts, &env));
    // Not a whitelist denial — a redirect-budget error, distinct from W1-W4.
    assert!(!matches!(err, ProtocolError::Denied { .. }), "{err:?}");
}
