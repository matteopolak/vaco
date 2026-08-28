//! Three pure, I/O-free parsers of `vaco-protocol-ftp`, none of which touch
//! a network — this crate's own docs explain why: there is no live FTP
//! server reachable here, so a malicious or broken server's responses are
//! exactly what these parsers must survive.
//!
//! 1. `protocol::parse_url` on an arbitrary `ftp:` URL tail.
//! 2. `control::parse_pasv` on arbitrary `227`-response text.
//! 3. `control::parse_epsv` on arbitrary `229`-response text.
//! fuzz-crate: vaco-protocol-ftp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_ftp::{control, protocol};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let _ = protocol::parse_url(s);
    let _ = control::parse_pasv(s);
    let _ = control::parse_epsv(s);
});
