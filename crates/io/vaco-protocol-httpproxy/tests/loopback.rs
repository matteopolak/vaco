//! End-to-end against a real loopback `TcpListener` standing in for a proxy
//! — the "captured bytes" substitute this crate's docs describe: no live
//! proxy is reachable, but a byte-for-byte real socket round trip is.
//!
//! Kept as an integration test (`tests/`, not an inline `src/connect.rs`
//! `#[cfg(test)]` module) specifically so its `std::thread::spawn` calls —
//! needed for the accepting side of the loopback listener — are outside
//! `cargo xtask time-gate`'s scan, which covers every `src/` file but
//! deliberately not `tests/`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use vaco_io::CancelToken;
use vaco_protocol_core::ProtocolEnv;
use vaco_protocol_httpproxy::connect::{self, ProxyUrl};
use vaco_protocol_socket::url::HostPort;

#[test]
fn dial_completes_against_a_local_listener_that_answers_200() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = conn.read(&mut buf).unwrap();
        let req = String::from_utf8_lossy(buf.get(..n).unwrap());
        assert!(req.starts_with("CONNECT example.com:80 HTTP/1.1\r\n"));
        assert!(!req.contains("Proxy-Authorization"));
        conn.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
    });

    let url = ProxyUrl {
        proxy: HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        },
        auth: None,
        target: HostPort {
            host: "example.com".to_owned(),
            port: 80,
        },
    };
    let registry = vaco_protocol_core::ProtocolRegistry::new();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["httpproxy", "tcp"]);
    connect::dial(&url, Some(Duration::from_secs(2)), &env).unwrap();
    handle.join().unwrap();
}

#[test]
fn dial_retries_with_auth_after_a_407_basic_challenge() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        // First attempt: 407 with a Basic challenge.
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = conn.read(&mut buf).unwrap();
        assert!(!String::from_utf8_lossy(buf.get(..n).unwrap()).contains("Proxy-Authorization"));
        conn.write_all(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\
              Proxy-Authenticate: Basic realm=\"x\"\r\n\r\n",
        )
        .unwrap();
        drop(conn);

        // Second, fresh connection: must carry Proxy-Authorization.
        let (mut conn2, _) = listener.accept().unwrap();
        let n2 = conn2.read(&mut buf).unwrap();
        let req2 = String::from_utf8_lossy(buf.get(..n2).unwrap());
        assert!(req2.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"));
        conn2
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
    });

    let url = ProxyUrl {
        proxy: HostPort {
            host: "127.0.0.1".to_owned(),
            port: addr.port(),
        },
        auth: Some(("user".to_owned(), "pass".to_owned())),
        target: HostPort {
            host: "example.com".to_owned(),
            port: 80,
        },
    };
    let registry = vaco_protocol_core::ProtocolRegistry::new();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["httpproxy", "tcp"]);
    connect::dial(&url, Some(Duration::from_secs(2)), &env).unwrap();
    handle.join().unwrap();
}
