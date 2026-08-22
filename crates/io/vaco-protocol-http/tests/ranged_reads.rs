//! Ranged reads against a real (loopback-only) HTTP server.
//!
//! This is the crate's central claim: seeking to the end of a large remote
//! file issues a `Range` request for a handful of bytes, not a download of
//! the whole thing — and a server that ignores `Range` entirely is still
//! readable, from the start, without corrupting the reader's position.
//!
//! No external network access: every server here is bound to
//! `127.0.0.1:0` inside the test process (see `tests/support/mod.rs`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

mod support;

use vaco_io::{CancelToken, Seekability};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_http::register(&mut r);
    r
}

#[test]
fn seeking_to_the_end_of_a_large_file_transfers_a_small_number_of_bytes() {
    // Large enough that "read it all, then look at the tail" and "issue a
    // Range request for the tail" are trivially distinguishable by size.
    let body: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let server = support::spawn(body.clone(), support::Behavior::HonorRange);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let url = format!("http://{}/big.bin", server.addr);

    let mut src = r
        .open(&url, IoFlags::READ, &Dict::new(), &env)
        .expect("open");
    assert_eq!(src.seekability(), Seekability::Expensive);
    assert_eq!(src.size(), Some(body.len() as u64));

    // Model what a real demuxer does: probe a small header before deciding
    // to jump elsewhere (an MP4 reader looking at `ftyp`, say). This is the
    // realistic shape of the scenario the brief describes ("a remote MP4
    // whose `moov` is at the end") — a raw "open, then seek immediately"
    // races the server's own eagerness to fill the socket buffer on a
    // same-host loopback connection, which is a test-harness artifact, not
    // the property under test.
    let mut probe = [0_u8; 64];
    src.read_exact(&mut probe).expect("probe the header");
    assert_eq!(&probe[..], &body[..64]);

    let target = body.len() as u64 - 16;
    let reached = src.seek(target).expect("seek near the end");
    assert_eq!(reached, target);

    let mut tail = [0_u8; 16];
    src.read_exact(&mut tail).expect("read the last 16 bytes");
    assert_eq!(&tail[..], &body[body.len() - 16..]);

    // Two requests: the initial open and the seek.
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    // The claim this test exists to prove: the *seek's own* response — not
    // the cumulative total, which the first response's legitimate
    // `bytes=0-` (the whole resource, correctly, since no `-offset` was
    // given) would dominate regardless of how the seek behaves — is small.
    // A linear "read to the target and discard" implementation would instead
    // make this response (or an even larger read against the first
    // connection) carry most of the file.
    let sizes = server.response_sizes.lock().expect("lock");
    let seek_response_bytes = *sizes.get(1).expect("a second response was recorded");
    assert!(
        seek_response_bytes < 4096,
        "expected the seek's own response to be small (a Range request for 16 bytes \
         plus headers), got {seek_response_bytes} bytes for an 8 MiB file"
    );
}

#[test]
fn a_server_that_ignores_range_is_still_readable_from_the_start() {
    let body = b"the quick brown fox jumps over the lazy dog".to_vec();
    let server = support::spawn(body.clone(), support::Behavior::IgnoreRange);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let url = format!("http://{}/x", server.addr);

    let mut src = r
        .open(&url, IoFlags::READ, &Dict::new(), &env)
        .expect("open");
    // The reference itself: a 200 (not 206) response to the probing
    // `Range: bytes=0-` means "not seekable", not "broken".
    assert_eq!(src.seekability(), Seekability::None);

    let mut got = Vec::new();
    let mut buf = [0_u8; 64];
    loop {
        let n = src.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        got.extend_from_slice(buf.get(..n).expect("n <= buf.len()"));
    }
    assert_eq!(got, body);
}

#[test]
fn seek_when_range_is_ignored_by_a_later_request_errors_instead_of_corrupting() {
    let body: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    let server = support::spawn(body, support::Behavior::FlakyRange);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let url = format!("http://{}/x", server.addr);

    let mut src = r
        .open(&url, IoFlags::READ, &Dict::new(), &env)
        .expect("open");
    // The first response was 206 (FlakyRange honours it once), so this
    // reader believes it is seekable.
    assert_eq!(src.seekability(), Seekability::Expensive);

    // The seek issues a second request, which this server answers with a
    // bare 200 (full body, ignoring the non-zero Range) — the exact scenario
    // the crate must refuse rather than silently serve wrong-offset bytes.
    let err = src
        .seek(512)
        .expect_err("a flaky server must not corrupt position");
    let _ = err; // the specific message is not a contract; only "it errors" is.
}
