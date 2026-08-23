//! `udp:` and `udplite:` against a loopback peer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_socket::register(&mut r);
    r
}

#[test]
fn receives_a_datagram() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["udp"]);

    // Bind our own ephemeral port first so we know where to send to; the
    // protocol under test binds its own listener on a *different* ephemeral
    // port and we send to it.
    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target_addr = probe.local_addr().unwrap();
    drop(probe);
    let url = format!("udp://{target_addr}?timeout=2000000");

    let mut src = r.open(&url, IoFlags::READ, &Dict::new(), &env).unwrap();

    let sender = thread::spawn(move || {
        // Give the receiver a moment to bind before we send.
        thread::sleep(Duration::from_millis(50));
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.send_to(b"hello udp", target_addr).unwrap();
    });

    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello udp");
    sender.join().unwrap();
}

#[test]
fn sends_a_datagram_via_create() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["udp"]);

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = receiver.local_addr().unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let url = format!("udp://{addr}");
    let mut sink = r
        .create(&url, IoFlags::WRITE, &Dict::new(), &env)
        .unwrap();
    sink.write(b"pong").unwrap();

    let mut buf = [0u8; 64];
    let (n, _) = receiver.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"pong");
}

#[test]
fn udplite_registers_under_its_own_scheme() {
    let r = registry();
    assert!(r.find("udplite").is_some());
    assert_eq!(r.find("udplite").unwrap().default_whitelist, &[] as &[&str]);
}

#[test]
fn udp_needs_to_be_on_the_whitelist_itself() {
    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    let url = format!("udp://{addr}");
    let err = r.open(&url, IoFlags::READ, &Dict::new(), &env).err();
    assert!(matches!(
        err,
        Some(vaco_protocol_core::ProtocolError::Denied { .. })
    ));
}
