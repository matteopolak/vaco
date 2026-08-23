//! The `unix:` fallback that registers on a target with no `AF_UNIX`.
//!
//! `#[cfg(not(unix))]`-gated, so this file compiles to zero tests on the
//! `unix`-family machines this crate is normally developed and gated on
//! (`tests/unix_loopback.rs` covers the real behaviour there) — it exists so
//! the fallback path in `src/unix.rs` has a check at all, rather than being
//! exercised only by inspection.

#![cfg(not(unix))]
#![allow(clippy::unwrap_used, reason = "tests")]

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolError, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_socket::register(&mut r);
    r
}

#[test]
fn unix_open_fails_with_unsupported_not_a_panic() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["unix"]);
    let err = r
        .open("unix:/tmp/does-not-matter.sock", IoFlags::READ, &Dict::new(), &env)
        .err();
    assert!(matches!(err, Some(ProtocolError::Unsupported { .. })));
}
