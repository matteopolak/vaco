#![allow(clippy::expect_used, reason = "test code")]
//! Property tests for the language table's round-trip invariants.

use proptest::prelude::*;
use vaco_format_avlanguage::{parse, table};

/// Every table entry, as a `proptest` strategy yielding an owned copy.
fn any_entry() -> impl Strategy<Value = table::LanguageEntry> {
    prop::sample::select(table::LANGUAGES)
}

proptest! {
    /// A code's own ISO 639-2/T spelling always resolves back to the same
    /// entry, regardless of how its letters are cased.
    #[test]
    fn iso639_2t_round_trips_through_parse_under_any_case(
        entry in any_entry(),
        upper_mask in prop::collection::vec(any::<bool>(), 3),
    ) {
        let code = entry.iso639_2t;
        let mixed: String = code
            .chars()
            .zip(upper_mask.iter().cycle())
            .map(|(c, &up)| if up { c.to_ascii_uppercase() } else { c })
            .collect();
        let resolved = parse(&mixed).expect("a table entry's own code must resolve");
        prop_assert_eq!(*resolved.entry, entry);
    }

    /// Whichever of the two ISO 639-2 spellings a code has, looking it up by
    /// either one lands on the same entry (they are the same language by
    /// construction for every row without a B/T split, and deliberately
    /// paired for the twenty that do).
    #[test]
    fn bibliographic_and_terminology_agree(entry in any_entry()) {
        let by_b = parse(entry.iso639_2b).expect("iso639_2b must resolve");
        let by_t = parse(entry.iso639_2t).expect("iso639_2t must resolve");
        prop_assert_eq!(*by_b.entry, *by_t.entry);
        prop_assert_eq!(*by_b.entry, entry);
    }

    /// A BCP-47 tag built from a table code plus an arbitrary two-letter
    /// region resolves to the same language entry, with the region
    /// upper-cased.
    #[test]
    fn a_bcp47_region_suffix_does_not_change_the_resolved_language(
        entry in any_entry(),
        region in "[a-zA-Z]{2}",
    ) {
        let tag = format!("{}-{}", entry.iso639_2t, region);
        let resolved = parse(&tag).expect("primary subtag must still resolve");
        prop_assert_eq!(*resolved.entry, entry);
        prop_assert_eq!(resolved.region, Some(region.to_ascii_uppercase()));
    }
}
