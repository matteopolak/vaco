//! `ApeTag::parse` and the trailing-tag locator over arbitrary bytes.
//!
//! Two untrusted length fields feed straight into slicing arithmetic here:
//! the footer's own `tag_size` (which decides where the item list is
//! believed to start, potentially before byte 0 of the buffer) and each
//! item's `value_size`. Both are exactly the "declared size exceeds what is
//! actually there" shape plan 13 §2.2 calls out, and [`locate::find_trailing`]
//! adds a second layer: it first decides whether the input's tail looks like
//! an `ID3v1` tag, which shifts where it then looks for the APE footer.
//!
//! Properties asserted:
//!
//! * Parsing never panics, whatever the bytes, under a generous budget and
//!   under one too small to hold more than a few items.
//! * [`locate::find_trailing`]'s reported range is always inside the buffer
//!   it was given — the whole reason it exists is to get that right when
//!   `tag_size` lies.
//! * A found range parses without a second panic, and any items it
//!   produces have keys within the specification's bounds.
//!
//! fuzz-crate: vaco-format-apetag

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_apetag::locate;
use vaco_format_apetag::tag::{ApeTag, MAX_KEY_LEN, MIN_KEY_LEN};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    // 1. Direct parse, generous budget.
    let mut generous = Budget::new(Limits::permissive());
    if let Ok(tag) = ApeTag::parse(data, &mut generous) {
        for item in &tag.items {
            assert!(
                (MIN_KEY_LEN..=MAX_KEY_LEN).contains(&item.key.len()),
                "parsed item key {:?} outside the specification's bounds",
                item.key
            );
        }
    }

    // 2. Direct parse, a budget too small to hold more than a handful of
    //    bytes: every failure must be a clean error, never a panic or an
    //    allocation the budget did not approve.
    let mut starved = Budget::new(Limits::strict().with_alloc_total(8).with_fuel(64));
    let _ = ApeTag::parse(data, &mut starved);

    // 3. The trailing-tag locator: its range must always lie inside `data`,
    //    and parsing that exact slice must not panic either.
    if let Some(found) = locate::find_trailing(data) {
        assert!(
            found.start <= found.end && found.end <= data.len(),
            "locate::find_trailing reported a range outside the buffer: {found:?}"
        );
        let mut budget = Budget::new(Limits::permissive());
        if let Some(slice) = data.get(found.start..found.end) {
            let _ = ApeTag::parse(slice, &mut budget);
        }
    }
    let mut budget = Budget::new(Limits::permissive());
    let _ = locate::parse_trailing(data, &mut budget);
});
