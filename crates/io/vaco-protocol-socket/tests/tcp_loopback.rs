//! `tcp:` against a real loopback listener — no external network, per the
//! brief's "tests must not need the network" rule.

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
    vaco_protocol_socket::register(&mut r);
    r
}

#[test]
fn connects_and_reads_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello over tcp").unwrap();
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    let url = format!("tcp://{addr}");
    let mut src = r.open(&url, IoFlags::READ, &Dict::new(), &env).unwrap();

    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello over tcp");

    server.join().unwrap();
}

#[test]
fn writes_bytes_through_create() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).unwrap();
        buf[..n].to_vec()
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    let url = format!("tcp://{addr}");
    let mut sink = r
        .create(&url, IoFlags::WRITE, &Dict::new(), &env)
        .unwrap();
    vaco_io::MediaSink::write(&mut *sink, b"ping").unwrap();
    drop(sink);

    let got = server.join().unwrap();
    assert_eq!(got, b"ping");
}

#[test]
fn tcp_needs_to_be_on_the_whitelist_itself() {
    // `tcp:` opens nothing nested, so there is no default grant question —
    // but the scheme itself still needs to be named (or the caller must be
    // unrestricted). See the crate docs' "Security" section.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http"]);
    let url = format!("tcp://{addr}");
    let err = r.open(&url, IoFlags::READ, &Dict::new(), &env).err();
    assert!(matches!(
        err,
        Some(vaco_protocol_core::ProtocolError::Denied { .. })
    ));
}

#[test]
fn connection_refused_is_reported_as_an_error_not_a_panic() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    let url = format!("tcp://{addr}?timeout=500000");
    assert!(r.open(&url, IoFlags::READ, &Dict::new(), &env).is_err());
}

#[test]
fn malformed_url_is_an_error_not_a_panic() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    for url in ["tcp://", "tcp://host-with-no-port", "tcp://:"] {
        assert!(r.open(url, IoFlags::READ, &Dict::new(), &env).is_err());
    }
}
