#![allow(clippy::expect_used, reason = "test code")]
//! Property test: an arbitrary well-formed APE tag round-trips through
//! `to_bytes`/`parse` byte-for-byte in its recovered content.

use proptest::prelude::*;
use vaco_format_apetag::tag::{ApeItem, ApeTag};
use vaco_limits::{Budget, Limits};

/// A valid item key: 2-255 printable ASCII bytes excluding `=`.
fn any_key() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[ -<>-~]{2,16}").expect("valid regex")
}

/// A value with no embedded NUL, so it round-trips through the multi-value
/// splitter as a single value.
fn any_value() -> impl Strategy<Value = String> {
    "[ -~]{0,32}"
}

fn any_item() -> impl Strategy<Value = ApeItem> {
    (any_key(), any_value()).prop_map(|(k, v)| ApeItem::text(k, v))
}

proptest! {
    #[test]
    fn a_footer_only_tag_round_trips(items in prop::collection::vec(any_item(), 0..8)) {
        let tag = ApeTag { version: 2000, items };
        let bytes = tag.to_bytes().expect("valid items must serialise");
        let mut budget = Budget::new(Limits::permissive());
        let parsed = ApeTag::parse(&bytes, &mut budget).expect("a tag we just wrote must parse");
        prop_assert_eq!(parsed.items.len(), tag.items.len());
        for (a, b) in tag.items.iter().zip(parsed.items.iter()) {
            prop_assert_eq!(&a.key, &b.key);
            prop_assert_eq!(&a.value, &b.value);
        }
    }

    #[test]
    fn a_header_plus_footer_tag_round_trips(items in prop::collection::vec(any_item(), 0..8)) {
        let tag = ApeTag { version: 2000, items };
        let bytes = tag.to_bytes_with_header().expect("valid items must serialise");
        let mut budget = Budget::new(Limits::permissive());
        let parsed = ApeTag::parse(&bytes, &mut budget).expect("a tag we just wrote must parse");
        prop_assert_eq!(parsed.items.len(), tag.items.len());
    }
}
