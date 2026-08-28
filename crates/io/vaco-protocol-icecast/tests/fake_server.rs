//! End to end against an in-process fake HTTP-ish server, driving the real
//! `Protocol::create` path rather than `request::build_headers` in
//! isolation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_icecast::ICECAST_PROTOCOL);
    r
}

fn env<'a>(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> ProtocolEnv<'a> {
    ProtocolEnv::new(registry, cancel).with_whitelist(&["icecast", "tcp"])
}

/// Reads exactly the header block (through the trailing blank line), then
/// hands it and the rest of what it read within the timeout to `report`.
fn capture(listener: TcpListener, answer_100: bool, report: mpsc::Sender<(String, Vec<u8>)>) {
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        conn.set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .unwrap();
        let mut header_buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match conn.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    header_buf.push(byte[0]);
                    if header_buf.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let headers = String::from_utf8_lossy(&header_buf).into_owned();

        if answer_100 {
            conn.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
        }

        let mut body = Vec::new();
        let _ = conn.read_to_end(&mut body);
        let _ = report.send((headers, body));
    });
}

#[test]
fn modern_mode_waits_for_100_continue_before_sending_the_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    capture(listener, true, tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("icecast://source:hackme@127.0.0.1:{port}/mount.mp3");

    let mut sink = registry.create(&url, IoFlags::WRITE, &Dict::new(), &e).unwrap();
    sink.write(b"stream-bytes").unwrap();
    sink.flush().unwrap();
    drop(sink);

    let (headers, body) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert!(headers.starts_with("PUT /mount.mp3 HTTP/1.1\r\n"));
    assert!(headers.contains("Expect: 100-continue\r\n"));
    assert_eq!(body, b"stream-bytes");
}

#[test]
fn modern_mode_never_sends_the_body_if_100_continue_never_comes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    capture(listener, false, tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("icecast://source:hackme@127.0.0.1:{port}/mount.mp3");

    // `create` itself is expected to fail: this crate's `handshake` treats
    // "the wait timed out / the peer closed without ever sending 100" as an
    // error rather than proceeding to send the body anyway.
    let result = registry.create(&url, IoFlags::WRITE, &Dict::new(), &e);
    assert!(result.is_err());

    let (headers, body) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert!(headers.starts_with("PUT "));
    assert!(body.is_empty(), "no body should ever have been sent");
}

#[test]
fn legacy_mode_sends_source_and_the_body_immediately_with_no_wait() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    // Legacy mode never waits for a 100, so whether the fake server answers
    // one is irrelevant; answer none to prove that.
    capture(listener, false, tx);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("icecast://source:hackme@127.0.0.1:{port}/mount.mp3");
    let mut opts = Dict::new();
    opts.set("legacy_icecast", "1");

    let mut sink = registry.create(&url, IoFlags::WRITE, &opts, &e).unwrap();
    sink.write(b"legacy-bytes").unwrap();
    sink.flush().unwrap();
    drop(sink);

    let (headers, body) = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert!(headers.starts_with("SOURCE /mount.mp3 HTTP/1.1\r\n"));
    assert!(!headers.contains("Expect"));
    assert_eq!(body, b"legacy-bytes");
}

#[test]
fn a_whitelist_naming_only_icecast_refuses_the_nested_tcp_open() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let registry = registry();
    let cancel = CancelToken::new();
    let e = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["icecast"]);
    let url = format!("icecast://127.0.0.1:{port}/f");
    let err = registry
        .create(&url, IoFlags::WRITE, &Dict::new(), &e)
        .err()
        .unwrap();
    assert!(matches!(err, vaco_protocol_core::ProtocolError::Denied { .. }));
}
