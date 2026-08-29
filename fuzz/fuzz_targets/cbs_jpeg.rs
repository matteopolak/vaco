//! `vaco-cbs-jpeg`'s split/read/write against arbitrary bytes.
//!
//! JPEG has one framing and every segment is byte-aligned and self-
//! delimiting, so the properties that matter are the same three
//! `cbs_vp9` checks: split-unit provenance, byte-exact reassembly of an
//! untouched fragment, and an unedited unit writing back identically.
//!
//! fuzz-crate: vaco-cbs-jpeg
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_cbs_jpeg::JpegCbs;
use vaco_cbs_jpeg::cbs::JpegFraming;
use vaco_codec_cbs::{Cbs, CbsFragment, CbsUnit};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut cbs = Cbs::new(JpegCbs::new());
    let framing = JpegFraming;

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

    for i in 0..fragment.len() {
        if let Ok(content) = cbs.read_unit(&fragment, i, &mut budget) {
            let before = fragment.units().get(i).map(|u| u.data.clone());
            if cbs
                .update_unit(&mut fragment, i, &content, &mut budget)
                .is_ok()
            {
                assert_eq!(
                    fragment.units().get(i).map(|u| u.data.clone()),
                    before,
                    "rewriting an unedited unit changed its bytes"
                );
            }
        }
    }

    let n = fragment.len();
    fragment.retain(|_| true);
    let _ = fragment.remove(n.wrapping_add(1));
    let _ = fragment.insert(n.wrapping_mul(3), CbsUnit::new(0xD8, vec![0xFF, 0xD8]), &mut budget);
    let mut out = Vec::new();
    let _ = cbs.assemble(&fragment, framing, &mut out, &mut budget);

    fragment.release(&mut budget);
});
