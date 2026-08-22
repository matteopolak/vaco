//! Annex-B framing against arbitrary bytes.
//!
//! Properties: the iterator terminates, every unit is a non-empty in-order
//! disjoint sub-slice of the input, no unit contains a start code, and the
//! word-skip scanner agrees exactly with a naive three-byte window scan.
//! fuzz-crate: vaco-bitstream
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::annexb;

/// The definition of a start code, scanned the obvious way.
fn naive_find(buf: &[u8], from: usize) -> Option<usize> {
    (from..buf.len().saturating_sub(2)).find(|&i| buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)
}

fuzz_target!(|data: &[u8]| {
    // The optimised scanner must agree with the definition at every offset.
    let mut from = 0usize;
    loop {
        let fast = annexb::find_start_code(data, from);
        let slow = naive_find(data, from);
        assert_eq!(fast, slow, "scanner disagrees with the definition at {from}");
        match fast {
            Some(i) => from = i + 1,
            None => break,
        }
    }

    let mut last_end = 0usize;
    let mut count = 0usize;
    let base = data.as_ptr() as usize;
    for unit in annexb::nal_units(data) {
        count += 1;
        assert!(count <= data.len() + 1, "nal_units did not terminate");
        assert!(!unit.is_empty(), "an empty unit was yielded");
        let offset = unit.as_ptr() as usize - base;
        assert!(offset >= last_end, "units overlap or go backwards");
        last_end = offset + unit.len();
        assert!(last_end <= data.len(), "a unit escaped the input");
        assert_eq!(
            annexb::find_start_code(unit, 0),
            None,
            "a unit contains a start code"
        );
        // A unit never ends in a zero byte: trailing_zero_8bits are trimmed.
        assert_ne!(unit.last(), Some(&0));
    }
});
