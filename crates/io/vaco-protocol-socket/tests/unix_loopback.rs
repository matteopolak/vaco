//! `unix:` against a real Unix domain socket. `#[cfg(unix)]`-gated: on any
//! other target `unix:` is the always-`Unsupported` fallback, which
//! `tests/unsupported_targets.rs` covers instead (running everywhere, so
//! there is still coverage even where this file does not compile).

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::Write;
use std::os::unix::net::UnixListener;
use std::thread;

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_socket::register(&mut r);
    r
}

/// A short path in the OS temp directory: `sockaddr_un::sun_path` is capped
/// at 104 (macOS) or 108 (Linux) bytes including the terminator, and a
/// descriptive name plus a long `$TMPDIR` (common on macOS, under
/// `/var/folders/...`) blows past that easily.
fn temp_socket_path(tag: u8) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vps{}-{tag}.s", std::process::id() % 10000));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn connects_and_reads_bytes() {
    let path = temp_socket_path(1);
    let listener = UnixListener::bind(&path).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"hello over unix").unwrap();
    });

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["unix"]);
    let url = format!("unix:{}", path.display());
    let mut src = r.open(&url, IoFlags::READ, &Dict::new(), &env).unwrap();

    let mut buf = [0u8; 64];
    let n = src.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello over unix");

    server.join().unwrap();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn listen_mode_accepts_one_connection() {
    let path = temp_socket_path(2);
    let path_for_open = path.clone();

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["unix"]);
    let url = format!("unix:{}", path.display());
    let mut opts = Dict::new();
    opts.set("listen", "1");
    opts.set("timeout", "5000");

    let got = thread::scope(|scope| {
        let opener = scope.spawn(|| {
            let mut src = r.open(&url, IoFlags::READ, &opts, &env).unwrap();
            let mut buf = [0u8; 64];
            let n = src.read(&mut buf).unwrap();
            buf[..n].to_vec()
        });

        // Give the listener a moment to bind before we dial it.
        for _ in 0..200 {
            if path_for_open.exists() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut client = std::os::unix::net::UnixStream::connect(&path_for_open).unwrap();
        client.write_all(b"from client").unwrap();
        drop(client);

        opener.join().unwrap()
    });
    assert_eq!(got, b"from client");
    let _ = std::fs::remove_file(&path_for_open);
}

#[test]
fn unix_needs_to_be_on_the_whitelist_itself() {
    let path = temp_socket_path(3);
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["tcp"]);
    let url = format!("unix:{}", path.display());
    let err = r.open(&url, IoFlags::READ, &Dict::new(), &env).err();
    assert!(matches!(
        err,
        Some(vaco_protocol_core::ProtocolError::Denied { .. })
    ));
}

#[test]
fn seqpacket_is_reported_as_unsupported_not_a_panic() {
    let path = temp_socket_path(4);
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["unix"]);
    let url = format!("unix:{}", path.display());
    let mut opts = Dict::new();
    opts.set("type", "5");
    let err = r.open(&url, IoFlags::READ, &opts, &env).err();
    assert!(matches!(
        err,
        Some(vaco_protocol_core::ProtocolError::Unsupported { .. })
    ));
}
