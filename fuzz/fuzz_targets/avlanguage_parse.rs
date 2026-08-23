//! `parse` and the `to_639_*` conversions over arbitrary strings.
//!
//! No binary layout and no allocation-from-a-length-field here — the risk
//! this crate actually carries is in its string handling: byte-vs-character
//! boundary slicing in [`vaco_format_avlanguage::table::is_private_use`] and
//! the BCP-47 splitter, both of which run over attacker-supplied text
//! (a `-metadata language=` value, or a container's language field decoded
//! to a `String` upstream) that is not guaranteed to be ASCII.
//!
//! Properties asserted: parsing and every conversion function never panics,
//! whatever the string (multi-byte UTF-8, empty, absurdly long, embedded
//! NUL); a resolved entry's own code always resolves back to an equal entry
//! (the round trip every table lookup promises).
//!
//! fuzz-crate: vaco-format-avlanguage

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_avlanguage::{parse, table, to_639_1, to_639_2b, to_639_2t};

fuzz_target!(|input: &str| {
    let _ = table::is_private_use(input);
    let _ = to_639_1(input);
    let _ = to_639_2b(input);
    let _ = to_639_2t(input);

    if let Some(lang) = parse(input) {
        // Round trip: the entry's own terminology code must resolve back to
        // an equal entry.
        let again = parse(lang.entry.iso639_2t).expect("a table entry's own code must resolve");
        assert_eq!(*again.entry, *lang.entry);
    }
});
