//! End-to-end: a real file through the registry, the gate and the buffer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::Write;

use vaco_io::{CancelToken, IoContext, IoOptions, IoWriter, Seekability};
use vaco_opts::Dict;
use vaco_protocol_core::{
    DenyReason, IoFlags, ProtocolEnv, ProtocolError, ProtocolRegistry, split_url,
};
use vaco_protocol_file::{register, url_to_path};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    register(&mut r);
    r
}

fn sample(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.path().join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(bytes).unwrap();
    p
}

#[test]
fn read_a_real_file_through_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
    let path = sample(&dir, "clip.bin", &payload);

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let src = r
        .open(path.to_str().unwrap(), IoFlags::READ, &Dict::new(), &env)
        .unwrap();

    assert_eq!(src.seekability(), Seekability::Cheap);
    assert_eq!(src.size(), Some(5000));

    let mut io = IoContext::new(src, &IoOptions::default().with_block_size(256)).unwrap();
    // Probe without consuming, exactly as a demuxer would.
    assert_eq!(io.peek(4).unwrap(), &payload[..4]);
    assert_eq!(io.pos(), 0);

    // Backwards seek outside the buffer is a real seek here.
    io.seek(4096).unwrap();
    assert_eq!(io.r8().unwrap(), payload[4096]);
    io.seek(0).unwrap();

    let mut got = vec![0u8; payload.len()];
    io.read_exact(&mut got).unwrap();
    assert_eq!(got, payload);
    assert_eq!(io.seek_from_end(10).unwrap(), 4990);
}

#[test]
fn write_a_real_file_and_read_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.bin");

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    let sink = r
        .create(path.to_str().unwrap(), IoFlags::WRITE, &Dict::new(), &env)
        .unwrap();
    {
        let mut w = IoWriter::new(sink, &IoOptions::default().with_block_size(64)).unwrap();
        w.wb32(0).unwrap();
        w.write_tag(b"ftyp").unwrap();
        w.write(&[0x11; 100]).unwrap();
        let total = w.pos();
        w.seek(0).unwrap();
        w.wb32(total as u32).unwrap();
        w.flush().unwrap();
    }

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 108);
    assert_eq!(&bytes[..4], &108u32.to_be_bytes());
    assert_eq!(&bytes[4..8], b"ftyp");
}

#[test]
fn file_url_spellings() {
    let cases = [
        ("clip.mkv", "clip.mkv"),
        ("file:clip.mkv", "clip.mkv"),
        ("file:/tmp/clip.mkv", "/tmp/clip.mkv"),
        ("file:///tmp/clip.mkv", "/tmp/clip.mkv"),
        ("file://localhost/tmp/clip.mkv", "/tmp/clip.mkv"),
    ];
    for (url, want) in cases {
        let p = url_to_path(&split_url(url)).unwrap();
        assert_eq!(p.to_str().unwrap(), want, "{url}");
    }
    // A remote authority is refused, not silently reinterpreted.
    assert!(matches!(
        url_to_path(&split_url("file://evil.example/share/x")),
        Err(ProtocolError::Malformed { .. })
    ));
}

#[test]
fn access_check_and_directory_listing() {
    let dir = tempfile::tempdir().unwrap();
    sample(&dir, "a.bin", b"a");
    sample(&dir, "b.bin", b"bb");

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);

    let access = r
        .check(dir.path().join("a.bin").to_str().unwrap(), &env)
        .unwrap();
    assert!(access.read);

    let listing = r.list_dir(dir.path().to_str().unwrap(), &env).unwrap();
    let names: Vec<&str> = listing.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.bin", "b.bin"]);
    assert_eq!(listing[1].size, Some(2));
}

// ------------------------------------------------------------------- rule U2

#[test]
fn root_confinement_allows_inside_and_refuses_outside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("ok.bin"), b"inside").unwrap();
    std::fs::write(dir.path().join("secret.bin"), b"outside").unwrap();

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_root(&root);

    // A relative name resolves against the root, not the process cwd.
    assert!(r.open("ok.bin", IoFlags::READ, &Dict::new(), &env).is_ok());

    // Traversal out of the root is refused.
    for escape in ["../secret.bin", "./../secret.bin"] {
        match r.open(escape, IoFlags::READ, &Dict::new(), &env) {
            Err(ProtocolError::Denied { reason, .. }) => {
                assert_eq!(reason, DenyReason::OutsideRoot, "{escape}");
            }
            Ok(_) => panic!("{escape} escaped the root"),
            Err(e) => panic!("{escape}: unexpected {e:?}"),
        }
    }

    // So is an absolute path elsewhere.
    let abs = dir.path().join("secret.bin");
    assert!(matches!(
        r.open(abs.to_str().unwrap(), IoFlags::READ, &Dict::new(), &env),
        Err(ProtocolError::Denied { .. })
    ));
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_root_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(dir.path().join("secret.bin"), b"outside").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret.bin"), root.join("link.bin")).unwrap();

    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_root(&root);

    // Confinement is by canonical path, so the link resolves out and is refused.
    assert!(matches!(
        r.open("link.bin", IoFlags::READ, &Dict::new(), &env),
        Err(ProtocolError::Denied { .. })
    ));

    // Without a root the same open is fine — confinement is opt-in.
    let open_env = ProtocolEnv::new(&r, &cancel);
    assert!(
        r.open(
            root.join("link.bin").to_str().unwrap(),
            IoFlags::READ,
            &Dict::new(),
            &open_env
        )
        .is_ok()
    );
}

// -------------------------------------------------------------------- pipe:

#[test]
fn pipe_rejects_descriptors_it_cannot_own_safely() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel);
    for url in ["pipe:3", "pipe:7"] {
        match r.open(url, IoFlags::READ, &Dict::new(), &env) {
            Err(ProtocolError::Unsupported { scheme, .. }) => assert_eq!(scheme, "pipe"),
            Ok(_) => panic!("{url} should not open"),
            Err(e) => panic!("{url}: unexpected {e:?}"),
        }
    }
    assert!(matches!(
        r.open("pipe:nonsense", IoFlags::READ, &Dict::new(), &env),
        Err(ProtocolError::Malformed { .. })
    ));
}

#[test]
fn the_gate_applies_to_file_too() {
    let r = registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&r, &cancel).with_whitelist(&["http"]);
    assert!(matches!(
        r.open("clip.mkv", IoFlags::READ, &Dict::new(), &env),
        Err(ProtocolError::Denied {
            reason: DenyReason::NotWhitelisted,
            ..
        })
    ));
}
