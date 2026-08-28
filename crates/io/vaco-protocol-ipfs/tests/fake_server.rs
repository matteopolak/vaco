//! End to end against an in-process fake HTTP server, driving the real
//! `Protocol::open` path through the real `vaco-protocol-http` (not a
//! substitute), the same as the reference's own internal routing.
//!
//! Deliberately uses only the `-gateway` **option**, never
//! `$IPFS_GATEWAY`/`$IPFS_PATH`/`$HOME`: `std::env::set_var` is `unsafe` on
//! this edition, and this crate (like the rest of the workspace) forbids
//! `unsafe_code` even in its test targets. The env-var- and
//! gateway-file-based precedence is instead covered purely, with no real
//! environment mutation, by `gateway::tests::*` in `src/gateway.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use vaco_io::{CancelToken, MediaSource};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_ipfs::IPFS_PROTOCOL);
    r.register(&vaco_protocol_ipfs::IPNS_PROTOCOL);
    r.register(&vaco_protocol_http::protocol::HTTP_PROTOCOL);
    r
}

fn env<'a>(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> ProtocolEnv<'a> {
    ProtocolEnv::new(registry, cancel).with_whitelist(&["ipfs", "ipns", "http", "tcp"])
}

/// Answer any request with a fixed 200 response, capturing the request line
/// via `report`.
fn serve_once(listener: TcpListener, body: &'static [u8], report: std::sync::mpsc::Sender<String>) {
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            conn.read_exact(&mut byte).unwrap();
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).into_owned();
        let request_line = request.lines().next().unwrap_or_default().to_owned();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        conn.write_all(response.as_bytes()).unwrap();
        conn.write_all(body).unwrap();
        let _ = report.send(request_line);
    });
}

fn read_all(source: &mut Box<dyn MediaSource>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16];
    loop {
        let n = source.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

#[test]
fn fetches_through_the_gateway_option_with_the_measured_ipfs_path() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    serve_once(listener, b"hello ipfs content", tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let mut opts = Dict::new();
    opts.set("gateway", &format!("http://127.0.0.1:{port}"));

    let mut source = registry
        .open("ipfs://QmTestCid/video.mp4", IoFlags::READ, &opts, &e)
        .unwrap();
    let got = read_all(&mut source);
    assert_eq!(got, b"hello ipfs content");

    let request_line = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(request_line, "GET /ipfs/QmTestCid/video.mp4 HTTP/1.1");
}

#[test]
fn fetches_through_the_gateway_option_with_the_measured_ipns_path() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    serve_once(listener, b"hello ipns content", tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let mut opts = Dict::new();
    opts.set("gateway", &format!("http://127.0.0.1:{port}"));

    let mut source = registry
        .open("ipns://example.com/video.mp4", IoFlags::READ, &opts, &e)
        .unwrap();
    let got = read_all(&mut source);
    assert_eq!(got, b"hello ipns content");

    let request_line = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(request_line, "GET /ipns/example.com/video.mp4 HTTP/1.1");
}

#[test]
fn a_gateway_with_a_trailing_slash_does_not_produce_a_double_slash() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    serve_once(listener, b"x", tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let mut opts = Dict::new();
    opts.set("gateway", &format!("http://127.0.0.1:{port}/"));

    let mut source = registry
        .open("ipfs://QmCid/f", IoFlags::READ, &opts, &e)
        .unwrap();
    let _ = read_all(&mut source);

    let request_line = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(request_line, "GET /ipfs/QmCid/f HTTP/1.1");
}

#[test]
fn no_gateway_configured_is_refused() {
    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let err = registry
        .open("ipfs://QmCid/f", IoFlags::READ, &Dict::new(), &e)
        .err()
        .unwrap();
    assert!(matches!(
        err,
        vaco_protocol_core::ProtocolError::Malformed { .. }
    ));
}

#[test]
fn a_whitelist_naming_only_ipfs_refuses_the_nested_http_open() {
    let registry = registry();
    let cancel = CancelToken::new();
    let e = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["ipfs"]);
    let mut opts = Dict::new();
    opts.set("gateway", "http://127.0.0.1:1");
    let err = registry
        .open("ipfs://QmCid/f", IoFlags::READ, &opts, &e)
        .err()
        .unwrap();
    assert!(matches!(
        err,
        vaco_protocol_core::ProtocolError::Denied { .. }
    ));
}
