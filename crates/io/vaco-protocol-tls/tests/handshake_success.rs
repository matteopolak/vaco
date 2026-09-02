//! A full TLS handshake against a real (in-process, loopback) `rustls`
//! server, exercising the paths that never touch a socket the fixture files
//! above only unit-test in isolation: the whole `Protocol::open` path, the
//! `verify = true` (chain-checked, against a private CA) success case, and
//! the `verify = false` default's success case against a certificate that
//! would fail chain validation (self-signed, not in any root store).
//!
//! No external network: the "peer" is a `rustls::ServerConnection` this test
//! drives by hand over a loopback `TcpStream`, and the certificate is a
//! fixture generated once, offline (`tests/fixtures/localhost-{cert,key}.pem`
//! — a 10-year self-signed RSA certificate for `CN=localhost`, `openssl req
//! -x509 -newkey rsa:2048 -days 3650 -nodes -subj "/CN=localhost" -addext
//! "subjectAltName=DNS:localhost,IP:127.0.0.1"`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "tests"
)]

use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

const CERT_PEM: &str = include_str!("fixtures/localhost-cert.pem");
const KEY_PEM: &str = include_str!("fixtures/localhost-key.pem");

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_tls::register(&mut r);
    r
}

fn server_config() -> Arc<ServerConfig> {
    let certs: Vec<CertificateDer<'static>> =
        vaco_protocol_tls::pem::extract_der_blocks(CERT_PEM, "CERTIFICATE")
            .unwrap()
            .into_iter()
            .map(CertificateDer::from)
            .collect();
    let key_der = vaco_protocol_tls::pem::extract_der_blocks(KEY_PEM, "PRIVATE KEY")
        .unwrap()
        .into_iter()
        .next()
        .expect("fixture key has one PKCS8 block");
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    Arc::new(config)
}

/// Accept one connection and complete the server side of the handshake, then
/// write `payload` and return.
fn serve_one(listener: TcpListener, config: Arc<ServerConfig>, payload: &'static [u8]) {
    let (tcp, _) = listener.accept().unwrap();
    let conn = ServerConnection::new(config).unwrap();
    let mut stream = StreamOwned::new(conn, tcp);
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

#[test]
fn default_verify_false_succeeds_against_a_self_signed_certificate() {
    // The whole point of the measured `verify = false` default: a
    // certificate that would fail chain validation (self-signed, signed by
    // nothing any root store trusts) is still accepted, because this is the
    // reference's own default and this crate matches it. See
    // `src/verify.rs`'s module docs for exactly what still IS checked.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config();
    let server = thread::spawn(move || serve_one(listener, config, b"hello from tls server"));

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tls", "tcp"]);
    let url = format!("tls://localhost:{}", addr.port());
    // `localhost` resolves via `/etc/hosts` on every platform this crate is
    // developed on; if that ever proves flaky in CI, switch to dialing
    // `127.0.0.1` directly and setting `-verifyhost localhost` instead.
    let mut src = r.open(&url, IoFlags::READ, &Dict::new(), &env).unwrap();

    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello from tls server");

    server.join().unwrap();
}

#[test]
fn verify_true_succeeds_against_a_trusted_private_ca() {
    // The fixture certificate is self-signed, so it is its own CA: passing
    // it back as `-ca_file` and turning verification on must succeed,
    // proving the `verify = true` path (chain + hostname) genuinely runs
    // rather than merely being wired to a config option that does nothing.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config();
    let server = thread::spawn(move || serve_one(listener, config, b"trusted hello"));

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tls", "tcp"]);
    let url = format!("tls://localhost:{}", addr.port());

    let mut opts = Dict::new();
    opts.set("verify", "1");
    opts.set(
        "ca_file",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/localhost-cert.pem"
        ),
    );

    let mut src = r.open(&url, IoFlags::READ, &opts, &env).unwrap();
    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"trusted hello");

    server.join().unwrap();
}

#[test]
fn verify_true_without_the_private_ca_is_refused() {
    // Same server, same certificate, but this time verification runs against
    // only the public root store (no `-ca_file`) — a self-signed certificate
    // must be refused, or `verify = true` would not be testing anything.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config();
    let server = thread::spawn(move || {
        // The client is expected to abort the handshake, so the server's
        // own write will typically fail too; both outcomes are fine here.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            serve_one(listener, config, b"should never be read");
        }));
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tls", "tcp"]);
    let url = format!("tls://localhost:{}", addr.port());
    let mut opts = Dict::new();
    opts.set("verify", "1");

    assert!(r.open(&url, IoFlags::READ, &opts, &env).is_err());
    let _ = server.join();
}
