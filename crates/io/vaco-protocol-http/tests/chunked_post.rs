//! `Protocol::create`'s chunked POST, against a loopback server that reads a
//! real `Transfer-Encoding: chunked` body (proving the wire format, not just
//! that some bytes arrived — a fixed-`Content-Length` body would also make a
//! naive assertion pass).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::thread;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_http::register(&mut r);
    r
}

/// Read a request's headers, then decode a `Transfer-Encoding: chunked` body
/// (the dechunking algorithm is small enough to write by hand for a test
/// server, same reasoning as `tests/support/mod.rs`'s own header parsing).
fn read_chunked_request(stream: std::net::TcpStream) -> (Vec<(String, String)>, Vec<u8>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader.read_line(&mut size_line).unwrap();
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap();
        if size == 0 {
            // Trailing CRLF after the terminating zero-size chunk.
            let mut trailer = String::new();
            let _ = reader.read_line(&mut trailer);
            break;
        }
        let mut chunk = vec![0u8; size];
        std::io::Read::read_exact(&mut reader, &mut chunk).unwrap();
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        std::io::Read::read_exact(&mut reader, &mut crlf).unwrap();
    }

    (headers, body)
}

#[test]
fn a_written_and_flushed_body_arrives_chunked_and_intact() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let (headers, body) = read_chunked_request(stream);
        (headers, body)
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http"]);
    let url = format!("http://{addr}/upload");

    // The server above only reads one request and never writes a response
    // (accepting the connection is enough to prove the chunked framing);
    // `flush()` will see the connection close before a response arrives.
    // What matters for this test is what the server actually received.
    let mut sink = r
        .create(&url, IoFlags::WRITE, &Dict::new(), &env)
        .unwrap();
    sink.write(b"hello, ").unwrap();
    sink.write(b"chunked ").unwrap();
    sink.write(b"world").unwrap();
    // `flush()` sends the request; the server never answers, so this is
    // expected to report an I/O error (connection closed with no response) —
    // the framing on the wire is what this test actually checks, captured by
    // the server thread regardless of what `flush()` itself returns.
    let _ = sink.flush();

    let (headers, body) = server.join().unwrap();
    assert_eq!(body, b"hello, chunked world");
    let has_chunked_header = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked"));
    assert!(has_chunked_header, "headers were: {headers:?}");
    let has_content_length = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    assert!(
        !has_content_length,
        "a chunked request must not also carry Content-Length: {headers:?}"
    );
}

#[test]
fn writing_after_flush_is_refused_not_panicked() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = read_chunked_request(stream);
        }
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http"]);
    let url = format!("http://{addr}/upload");
    let mut sink = r
        .create(&url, IoFlags::WRITE, &Dict::new(), &env)
        .unwrap();
    sink.write(b"first").unwrap();
    let _ = sink.flush();
    assert!(sink.write(b"too late").is_err());
    let _ = server.join();
}
