//! Exercises `fetch::fetch` end to end against a real socket — a loopback
//! HTTP server bound to `127.0.0.1:0` (OS-assigned port), never an external
//! host. This is the same technique `vaco-protocol-http`'s own tests use
//! (`tests/support/mod.rs` there); reproduced here in miniature rather than
//! shared, because it is a few dozen lines and pulling in a whole other
//! crate's `tests/` directory is not how Cargo test helpers compose across
//! crates.
//!
//! CI's lack of internet access is irrelevant to this file: nothing here
//! resolves a DNS name or leaves the loopback interface.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test-support code, not the crate under test"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use vaco_corpus::fetch::{self, NetworkPolicy};
use vaco_corpus::lock::LockEntry;
use vaco_corpus::store::{ObjectId, Store};

/// Spawn a server that answers every request `200 OK` with `body`.
fn spawn(body: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            serve_one(stream, &body);
        }
    });
    addr
}

fn serve_one(mut stream: TcpStream, body: &[u8]) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn entry(name: &str, url: String, sha: &ObjectId) -> LockEntry {
    LockEntry {
        name: name.to_owned(),
        suite: "test".to_owned(),
        url: Some(url),
        sha256: Some(sha.clone()),
        size: None,
        license: "test".to_owned(),
        source: "loopback test server".to_owned(),
        targets: vec![],
    }
}

#[test]
fn fetch_verifies_and_populates_the_store() {
    let body = b"hello from the loopback corpus server".to_vec();
    let addr = spawn(body.clone());
    let id = ObjectId::of(&body);
    let e = entry("t", format!("http://{addr}/asset"), &id);

    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());
    assert!(!store.has(&id));

    let got = fetch::fetch(&e, &store, NetworkPolicy::Allowed).expect("fetch succeeds");
    assert_eq!(got, body);
    assert!(store.has(&id));

    // Second call is a cache hit and must succeed even with the network
    // disallowed.
    let got_again = fetch::fetch(&e, &store, NetworkPolicy::CacheOnly).expect("cache hit");
    assert_eq!(got_again, body);
}

#[test]
fn fetch_rejects_a_server_that_serves_the_wrong_bytes() {
    let addr = spawn(b"actual server content".to_vec());
    // Claim a hash that does NOT match what the server will serve.
    let wrong_hash = ObjectId::of(b"what the lock file expected");
    let e = entry("t", format!("http://{addr}/asset"), &wrong_hash);

    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());

    let err = fetch::fetch(&e, &store, NetworkPolicy::Allowed).unwrap_err();
    assert!(matches!(err, fetch::FetchError::Store(_)));
    assert!(!store.has(&wrong_hash), "a hash mismatch must store nothing");
}
