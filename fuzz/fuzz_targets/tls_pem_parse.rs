//! `vaco_protocol_tls::pem::extract_der_blocks` parses a PEM file's text — in
//! principle local-only input (`-ca_file`), but a hand-rolled parser is worth
//! fuzzing regardless of provenance (AGENT-CONSTRAINTS: "a crate that parses
//! untrusted input and has no fuzz target is not done"), and this one does
//! its own base64 decoding rather than delegating to a reviewed crate.
//! fuzz-crate: vaco-protocol-tls

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_tls::pem::extract_der_blocks;

fuzz_target!(|data: &str| {
    // Total: never panics, whatever label is asked for, and calling it twice
    // with the same input is deterministic (no hidden state carried between
    // calls despite the module-level `ALPHABET` table being shared).
    let a = extract_der_blocks(data, "CERTIFICATE");
    let b = extract_der_blocks(data, "CERTIFICATE");
    match (a, b) {
        (Ok(x), Ok(y)) => assert_eq!(x, y, "extract_der_blocks must be deterministic"),
        (Err(_), Err(_)) => {}
        _ => panic!("extract_der_blocks gave a different Ok/Err verdict on the same input"),
    }
    let _ = extract_der_blocks(data, "PRIVATE KEY");
});
