//! The coded-bitstream layer against arbitrary bytes, through AV1.
//!
//! `vaco-codec-cbs` is codec-agnostic and has no parser of its own, so it is
//! fuzzed through the codec that implements it — the same shape as
//! `cbs_hevc`. What this target adds is the second framing AV1 has and HEVC
//! does not: [`Av1Framing::LowOverheadBitstream`], Annex B's nested
//! `temporal_unit_size`/`frame_unit_size`/`obu_length` wrapper.
//!
//! Properties:
//!
//! * **A split unit's bytes really came from the input**, at the offset its
//!   origin claims — true for both framings, since neither escapes bytes the
//!   way NAL emulation prevention does.
//! * **`ObuStream` round-trips byte for byte when untouched** — no wrapper,
//!   no escaping, nothing to diverge on. If this ever fails it is a real bug,
//!   not the framing's own ambiguity.
//! * **`LowOverheadBitstream` round-trips *content*, not always *bytes*.**
//!   [`FRAME_UNIT_GRANULARITY_DIVERGENCE`] documents why: a `frame_unit_size`
//!   boundary is the encoder's choice, and this crate always reconstructs one
//!   frame unit per temporal unit on `assemble`. So this target checks the
//!   weaker, honest property — re-splitting the reassembled bytes recovers
//!   the same unit types and payloads — rather than asserting byte equality
//!   and then carrying an exception list the way `cbs_hevc` does for Annex
//!   B's two inexpressible shapes.
//!
//! fuzz-crate: vaco-parse-av1
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_cbs::{Cbs, CbsFragment};
use vaco_limits::{Budget, Limits};
use vaco_parse_av1::cbs::Av1Cbs;
use vaco_parse_av1::obu::Av1Framing;

/// The unit list as `(type, bytes)`, for comparing two fragments.
fn shape(f: &CbsFragment) -> Vec<(u32, Vec<u8>)> {
    f.units()
        .iter()
        .map(|u| (u.unit_type, u.data.clone()))
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut cbs = Cbs::new(Av1Cbs::new());

    for framing in [Av1Framing::ObuStream, Av1Framing::LowOverheadBitstream] {
        let mut fragment = CbsFragment::new();
        if cbs
            .split(data, framing, &mut fragment, &mut budget)
            .is_err()
        {
            fragment.release(&mut budget);
            continue;
        }

        for unit in fragment.units() {
            let o = unit.origin.expect("a split unit always has an origin");
            let end = o.offset.saturating_add(unit.data.len());
            assert!(end <= data.len(), "a unit ran past the end of the input");
            assert_eq!(
                data.get(o.offset..end),
                Some(unit.data.as_slice()),
                "a unit's bytes are not the input's"
            );
        }

        // Every unit the split found is readable without panicking, whatever
        // it decodes to.
        for i in 0..fragment.len() {
            let _ = cbs.read_unit(&fragment, i, &mut budget);
        }

        match framing {
            Av1Framing::ObuStream => {
                let mut out = Vec::new();
                if cbs
                    .assemble(&fragment, framing, &mut out, &mut budget)
                    .is_ok()
                {
                    // Byte-exact only when `data` is itself a complete,
                    // well-formed `ObuStream` (the crate's own
                    // `an_untouched_obu_stream_round_trips_byte_for_byte`
                    // test asserts exactly that, over a real fixture). Over
                    // arbitrary fuzzer input, `split` legitimately drops a
                    // truncated trailing OBU it cannot parse — see
                    // `obu::units`'s own docs — so the property that holds
                    // *unconditionally* is the weaker one `cbs_hevc` asserts:
                    // reassembling an untouched fragment never grows it.
                    assert!(
                        out.len() <= data.len(),
                        "an untouched ObuStream fragment grew from {} to {} bytes",
                        data.len(),
                        out.len()
                    );
                    // And when every byte of `data` fell inside some unit
                    // (nothing was dropped as an unparseable tail), the
                    // round trip must be exact: `ObuStream` units sit
                    // back-to-back with no gap, so that is exactly "the last
                    // unit's origin reaches the end of the buffer".
                    let fully_consumed = fragment.units().last().is_some_and(|u| {
                        u.origin
                            .is_some_and(|o| o.offset + u.data.len() == data.len())
                    });
                    if fully_consumed {
                        assert_eq!(out, data, "a fully-consumed input did not round-trip exactly");
                    }
                }
            }
            Av1Framing::LowOverheadBitstream => {
                let mut out = Vec::new();
                if cbs
                    .assemble(&fragment, framing, &mut out, &mut budget)
                    .is_ok()
                {
                    let mut back = CbsFragment::new();
                    if cbs.split(&out, framing, &mut back, &mut budget).is_ok() {
                        assert_eq!(
                            shape(&back),
                            shape(&fragment),
                            "re-assembling changed which units or bytes survive, not just the wrapper"
                        );
                    }
                    back.release(&mut budget);
                }
            }
        }

        fragment.release(&mut budget);
    }
});
