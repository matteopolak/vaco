//! `concat:`/`concatf:` list splitting, including inputs shaped like a
//! slow-unit candidate: thousands of `|` separators or newlines in one input.
//!
//! Both `split_inline_list` and `read_list_file` are single-pass and
//! allocate one `Vec` entry per separator, so this target is mostly here to
//! keep that true — a future edit that made either one quadratic in the
//! separator count would show up here as a timeout rather than only in a
//! benchmark nobody happened to run against pathological input.
//! fuzz-crate: vaco-protocol-wrap
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_wrap::concat::{read_list_file, split_inline_list};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let inline = split_inline_list(s);
    assert_eq!(
        inline.len(),
        s.matches('|').count() + 1,
        "entry count must be exactly one more than the number of separators"
    );
    // `read_list_file` has no invariant this simple (a trailing newline is
    // deliberately not a separate entry — see the module docs), so this only
    // asserts it does not panic and does not somehow produce more entries
    // than there are newlines plus one.
    let lines = read_list_file(s);
    assert!(lines.len() <= s.matches('\n').count() + 1);
});
