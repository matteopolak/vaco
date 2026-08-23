//! ICY/SHOUTcast in-band metadata: interleaved blocks must be stripped from
//! the audio stream a demuxer sees, and the most recent block's text must be
//! recoverable. Probed shape (there is no RFC — see `crate::source`'s module
//! docs): one length byte `L`, then `L * 16` bytes of metadata, every
//! `icy-metaint` audio bytes.
//!
//! A hand-rolled loopback server, not `tests/support` (that module answers
//! plain `Range` requests; this needs a raw interleaved body and a
//! `icy-metaint` response header neither reference nor build).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "tests"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use vaco_io::RawSource;
use vaco_protocol_http::options::HttpOptions;
use vaco_protocol_http::{HttpSource, headers, transport};

/// One ICY metadata block: a length byte, then the text padded to the next
/// multiple of 16 with NUL bytes (empty `text` still emits the block with
/// `length = 0` and no payload bytes, matching "nothing changed").
fn metadata_block(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return vec![0u8];
    }
    let mut payload = text.as_bytes().to_vec();
    let padded = payload.len().div_ceil(16) * 16;
    payload.resize(padded, 0);
    let len_byte = u8::try_from(padded / 16).expect("test fixture stays under 255*16 bytes");
    let mut out = vec![len_byte];
    out.extend(payload);
    out
}

fn spawn_icy_server(metaint: usize, audio_chunks: &[&[u8]], metadata: &[&str]) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let mut body = Vec::new();
    for (chunk, meta) in audio_chunks.iter().zip(metadata.iter()) {
        assert_eq!(chunk.len(), metaint, "test fixture chunk must match metaint");
        body.extend_from_slice(chunk);
        body.extend(metadata_block(meta));
    }
    // A trailing partial chunk with no metadata block after it, then EOF —
    // the common "stream just ended" case.
    if let Some(last) = audio_chunks.get(metadata.len()) {
        body.extend_from_slice(last);
    }

    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                    break;
                }
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nicy-metaint: {metaint}\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            drop(stream);
        }
    });
    addr
}

fn open_source(addr: std::net::SocketAddr) -> HttpSource {
    let target = format!("http://{addr}/stream");
    let opts = HttpOptions::default();
    let hdrs = headers::build(&opts, None, None);
    let response = transport::send("GET", &target, &hdrs, None).expect("send");
    HttpSource::from_first_response(target, None, opts, None, response, 0).expect("adopt")
}

fn read_all(src: &mut HttpSource) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 7]; // deliberately not aligned with metaint or block sizes
    loop {
        let n = src.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

#[test]
fn interleaved_metadata_is_stripped_from_the_audio_stream() {
    let a = [b'A'; 16];
    let b = [b'B'; 16];
    let c = [b'C'; 8];
    let addr = spawn_icy_server(16, &[&a, &b, &c], &["StreamTitle='Hello';", ""]);

    let mut src = open_source(addr);
    let audio = read_all(&mut src);

    let mut expected = Vec::new();
    expected.extend_from_slice(&a);
    expected.extend_from_slice(&b);
    expected.extend_from_slice(&c);
    assert_eq!(audio, expected, "audio bytes must be exactly the un-interleaved chunks");
}

#[test]
fn the_most_recent_non_empty_metadata_block_is_recoverable() {
    let a = [b'A'; 16];
    let b = [b'B'; 16];
    let c = [b'C'; 16];
    let addr = spawn_icy_server(
        16,
        &[&a, &b, &c],
        &["StreamTitle='First';", "StreamTitle='Second';"],
    );

    let mut src = open_source(addr);
    assert_eq!(src.icy_metadata(), None, "no metadata read yet");
    let _ = read_all(&mut src);
    assert_eq!(src.icy_metadata(), Some("StreamTitle='Second';"));
}

#[test]
fn an_empty_metadata_block_leaves_the_previous_title_in_place() {
    // Measured-shape assumption: `length == 0` means "nothing changed", not
    // "clear the title" — the block is consumed (or the audio stream would
    // desync) but `last_metadata` is left untouched.
    let a = [b'A'; 16];
    let b = [b'B'; 16];
    let addr = spawn_icy_server(16, &[&a, &b], &["StreamTitle='Only';", ""]);

    let mut src = open_source(addr);
    let _ = read_all(&mut src);
    assert_eq!(src.icy_metadata(), Some("StreamTitle='Only';"));
}

#[test]
fn no_icy_metaint_header_means_no_icy_state_at_all() {
    // A plain server, no `icy-metaint`: `icy_metadata()` must be `None` and
    // the byte stream must be completely unmodified (the fast path in
    // `RawSource::read` that skips the de-interleaving loop entirely).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                    break;
                }
            }
            let body = b"plain audio bytes, no icy here";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });

    let mut src = open_source(addr);
    assert_eq!(src.icy_metadata(), None);
    let audio = read_all(&mut src);
    assert_eq!(audio, b"plain audio bytes, no icy here");
}
