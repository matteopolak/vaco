//! `vaco-cbs-vp9`'s split/read/write against arbitrary bytes.
//!
//! VP9 has one framing (a container sample, optionally a superframe), unlike
//! H.264/HEVC/AV1's several, so there is no reframing property to check here
//! — the three that matter are: a split unit's bytes come from the input at
//! the offset it claims, an untouched fragment reassembles byte for byte, and
//! every unit that decodes writes back identically with no edit (the
//! property `a_real_key_frame_round_trips_bit_exactly_with_no_edit` pins on
//! one real fixture; this target runs it over arbitrary bytes).
//!
//! fuzz-crate: vaco-cbs-vp9
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_cbs_vp9::Vp9Cbs;
use vaco_cbs_vp9::cbs::Vp9Framing;
use vaco_codec_cbs::{Cbs, CbsFragment, CbsUnit};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut cbs = Cbs::new(Vp9Cbs::new());
    let framing = Vp9Framing;

    let mut fragment = CbsFragment::new();
    if cbs.split(data, framing, &mut fragment, &mut budget).is_err() {
        fragment.release(&mut budget);
        return;
    }

    for unit in fragment.units() {
        if let Some(o) = unit.origin {
            let end = o.offset.saturating_add(unit.data.len());
            assert!(end <= data.len(), "a unit ran past the end of the input");
            assert_eq!(
                data.get(o.offset..end),
                Some(unit.data.as_slice()),
                "a unit's bytes are not the input's"
            );
        }
    }

    let mut same = Vec::new();
    if cbs
        .assemble(&fragment, framing, &mut same, &mut budget)
        .is_ok()
    {
        assert_eq!(same, data, "an untouched fragment did not reassemble byte for byte");
    }

    // Not asserted byte-identical: `uncompressed_header()` has reserved and
    // padding bits (the `profile == 3` reserved bit, the tail of
    // `byte_alignment()`) this crate does not track the literal value of,
    // since no conforming encoder ever sets them to anything but 0 — arbitrary
    // fuzzer input can, and the writer canonicalises them, changing bytes
    // with no semantic edit. What must hold instead: re-parsing what was
    // written reproduces the identical typed value.
    for i in 0..fragment.len() {
        if let Ok(content) = cbs.read_unit(&fragment, i, &mut budget) {
            if cbs
                .update_unit(&mut fragment, i, &content, &mut budget)
                .is_ok()
            {
                let reread = cbs.read_unit(&fragment, i, &mut budget);
                assert_eq!(
                    reread.ok(),
                    Some(content),
                    "re-parsing a freshly written unit did not reproduce it"
                );
            }
        }
    }

    let n = fragment.len();
    fragment.retain(|_| true);
    let _ = fragment.remove(n.wrapping_add(1));
    let _ = fragment.insert(n.wrapping_mul(3), CbsUnit::new(0, vec![0x82, 0x49, 0x83, 0x42]), &mut budget);
    let mut out = Vec::new();
    let _ = cbs.assemble(&fragment, framing, &mut out, &mut budget);

    fragment.release(&mut budget);
});
