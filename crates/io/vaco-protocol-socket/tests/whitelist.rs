//! The whitelist properties this crate's docs claim, checked directly against
//! the registered descriptors rather than only exercised incidentally by the
//! loopback tests.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use vaco_protocol_core::ProtocolRegistry;

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    vaco_protocol_socket::register(&mut r);
    r
}

#[test]
fn every_protocol_here_grants_nothing_and_nests_nothing() {
    let r = registry();
    for name in ["tcp", "udp", "udplite", "unix"] {
        let Some(desc) = r.find(name) else {
            panic!("{name} not registered");
        };
        assert_eq!(
            desc.default_whitelist,
            &[] as &[&str],
            "{name}'s default_whitelist must be empty: it opens no nested URL"
        );
        assert!(
            !desc.flags.nested_scheme,
            "{name} does not open a nested URL, so nested_scheme must be false"
        );
        assert!(desc.flags.network || name == "unix");
    }
}

#[test]
fn unix_registers_on_every_platform() {
    // Even where `AF_UNIX` does not exist, `unix:` is a known scheme that
    // fails at open time rather than an absent one — see `src/unix.rs`'s
    // module docs.
    let r = registry();
    assert!(r.find("unix").is_some());
}

#[test]
fn names_are_registered_case_insensitively_findable() {
    let r = registry();
    assert!(r.find("TCP").is_some());
    assert!(r.find("Udp").is_some());
}
