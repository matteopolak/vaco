//! End-to-end against an in-process fake gopher server, driving the real
//! `Protocol::open`/`create` path.

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

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_gopher::GOPHER_PROTOCOL);
    r
}

fn env<'a>(registry: &'a ProtocolRegistry, cancel: &'a CancelToken) -> ProtocolEnv<'a> {
    ProtocolEnv::new(registry, cancel).with_whitelist(&["gopher", "tcp"])
}

#[test]
fn reads_a_binary_resource() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut sel = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            conn.read_exact(&mut byte).unwrap();
            sel.push(byte[0]);
            if sel.ends_with(b"\r\n") {
                break;
            }
        }
        assert_eq!(sel, b"/some/selector\r\n");
        conn.write_all(b"binary payload bytes").unwrap();
    });

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("gopher://127.0.0.1:{}/9/some/selector", addr.port());
    let mut source = registry.open(&url, IoFlags::READ, &Dict::new(), &e).unwrap();

    let mut got = Vec::new();
    let mut buf = [0u8; 6];
    loop {
        let n = source.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"binary payload bytes");
    handle.join().unwrap();
}

#[test]
fn an_unsupported_type_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // The connection is accepted (measured: the reference connects before
    // checking the type) but nothing is ever read from it.
    let handle = thread::spawn(move || {
        let _ = listener.accept().unwrap();
    });

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("gopher://127.0.0.1:{}/1/menu", addr.port());
    let err = registry
        .open(&url, IoFlags::READ, &Dict::new(), &e)
        .err()
        .unwrap();
    assert!(!matches!(err, vaco_protocol_core::ProtocolError::Denied { .. }));
    handle.join().unwrap();
}

#[test]
fn writes_the_selector_then_raw_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut all = Vec::new();
        conn.read_to_end(&mut all).unwrap();
        all
    });

    let registry = registry();
    let cancel = CancelToken::new();
    let e = env(&registry, &cancel);
    let url = format!("gopher://127.0.0.1:{}/9/out", addr.port());
    {
        // The block scope's end drops `sink`, closing the connection —
        // that close is what signals EOF to `read_to_end` on the thread
        // above.
        let mut sink = registry.create(&url, IoFlags::WRITE, &Dict::new(), &e).unwrap();
        sink.write(b"hello output data").unwrap();
        sink.flush().unwrap();
    }
    let received = handle.join().unwrap();
    assert_eq!(received, b"/out\r\nhello output data");
}
