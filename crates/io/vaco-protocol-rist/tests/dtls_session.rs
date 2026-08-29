//! §6.1's claim, checked for real: *"one single DTLS session carrying the
//! RFC 8086 tunnel packets."* This drives two genuinely independent
//! [`vaco_protocol_dtls`] handshakes to completion over loopback UDP (a
//! client and a server, both with ephemeral self-signed certificates — the
//! same DTLS stack already interop-verified against `ffmpeg 8.1`), then
//! pushes a real [`vaco_protocol_rist::gre::GreHeader`]-framed tunnel packet
//! through the resulting stream in both directions.
//!
//! This is a self-consistency test (this crate's own two sides agreeing),
//! the same evidence class the rest of this crate's tests are already
//! labelled with — there is no `librist` build on this machine to check
//! against (see the crate docs), and no DTLS-capable RIST reference either.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::{Read, Write};
use std::net::UdpSocket;
use std::time::Duration;

use vaco_io::CancelToken;
use vaco_protocol_core::{ProtocolEnv, ProtocolRegistry};
use vaco_protocol_dtls::connect;
use vaco_protocol_dtls::listen;
use vaco_protocol_dtls::options::DtlsOptions;
use vaco_protocol_rist::dtls::negotiated_cipher_is_required_suite;
use vaco_protocol_rist::gre::{GreHeader, RistVersion};
use vaco_protocol_socket::url::HostPort;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

fn tunnel_packet(seq: u32, payload: &[u8]) -> Vec<u8> {
    let header = GreHeader {
        checksum: None,
        key_or_nonce: None,
        sequence_number: Some(seq),
        h: false,
        rv: RistVersion::V2022,
        // RIST over GRE per `TR-06-2` §5.2's `Protocol Type` field.
        protocol_type: 0xB5E2,
    };
    let mut out = header.serialize();
    out.extend_from_slice(payload);
    out
}

#[test]
fn one_dtls_session_carries_rist_tunnel_packets_both_ways() {
    // Reserve a free port the same way `vaco-protocol-dtls`'s own
    // `bind_accept` test does: bind port 0, read it back, drop, then hand
    // that port to the real bind. A tiny race in principle; accepted
    // practice already in this codebase (see `vaco-protocol-dtls::listen`'s
    // own test module).
    let probe = UdpSocket::bind("127.0.0.1:0").expect("reserve a port");
    let port = probe.local_addr().expect("local_addr").port();
    drop(probe);
    let hp = HostPort {
        host: "127.0.0.1".to_owned(),
        port,
    };

    let server_hp = hp.clone();
    let server = std::thread::spawn(move || {
        let socket =
            listen::bind_accept(&server_hp, Some(HANDSHAKE_TIMEOUT)).expect("server bind/accept");
        listen::handshake(socket, &DtlsOptions::default(), None, None, None)
            .expect("server handshake")
    });

    // Give the server a moment to be bound and waiting before the client's
    // first flight goes out — matching the same ordering
    // `vaco-protocol-dtls::listen`'s own test uses.
    std::thread::sleep(Duration::from_millis(50));

    let registry = ProtocolRegistry::new();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["udp"]);
    let client_socket =
        connect::connect_udp(&hp, Some(HANDSHAKE_TIMEOUT), &env).expect("client connect_udp");
    let mut client_stream =
        connect::handshake(client_socket, &DtlsOptions::default(), None, None, None)
            .expect("client handshake");

    let mut server_stream = server.join().expect("server thread panicked");

    // §6.2: whatever suite was actually negotiated must be one of the five
    // mandatory ones. Both ends of one session negotiate the same cipher by
    // construction, but this checks both independently rather than assuming
    // symmetry.
    assert!(
        negotiated_cipher_is_required_suite(&client_stream),
        "client's negotiated cipher was not one of TR-06-2 section 6.2's mandatory suites"
    );
    assert!(
        negotiated_cipher_is_required_suite(&server_stream),
        "server's negotiated cipher was not one of TR-06-2 section 6.2's mandatory suites"
    );

    // §6.1: the tunnel packet IS the DTLS application data, no extra framing.
    let client_to_server = tunnel_packet(1, b"rist-main-profile-tunnel-payload");
    client_stream
        .write_all(&client_to_server)
        .expect("client write");
    let mut received = vec![0_u8; client_to_server.len() + 64];
    let n = server_stream.read(&mut received).expect("server read");
    assert_eq!(
        received.get(..n),
        Some(client_to_server.as_slice()),
        "server did not receive the client's tunnel packet byte-for-byte"
    );

    // And the reverse direction, over the same one session — proving it is
    // genuinely full-duplex, not two one-way pipes.
    let server_to_client = tunnel_packet(2, b"reverse-direction-rtcp-feedback");
    server_stream
        .write_all(&server_to_client)
        .expect("server write");
    let mut received_back = vec![0_u8; server_to_client.len() + 64];
    let n_back = client_stream.read(&mut received_back).expect("client read");
    assert_eq!(
        received_back.get(..n_back),
        Some(server_to_client.as_slice()),
        "client did not receive the server's tunnel packet byte-for-byte"
    );
}
