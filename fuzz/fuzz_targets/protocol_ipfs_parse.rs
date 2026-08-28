//! Three pure, I/O-free surfaces of `vaco-protocol-ipfs`, neither of which
//! touches a network or the filesystem — this crate's own docs say why
//! gateway *resolution logic* and target-URL construction are the tested
//! surfaces rather than a live IPFS gateway.
//!
//! 1. [`gateway::resolve`] on four arbitrary candidate strings.
//! 2. [`gateway::build_target`] on an arbitrary gateway/kind/rest triple.
//! 3. [`gateway::ipfs_path_gateway_file`]/[`gateway::home_gateway_file`] on
//!    an arbitrary path fragment.
//! fuzz-crate: vaco-protocol-ipfs
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_ipfs::gateway;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let _ = gateway::resolve(s, Some(s), Some(s), Some(s));
    let _ = gateway::build_target(s, "ipfs", s);
    let _ = gateway::build_target(s, "ipns", s);
    let _ = gateway::ipfs_path_gateway_file(s);
    let _ = gateway::home_gateway_file(s);
});
