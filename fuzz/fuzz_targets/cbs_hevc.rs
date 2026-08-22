//! The coded-bitstream layer against arbitrary bytes, through HEVC.
//!
//! `vaco-codec-cbs` is codec-agnostic and has no parser of its own, so it is
//! fuzzed through the codec that implements it. What this target exercises that
//! `parse_hevc` does not is the **write** side: splitting, editing the unit
//! list, and re-assembling — in the framing it came in and in the other one.
//!
//! Three properties, all of which a bitstream filter's correctness rests on:
//!
//! * **A split unit's bytes really came from the input**, at the offset its
//!   origin claims. A filter that reports the wrong origin attaches timestamps
//!   to the wrong bytes.
//! * **An untouched fragment never grows.** Re-assembling in the framing it was
//!   read in may *shrink* the buffer — leading garbage before the first start
//!   code belongs to no unit and is dropped — but a layer that inflates a stream
//!   it was asked to leave alone is rewriting framing it should not touch.
//! * **Reframing is lossless for every conforming unit.** Annex B to
//!   length-prefixed and back must give the same unit list, byte for byte, or
//!   `hevc_mp4toannexb` corrupts a stream.
//!
//!   The exceptions are the two shapes Annex B cannot express, both of which
//!   §7.4.1.1 makes impossible in a conforming stream: a unit whose bytes end in
//!   `0x00` (indistinguishable from §B.1's `trailing_zero_8bits`) and a unit
//!   containing `00 00 01` (which becomes a start code). Both were found by this
//!   target; `vaco_parse_hevc::cbs::annexb_safe` tests for them and
//!   `ANNEXB_EXPRESSIVENESS_DIVERGENCE` pins them.
//!
//! fuzz-crate: vaco-parse-hevc
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_cbs::{Cbs, CbsFragment, CbsUnit};
use vaco_format_nalu::{Framing, LengthSize};
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::{HevcCbs, HevcContent};

/// The unit list as `(type, bytes)`, for comparing two fragments.
fn shape(f: &CbsFragment) -> Vec<(u32, Vec<u8>)> {
    f.units()
        .iter()
        .map(|u| (u.unit_type, u.data.clone()))
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut cbs = Cbs::new(HevcCbs::new());

    for framing in [
        Framing::AnnexB,
        Framing::LengthPrefixed(LengthSize::ONE),
        Framing::LengthPrefixed(LengthSize::TWO),
        Framing::LengthPrefixed(LengthSize::FOUR),
    ] {
        let mut fragment = CbsFragment::new();
        if cbs
            .split(data, framing, &mut fragment, &mut budget)
            .is_err()
        {
            fragment.release(&mut budget);
            continue;
        }

        // Every unit's bytes are exactly the input's, at the offset it claims.
        for unit in fragment.units() {
            let o = unit.origin.expect("a split unit always has an origin");
            let end = o.offset.saturating_add(unit.data.len());
            assert!(end <= data.len(), "a unit ran past the end of the input");
            assert_eq!(
                data.get(o.offset..end),
                Some(unit.data.as_slice()),
                "a unit's bytes are not the input's"
            );
            assert!(unit.data.len() >= 2, "a unit without a header was kept");
        }

        // Re-assembling unchanged never grows the buffer.
        let mut same = Vec::new();
        if cbs
            .assemble(&fragment, framing, &mut same, &mut budget)
            .is_ok()
        {
            assert!(
                same.len() <= data.len(),
                "an untouched fragment grew from {} to {} bytes",
                data.len(),
                same.len()
            );
        }

        // Reframing round trip: to the other framing and back.
        let other = match framing {
            Framing::AnnexB => Framing::LengthPrefixed(LengthSize::FOUR),
            _ => Framing::AnnexB,
        };
        let mut reframed = Vec::new();
        if cbs
            .assemble(&fragment, other, &mut reframed, &mut budget)
            .is_ok()
        {
            let mut back = CbsFragment::new();
            if cbs
                .split(&reframed, other, &mut back, &mut budget)
                .is_ok()
                // A non-conforming unit cannot survive Annex B; see the module
                // docs and `annexb_safe`.
                && fragment
                    .units()
                    .iter()
                    .all(|u| vaco_parse_hevc::cbs::annexb_safe(&u.data))
            {
                assert_eq!(
                    shape(&back),
                    shape(&fragment),
                    "reframing changed the unit list"
                );
            }
            back.release(&mut budget);
        }

        // Every unit decodes, or says why not, without panicking; and a raw
        // unit written back is byte-identical.
        for i in 0..fragment.len() {
            if let Ok(content) = cbs.read_unit(&fragment, i, &mut budget) {
                let before = fragment.units().get(i).map(|u| u.data.clone());
                let is_raw = matches!(content, HevcContent::Raw { .. });
                let updated = cbs
                    .update_unit(&mut fragment, i, &content, &mut budget)
                    .is_ok();
                if is_raw && updated {
                    assert_eq!(
                        fragment.units().get(i).map(|u| u.data.clone()),
                        before,
                        "rewriting a raw unit changed its bytes"
                    );
                }
            }
        }

        // Editing operations must not panic on any index, in range or not.
        let n = fragment.len();
        fragment.retain(|u| u.unit_type != 39);
        let _ = fragment.position_of(33);
        let _ = fragment.units_of_type(34).count();
        let _ = fragment.remove(n.wrapping_add(1));
        let _ = fragment.insert(
            n.wrapping_mul(3),
            CbsUnit::new(35, vec![0x46, 0x01, 0x50]),
            &mut budget,
        );
        let mut out = Vec::new();
        let _ = cbs.assemble(&fragment, Framing::AnnexB, &mut out, &mut budget);

        fragment.release(&mut budget);
    }
});
