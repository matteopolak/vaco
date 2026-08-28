//! Arbitrary `cc_data` bytes against `CcDecoder::feed`: triplet framing,
//! CEA-608 field/channel/control-code parsing and CEA-708 DTVCC packet
//! assembly all run on attacker-controlled bytes with no upstream
//! validation, so this is the target that stands in for a hostile
//! `user_data_registered_itu_t_t35`/A/53 payload once a producer exists
//! (see this crate's top-level doc comment for that gap).
//!
//! Property: `feed` never panics, on any input length including 0, 1 and 2
//! bytes (a truncated final triplet), and every counter in `stats()` only
//! grows by at most one per triplet in the input — the actual defence
//! against unbounded allocation is this crate's fixed-size packet/window
//! buffers (see the crate doc's "Allocation" section), so this checks the
//! bookkeeping around that rather than a cap this fuzzer could usefully
//! exercise on its own.
//!
//! fuzz-crate: vaco-codec-subtitle-cc

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_cc::CcDecoder;

fuzz_target!(|data: &[u8]| {
    let mut dec = CcDecoder::default();
    let max_triplets = data.len().div_ceil(3) as u64;

    let _events = dec.feed(data);

    let stats = dec.stats();
    let total_dropped = stats
        .skipped_triplets
        .saturating_add(stats.parity_errors)
        .saturating_add(stats.dtvcc_desync);
    assert!(
        total_dropped <= max_triplets,
        "dropped {total_dropped} triplets from at most {max_triplets} in {} input bytes",
        data.len()
    );
});
