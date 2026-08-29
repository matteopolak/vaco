//! A full DTLS handshake between two real `openssl` endpoints over loopback
//! UDP, exercising the whole `Protocol::open`/`create` path: the `-listen 1`
//! server side accepting the client's connection, application data flowing
//! both ways afterward, the `verify = true` success case (chain-checked,
//! against a private CA), and the `verify = false` default's success case
//! against a certificate that would fail chain validation (self-signed, not
//! in any root store) — plus the case that actually tests verification: a
//! self-signed certificate correctly REJECTED under `verify = true` with no
//! matching CA configured.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::thread;
use std::time::Duration;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_dtls::register(&mut r);
    r
}

/// A fresh self-signed certificate/key pair, PEM-encoded — used as the
/// server's identity, and (in the `verify = true` tests) as its own trusted
/// CA, since it is self-signed.
fn fresh_cert_pem() -> (String, String) {
    let (cert, key) = vaco_protocol_dtls::cert::generate_self_signed().unwrap();
    let cert_pem = String::from_utf8(cert.to_pem().unwrap()).unwrap();
    let key_pem = String::from_utf8(key.private_key_to_pem_pkcs8().unwrap()).unwrap();
    (cert_pem, key_pem)
}

#[test]
fn default_verify_false_succeeds_and_carries_data_both_ways() {
    let (cert_pem, key_pem) = fresh_cert_pem();
    let r = registry();
    let cancel = CancelToken::new();

    // Bind the server first so we know the real port to dial.
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let server_cert = cert_pem.clone();
    let server_key = key_pem.clone();
    let server = thread::spawn(move || {
        let registry = {
            let mut r = ProtocolRegistry::new();
            vaco_protocol_dtls::register(&mut r);
            r
        };
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["dtls", "udp"]);
        let mut opts = Dict::new();
        opts.set("listen", "1");
        opts.set("cert_pem", &server_cert);
        opts.set("key_pem", &server_key);
        let url = format!("dtls://127.0.0.1:{port}");
        let mut sink = registry
            .create(&url, IoFlags::WRITE, &opts, &env)
            .expect("server-side accept must succeed");
        sink.write(b"hello from dtls server").unwrap();
        sink.flush().unwrap();
    });

    // Give the server a moment to bind before the client dials.
    thread::sleep(Duration::from_millis(100));

    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["dtls", "udp"]);
    let url = format!("dtls://127.0.0.1:{port}");
    let mut src = r
        .open(&url, IoFlags::READ, &Dict::new(), &env)
        .expect("client-side handshake against a self-signed cert must succeed by default");

    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello from dtls server");

    server.join().unwrap();
}

#[test]
fn verify_true_succeeds_against_a_trusted_private_ca() {
    let (cert_pem, key_pem) = fresh_cert_pem();
    let ca_pem = cert_pem.clone();

    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let server_cert = cert_pem.clone();
    let server_key = key_pem.clone();
    let server = thread::spawn(move || {
        let mut r = ProtocolRegistry::new();
        vaco_protocol_dtls::register(&mut r);
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["dtls", "udp"]);
        let mut opts = Dict::new();
        opts.set("listen", "1");
        opts.set("cert_pem", &server_cert);
        opts.set("key_pem", &server_key);
        let url = format!("dtls://127.0.0.1:{port}");
        let mut sink = r
            .create(&url, IoFlags::WRITE, &opts, &env)
            .expect("server-side accept must succeed");
        sink.write(b"trusted hello").unwrap();
        sink.flush().unwrap();
    });

    thread::sleep(Duration::from_millis(100));

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["dtls", "udp"]);
    let mut opts = Dict::new();
    opts.set("verify", "1");
    let ca_path = write_temp_pem(&ca_pem);
    opts.set("ca_file", ca_path.to_str().unwrap());
    let url = format!("dtls://127.0.0.1:{port}");

    let mut src = r
        .open(&url, IoFlags::READ, &opts, &env)
        .expect("verify=true against the peer's own certificate as CA must succeed");
    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"trusted hello");

    server.join().unwrap();
}

#[test]
fn verify_true_without_the_private_ca_is_refused() {
    // Same shape as the success case, but this time the client verifies
    // against the system root store only (no `-ca_file`) — the server's
    // self-signed certificate must be refused, or `verify = true` would not
    // be testing anything real.
    let (cert_pem, key_pem) = fresh_cert_pem();

    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let server = thread::spawn(move || {
        let mut r = ProtocolRegistry::new();
        vaco_protocol_dtls::register(&mut r);
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["dtls", "udp"]);
        let mut opts = Dict::new();
        opts.set("listen", "1");
        opts.set("cert_pem", &cert_pem);
        opts.set("key_pem", &key_pem);
        let url = format!("dtls://127.0.0.1:{port}");
        // The client is expected to abort the handshake; the server's own
        // accept may then fail too. Both outcomes are fine here.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = r.create(&url, IoFlags::WRITE, &opts, &env);
        }));
    });

    thread::sleep(Duration::from_millis(100));

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["dtls", "udp"]);
    let mut opts = Dict::new();
    opts.set("verify", "1");
    let url = format!("dtls://127.0.0.1:{port}");

    assert!(
        r.open(&url, IoFlags::READ, &opts, &env).is_err(),
        "a self-signed certificate must be REJECTED under verify=true with no matching CA"
    );
    let _ = server.join();
}

fn write_temp_pem(pem: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "vaco-dtls-test-ca-{}-{}.pem",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, pem).unwrap();
    path
}
