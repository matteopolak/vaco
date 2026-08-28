//! Arbitrary bytes against MPEG-2's A/53 caption `user_data()` scan.
//!
//! This is the one of the three new A/53 extraction paths that does its own
//! *scanning* rather than being handed an already-delimited payload: it walks
//! start codes across a whole buffer, so a malformed stream controls both how
//! many elements it finds and where each one ends. The H.264 and HEVC paths
//! receive an SEI payload whose bounds their own already-fuzzed SEI parsers
//! established, so this target covers the genuinely new attack surface.
//!
//! Properties, beyond "does not panic":
//!
//! - Every yielded slice is a whole number of three-byte triplets and never
//!   exceeds the 5-bit `cc_count` bound of 93 bytes. That bound is what makes
//!   the module allocation-free, so a regression in it is the bug worth
//!   catching.
//! - The iterator terminates. Its position must advance past every start code
//!   it examines; a scan that failed to advance would hang rather than crash,
//!   which no panic-based check would notice, so the element count is bounded
//!   against the input length explicitly.
//!
//! fuzz-crate: vaco-parse-mpegvideo

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_parse_mpegvideo::a53;

fuzz_target!(|data: &[u8]| {
    // A `user_data()` element needs at least a 4-byte start code, so no input
    // can contain more elements than a quarter of its length.
    let max_elements = data.len() / 4 + 1;

    let mut seen = 0usize;
    for cc in a53::iter_cc_data(data) {
        seen += 1;
        assert!(
            seen <= max_elements,
            "scan yielded {seen} elements from {} bytes — it is not advancing",
            data.len()
        );
        assert!(
            cc.len() % 3 == 0,
            "cc_data slice of {} bytes is not whole triplets",
            cc.len()
        );
        assert!(
            cc.len() <= a53::MAX_CC_DATA_BYTES,
            "cc_data slice of {} bytes exceeds the 5-bit cc_count bound",
            cc.len()
        );
    }

    // `find_cc_data` must agree with the iterator's first element.
    assert_eq!(a53::find_cc_data(data), a53::iter_cc_data(data).next());

    // The two lower-level entry points take attacker bytes directly.
    if let Some(cc) = a53::cc_data_after_identifier(data) {
        assert!(cc.len() % 3 == 0 && cc.len() <= a53::MAX_CC_DATA_BYTES);
    }
    if let Some(cc) = a53::cc_data_triplets(data) {
        assert!(cc.len() % 3 == 0 && cc.len() <= a53::MAX_CC_DATA_BYTES);
    }
});
