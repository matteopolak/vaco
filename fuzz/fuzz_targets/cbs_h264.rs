//! The coded-bitstream layer against arbitrary bytes, through H.264.
//!
//! Mirrors `cbs_hevc`'s shape — see that target's own doc for why each
//! property matters. What this target adds is the **write** path `cbs_hevc`
//! does not have for its own codec: `H264Cbs::write_unit` re-encodes a typed
//! `Sps`/`Pps` bit-exactly (see `vaco-parse-h264::cbs`'s module doc), so
//! `sps_and_pps_round_trip_bit_exactly_with_no_edit`-shaped behaviour is
//! checked here over arbitrary bytes, not just the crate's own fixtures.
//!
//! fuzz-crate: vaco-parse-h264
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_cbs::{Cbs, CbsFragment, CbsUnit};
use vaco_format_nalu::{Framing, LengthSize};
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264Cbs, H264Content};

fn shape(f: &CbsFragment) -> Vec<(u32, Vec<u8>)> {
    f.units()
        .iter()
        .map(|u| (u.unit_type, u.data.clone()))
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut cbs = Cbs::new(H264Cbs::new());

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
                && fragment
                    .units()
                    .iter()
                    .all(|u| u.data.last() != Some(&0) && !vaco_codec_cbs::violates_ebsp_constraint(&u.data))
            {
                assert_eq!(
                    shape(&back),
                    shape(&fragment),
                    "reframing changed the unit list"
                );
            }
            back.release(&mut budget);
        }

        // Every unit decodes, or says why not, without panicking, and a raw
        // unit written back is byte-identical. A typed Sps/Pps is *not*
        // asserted byte-identical here on purpose: `BitReader::check` only
        // confirms no overrun, not that the whole unit was consumed (see
        // `vaco-bitstream`'s own doc on `check`), so arbitrary bytes can
        // parse into a valid Sps/Pps that legitimately consumes fewer bytes
        // than `unit.data` holds — trailing bytes past
        // `rbsp_trailing_bits()` that a real, conforming encoder's framing
        // would never produce. What must hold instead is the write path's
        // real contract: re-parsing what it wrote reproduces the identical
        // typed value.
        for i in 0..fragment.len() {
            if let Ok(content) = cbs.read_unit(&fragment, i, &mut budget) {
                let before = fragment.units().get(i).map(|u| u.data.clone());
                let is_raw = matches!(content, H264Content::Raw { .. });
                let updated = cbs
                    .update_unit(&mut fragment, i, &content, &mut budget)
                    .is_ok();
                if is_raw && updated {
                    assert_eq!(
                        fragment.units().get(i).map(|u| u.data.clone()),
                        before,
                        "rewriting a raw unit changed its bytes"
                    );
                } else if updated {
                    let reread = cbs.read_unit(&fragment, i, &mut budget);
                    assert_eq!(
                        reread.ok(),
                        Some(content),
                        "re-parsing a freshly written typed unit did not reproduce it"
                    );
                }
            }
        }

        let n = fragment.len();
        fragment.retain(|u| u.unit_type != 6);
        let _ = fragment.position_of(7);
        let _ = fragment.units_of_type(8).count();
        let _ = fragment.remove(n.wrapping_add(1));
        let _ = fragment.insert(
            n.wrapping_mul(3),
            CbsUnit::new(7, vec![0x67, 0x42]),
            &mut budget,
        );
        let mut out = Vec::new();
        let _ = cbs.assemble(&fragment, Framing::AnnexB, &mut out, &mut budget);

        fragment.release(&mut budget);
    }
});
