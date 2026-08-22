//! A from-scratch, minimal HTTP/1.1 server for the integration tests in this
//! directory.
//!
//! Not `mod.rs` inside `tests/` directly — it lives in a subdirectory
//! (`tests/support/mod.rs`), which is the standard way to give integration
//! tests a shared helper without Cargo compiling the helper itself as its own
//! test binary (only files directly under `tests/` become one).
//!
//! # Why hand-rolled rather than a dependency
//!
//! No mock-HTTP-server crate is in `[workspace.dependencies]` (D10: an agent
//! stops and asks rather than adding one silently), and what this needs is
//! small: parse a request line and headers well enough to read the method,
//! path and `Range` header, then write a status line, a couple of headers and
//! a body slice. That is a few dozen lines, not dependency-shaped.
//!
//! # Why this is not "a network request in a test"
//!
//! Every server here binds `127.0.0.1:0` (OS-assigned loopback port) and every
//! client in this crate's tests connects only to that address. Nothing
//! reaches an external host, so CI's lack of internet access is irrelevant —
//! see plan 19's brief: "bind a local server on port 0 in the test itself".

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "not every test file uses every helper; this is test-support code"
)]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// How the server responds to a request.
#[derive(Clone)]
pub(crate) enum Behavior {
    /// Honour `Range` (respond `206` with `Content-Range` when a `Range`
    /// header is present, `200` with the whole body otherwise).
    HonorRange,
    /// Always `200` with the whole body, regardless of any `Range` header —
    /// the common "this server does not support ranges" case.
    IgnoreRange,
    /// Honour `Range` normally on the first request, then ignore it (always
    /// `200`, whole body) on every request after that — simulates a flaky or
    /// inconsistent server for the "must not corrupt position" case.
    FlakyRange,
    /// Answer every request with a redirect.
    Redirect { status: u16, location: String },
}

/// A running server and the counters its handler updates.
pub(crate) struct TestServer {
    pub addr: SocketAddr,
    /// Total bytes written across every response so far. Useful as a coarse
    /// sanity check, but **not** by itself proof of a small transfer for a
    /// request whose `Range` legitimately covers the whole resource (an
    /// initial `bytes=0-` open with no `-offset` does, correctly) — see
    /// [`TestServer::response_sizes`] for the per-request answer that
    /// actually distinguishes "the initial read" from "the seek".
    pub bytes_sent: Arc<AtomicU64>,
    pub requests: Arc<AtomicU64>,
    /// Bytes written for each response, in the order requests were
    /// *accepted* (not necessarily the order they finished, but connections
    /// in these tests are made one after another by a single client thread,
    /// so acceptance order and client-request order coincide).
    pub response_sizes: Arc<Mutex<Vec<u64>>>,
    // Keeping the join handle around is not necessary for correctness (the
    // thread is detached in spirit — it runs until the test binary exits),
    // but holding it means a `TestServer` cannot be silently dropped and its
    // listener closed out from under an in-flight test by mistake.
    _handle: std::thread::JoinHandle<()>,
}

/// Spawn a server that serves `body` for any path, per `behavior`.
///
/// One thread per accepted connection, deliberately — this crate's own
/// `HttpSource` can and does hold a first response's body unread while a
/// `seek` opens a *second* connection to the same server (that is the whole
/// point of a ranged read: the caller does not have to drain the first
/// response first). A single-threaded accept loop that blocks writing an
/// unread body would then deadlock waiting to `accept()` the second
/// connection. A real server always accepts concurrently; this one now does
/// too.
#[must_use]
pub(crate) fn spawn(body: Vec<u8>, behavior: Behavior) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let requests = Arc::new(AtomicU64::new(0));
    let response_sizes: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let body = Arc::new(body);

    let handle = {
        let bytes_sent = Arc::clone(&bytes_sent);
        let requests = Arc::clone(&requests);
        let response_sizes = Arc::clone(&response_sizes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let n = requests.fetch_add(1, Ordering::SeqCst);
                let respond_with_range = match &behavior {
                    Behavior::HonorRange => true,
                    Behavior::FlakyRange => n == 0,
                    Behavior::IgnoreRange | Behavior::Redirect { .. } => false,
                };
                // Reserve this request's slot *now*, in accept order — the
                // per-connection thread below fills it in whenever it
                // finishes, which for a huge first response and a tiny
                // second one is often the other way around.
                {
                    let mut sizes = response_sizes.lock().expect("lock");
                    sizes.resize(usize::try_from(n).unwrap_or(0) + 1, 0);
                }
                let body = Arc::clone(&body);
                let behavior = behavior.clone();
                let bytes_sent = Arc::clone(&bytes_sent);
                let response_sizes = Arc::clone(&response_sizes);
                std::thread::spawn(move || {
                    let written =
                        serve_one(stream, &body, respond_with_range, &behavior, &bytes_sent);
                    if let Ok(mut sizes) = response_sizes.lock()
                        && let Some(slot) = sizes.get_mut(usize::try_from(n).unwrap_or(0))
                    {
                        *slot = written;
                    }
                });
            }
        })
    };

    TestServer {
        addr,
        bytes_sent,
        requests,
        response_sizes,
        _handle: handle,
    }
}

/// Convenience for the redirect-focused tests: no body ever matters.
#[must_use]
pub(crate) fn spawn_redirect(status: u16, location: &str) -> TestServer {
    spawn(
        Vec::new(),
        Behavior::Redirect {
            status,
            location: location.to_owned(),
        },
    )
}

/// Serve one connection, returning the number of bytes written for it.
fn serve_one(
    mut stream: TcpStream,
    body: &[u8],
    honor_range: bool,
    behavior: &Behavior,
    bytes_sent: &AtomicU64,
) -> u64 {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return 0;
    }

    let mut range: Option<(u64, Option<u64>)> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
        {
            range = parse_range(value.trim());
        }
    }

    if let Behavior::Redirect { status, location } = behavior {
        let response = format!(
            "HTTP/1.1 {status} Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes());
        let n = response.len() as u64;
        bytes_sent.fetch_add(n, Ordering::SeqCst);
        return n;
    }

    let (status, start, end, total) = match (honor_range, range) {
        (true, Some((s, e))) => {
            let e = e.unwrap_or(body.len().saturating_sub(1) as u64);
            (
                206_u16,
                s,
                e.min(body.len().saturating_sub(1) as u64),
                body.len(),
            )
        }
        _ => (200_u16, 0, body.len().saturating_sub(1) as u64, body.len()),
    };

    let start_usize = usize::try_from(start).unwrap_or(0);
    let end_usize = usize::try_from(end).unwrap_or(0);
    let slice = body
        .get(start_usize..=end_usize.min(body.len().saturating_sub(1)))
        .unwrap_or(&[]);

    let mut head = format!(
        "HTTP/1.1 {status} {}\r\n",
        if status == 206 {
            "Partial Content"
        } else {
            "OK"
        }
    );
    if status == 206 {
        let _ = write!(head, "Content-Range: bytes {start}-{end}/{total}\r\n");
    }
    let _ = write!(head, "Content-Length: {}\r\n", slice.len());
    head.push_str("Accept-Ranges: bytes\r\n");
    head.push_str("Connection: close\r\n\r\n");

    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(slice);
    let n = (head.len() + slice.len()) as u64;
    bytes_sent.fetch_add(n, Ordering::SeqCst);
    n
}

fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = if end.trim().is_empty() {
        None
    } else {
        Some(end.trim().parse::<u64>().ok()?)
    };
    Some((start, end))
}
