//! RBSP escaping and de-escaping.
//!
//! `to_rbsp ∘ to_ebsp` is the identity for every input, and `to_ebsp` output
//! never violates the constraint that makes start-code scanning unambiguous.
//! De-escaping arbitrary bytes must not panic and must not grow the buffer.
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::annexb;

fuzz_target!(|data: &[u8]| {
    // Escaping is invertible.
    let mut ebsp = Vec::new();
    annexb::to_ebsp(data, &mut ebsp);
    assert!(
        !annexb::violates_ebsp_constraint(&ebsp),
        "escaped output still violates the EBSP constraint"
    );
    assert!(ebsp.len() >= data.len());

    let mut scratch = Vec::new();
    assert_eq!(
        annexb::to_rbsp(&ebsp, &mut scratch),
        data,
        "to_rbsp did not invert to_ebsp"
    );

    // De-escaping arbitrary (possibly malformed) bytes is total and shrinking.
    let mut a = Vec::new();
    let unescaped = annexb::to_rbsp(data, &mut a).to_vec();
    assert!(unescaped.len() <= data.len());

    // And idempotent once re-escaped: escape/de-escape reaches a fixed point.
    let mut re = Vec::new();
    annexb::to_ebsp(&unescaped, &mut re);
    let mut b = Vec::new();
    assert_eq!(annexb::to_rbsp(&re, &mut b), &unescaped[..]);

    // The scratch buffer is reused, never grown without bound.
    let before = scratch.capacity();
    for _ in 0..4 {
        annexb::to_rbsp(&ebsp, &mut scratch);
    }
    assert_eq!(scratch.capacity(), before);
});
