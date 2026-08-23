//! The security boundary: W1-W4 and rule U1.
//!
//! Each rule gets a case that fails if the rule is removed. The nesting case is
//! the one that matters most in practice — a hostile playlist reaching `file:`
//! is the vulnerability the whole gate exists to prevent.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use vaco_io::{CancelToken, MediaSource, MemorySource};
use vaco_opts::Dict;
use vaco_protocol_core::{
    DenyReason, IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags,
    ProtocolRegistry, Result, Url, split_url,
};

/// A protocol that yields its own URL as bytes. Enough to prove dispatch.
#[derive(Debug)]
struct Echo;

impl Protocol for Echo {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        Ok(Box::new(MemorySource::new(url.rest.clone().into_bytes())))
    }
}

/// A protocol that opens the URL it was given, like a playlist does.
#[derive(Debug)]
struct Playlist;

impl Protocol for Playlist {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        // The whole point: a nested open goes back through the registry with
        // the environment we were handed, never a fresh one.
        env.registry.open(&url.rest, flags, opts, env)
    }
}

static ECHO_HTTP: ProtocolDesc = ProtocolDesc {
    name: "http",
    long_name: "test http",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: false,
        server_capable: false,
        readable: true,
        writable: true,
    },
    default_whitelist: &[],
    options: None,
    proto: &Echo,
};

static ECHO_FILE: ProtocolDesc = ProtocolDesc {
    name: "file",
    long_name: "test file",
    flags: ProtocolFlags::LOCAL,
    default_whitelist: &[],
    options: None,
    proto: &Echo,
};

/// Grants exactly what a remote playlist should: http, and not file (W3).
static PLAYLIST: ProtocolDesc = ProtocolDesc {
    name: "playlist",
    long_name: "test playlist",
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: false,
    },
    default_whitelist: &["http"],
    options: None,
    proto: &Playlist,
};

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&ECHO_HTTP);
    r.register(&ECHO_FILE);
    r.register(&PLAYLIST);
    r
}

/// `Box<dyn MediaSource>` is not `Debug`, so `unwrap_err` is unavailable.
fn err_of<T>(r: Result<T>) -> ProtocolError {
    match r {
        Ok(_) => panic!("expected an error, got a source"),
        Err(e) => e,
    }
}

fn denied<T>(r: Result<T>) -> DenyReason {
    match err_of(r) {
        ProtocolError::Denied { reason, .. } => reason,
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn w1_blacklist_beats_whitelist() {
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c)
        .with_whitelist(&["file", "http"])
        .with_blacklist(&["file"]);
    assert_eq!(denied(env.check_scheme("file")), DenyReason::Blacklisted);
    assert!(env.check_scheme("http").is_ok());
}

#[test]
fn w2_unrestricted_means_unrestricted() {
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c);
    assert!(env.check_scheme("file").is_ok());
    assert!(env.check_scheme("anything").is_ok());
}

#[test]
fn w3_a_hostile_playlist_cannot_reach_file() {
    let r = registry();
    let c = CancelToken::new();
    // What the CLI would build for a URL fetched over the network.
    let env = ProtocolEnv::new(&r, &c).with_whitelist(&["playlist", "http", "https"]);

    // The legitimate case still works.
    assert!(
        r.open(
            "playlist:http://cdn/segment0.ts",
            IoFlags::READ,
            &Dict::new(),
            &env
        )
        .is_ok()
    );

    // The attack does not.
    assert_eq!(
        denied(r.open(
            "playlist:file:/etc/passwd",
            IoFlags::READ,
            &Dict::new(),
            &env
        )),
        DenyReason::NotWhitelisted
    );

    // Nor does the bare-path spelling, because a bare path is `file` (U1).
    assert_eq!(
        denied(r.open("playlist:/etc/passwd", IoFlags::READ, &Dict::new(), &env)),
        DenyReason::NotWhitelisted
    );
}

/// An explicit whitelist **replaces** the parent's default grant.
///
/// This test used to assert the opposite — that `playlist`'s default grant of
/// `http` widened a caller's whitelist of `["playlist"]` so the segment fetch
/// succeeded. That was a deliberate design decision and it was wrong.
///
/// Measured against ffmpeg 8.1 while building `vaco-protocol-tls`:
/// `-protocol_whitelist tls` alone is refused the nested `tcp:` open with
/// `Protocol 'tcp' not on whitelist 'tls'!`, even though `tls` grants `tcp` by
/// default. Union semantics made us strictly more permissive than the
/// reference — a caller who narrowed the whitelist got less narrowing than they
/// asked for, in the one mechanism whose whole job is to stop a playlist
/// opening `file:///etc/passwd`.
#[test]
fn w3_an_explicit_whitelist_replaces_the_parent_grant() {
    let r = registry();
    let c = CancelToken::new();

    // `playlist` grants `http` by default, but the caller did not name it.
    let narrow = ProtocolEnv::new(&r, &c).with_whitelist(&["playlist"]);
    assert_eq!(
        denied(r.open(
            "playlist:http://cdn/segment0.ts",
            IoFlags::READ,
            &Dict::new(),
            &narrow
        )),
        DenyReason::NotWhitelisted
    );

    // Naming it works, which is the point: the caller decides.
    let named = ProtocolEnv::new(&r, &c).with_whitelist(&["playlist", "http"]);
    assert!(
        r.open(
            "playlist:http://cdn/segment0.ts",
            IoFlags::READ,
            &Dict::new(),
            &named
        )
        .is_ok()
    );

    // And with no whitelist at all, nothing is restricted — see the note in
    // `env.rs` on why the default grant is not (yet) made to restrict here.
    let open = ProtocolEnv::new(&r, &c);
    assert!(
        r.open(
            "playlist:http://cdn/segment0.ts",
            IoFlags::READ,
            &Dict::new(),
            &open
        )
        .is_ok()
    );
}

#[test]
fn w4_depth_is_bounded() {
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c).with_recursion_limit(3);
    // playlist -> playlist -> playlist -> http is four opens.
    assert_eq!(
        denied(r.open(
            "playlist:playlist:playlist:http://x/y",
            IoFlags::READ,
            &Dict::new(),
            &env,
        )),
        DenyReason::TooDeep
    );

    // One level shallower is fine.
    assert!(
        r.open(
            "playlist:playlist:http://x/y",
            IoFlags::READ,
            &Dict::new(),
            &env
        )
        .is_ok()
    );
}

#[test]
fn u1_a_bare_path_dispatches_to_file() {
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c);
    let (desc, _) = r.resolve(&split_url("clip.mkv"), &env).unwrap();
    assert_eq!(desc.name, "file");
}

#[test]
fn unknown_scheme_is_reported_as_unknown() {
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c);
    let err = err_of(r.open("gopher://x/y", IoFlags::READ, &Dict::new(), &env));
    assert!(matches!(err, ProtocolError::Unknown { .. }), "{err:?}");
}

#[test]
fn a_denied_unknown_scheme_reports_the_denial_not_the_absence() {
    // Error messages must not be a registry oracle.
    let r = registry();
    let c = CancelToken::new();
    let env = ProtocolEnv::new(&r, &c).with_whitelist(&["file"]);
    assert_eq!(
        denied(r.open("gopher://x/y", IoFlags::READ, &Dict::new(), &env)),
        DenyReason::NotWhitelisted
    );
}
