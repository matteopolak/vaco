//! NAL framing, de-escaping and framing conversion against arbitrary bytes.
//!
//! Properties: every iterator terminates and yields non-empty, in-order,
//! disjoint sub-slices; this crate's framing agrees exactly with the definition
//! in `vaco-bitstream`; `RbspBuf` agrees with `to_rbsp` byte for byte and always
//! produces a valid `Padded`; escaping round-trips; framing conversion preserves
//! the unit sequence; the incremental `Scanner` finds exactly the boundaries
//! a whole-buffer scan finds however the input is chopped up; and
//! `extradata::assemble_extradata` (CONFORMANCE-FINDINGS 26) round-trips
//! through `units` — what it assembles, `units` parses back into exactly the
//! same NAL units in the same order.
//!
//! A `LimitExceeded` is correct behaviour and returns normally; a panic, a hang
//! or a disagreement is a bug (plan 13 §2.2.4).
//! fuzz-crate: vaco-format-nalu
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::{annexb, avcc};
use vaco_format_nalu::{
    Framing, HeaderKind, LengthSize, RbspBuf, Scanner, annexb_to_length_prefixed,
    assemble_extradata, escape_into, length_prefixed_to_annexb, parameter_sets, units,
    violates_ebsp_constraint,
};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    // ---- Annex B framing agrees with layer 0, and locates what it yields.
    let mine: Vec<&[u8]> = units(data, Framing::AnnexB).map(|n| n.data).collect();
    let theirs: Vec<&[u8]> = annexb::nal_units(data).collect();
    assert_eq!(mine, theirs, "annex-b framing disagrees with vaco-bitstream");

    let mut last_end = 0usize;
    for nal in units(data, Framing::AnnexB) {
        assert!(!nal.data.is_empty(), "empty unit yielded");
        assert!(nal.offset >= last_end, "units out of order or overlapping");
        assert!(nal.end() <= data.len(), "unit runs past the buffer");
        assert_eq!(
            &data[nal.offset..nal.end()],
            nal.data,
            "offset does not locate the unit"
        );
        assert!(nal.start_code_len == 3 || nal.start_code_len == 4);
        assert!(nal.offset >= nal.start_code_len as usize);
        last_end = nal.end();
    }

    // ---- Length-prefixed framing agrees with layer 0 at every legal width.
    for size in [LengthSize::ONE, LengthSize::TWO, LengthSize::FOUR] {
        let mine: Vec<&[u8]> = units(data, Framing::LengthPrefixed(size))
            .map(|n| n.data)
            .collect();
        let theirs: Vec<&[u8]> = avcc::nal_units(data, size.get()).collect();
        assert_eq!(mine, theirs, "length-prefixed framing disagrees at {size:?}");
        for nal in units(data, Framing::LengthPrefixed(size)) {
            assert_eq!(&data[nal.offset..nal.end()], nal.data);
            assert_eq!(nal.start_code_len, 0);
        }
    }

    // ---- De-escaping agrees with layer 0 and always yields a valid `Padded`.
    let mut budget = Budget::new(Limits::strict());
    let mut rbsp = RbspBuf::new();
    if rbsp.fill(data, &mut budget).is_ok() {
        let mut scratch = Vec::new();
        assert_eq!(
            rbsp.as_slice(),
            annexb::to_rbsp(data, &mut scratch),
            "rbsp disagrees with vaco-bitstream"
        );
        assert!(rbsp.len() <= data.len(), "de-escaping grew the buffer");
        let padded = rbsp.padded().expect("fill must establish the padding");
        assert_eq!(padded.logical_len(), rbsp.len());
        assert!(padded.as_bytes()[rbsp.len()..].iter().all(|&b| b == 0));
        // The reader must be constructible and readable without panicking.
        let mut r = rbsp.reader();
        let _ = r.get(8);
        let _ = r.check();
    }

    // ---- Escaping round-trips and produces a scannable stream.
    let mut ebsp = Vec::new();
    if escape_into(data, &mut ebsp, &mut budget).is_ok() {
        assert!(!violates_ebsp_constraint(&ebsp));
        let mut back = RbspBuf::new();
        if back.fill(&ebsp, &mut budget).is_ok() {
            assert_eq!(back.as_slice(), data, "escape round-trip lost data");
        }
    }

    // ---- Framing conversion preserves the unit sequence.
    let mut budget = Budget::new(Limits::permissive());
    let before: Vec<Vec<u8>> = units(data, Framing::AnnexB).map(|n| n.data.to_vec()).collect();
    let mut lp = Vec::new();
    if annexb_to_length_prefixed(data, LengthSize::FOUR, &mut lp, &mut budget).is_ok() {
        let mut back = Vec::new();
        if length_prefixed_to_annexb(&lp, LengthSize::FOUR, &mut back, &mut budget).is_ok() {
            let after: Vec<Vec<u8>> = units(&back, Framing::AnnexB)
                .map(|n| n.data.to_vec())
                .collect();
            assert_eq!(before, after, "framing conversion lost or changed a unit");
        }
    }

    // ---- The incremental scanner agrees with the whole-buffer one, whatever
    // the chunking. This is the property the chunked-parser bug violates.
    let step = 1 + (data.first().copied().unwrap_or(1) as usize % 7);
    let mut scanner = Scanner::new();
    let mut from = 0usize;
    let mut incremental = Vec::new();
    let mut sizes: Vec<usize> = (0..=data.len()).step_by(step).collect();
    if sizes.last() != Some(&data.len()) {
        sizes.push(data.len());
    }
    for n in sizes {
        while let Some(sc) = scanner.find(&data[..n], from) {
            incremental.push(sc.offset);
            from = sc.payload_offset();
        }
    }
    let mut reference = Vec::new();
    let mut i = 0usize;
    while let Some(sc) = annexb::find_start_code(data, i) {
        let four = sc >= 1 && data[sc - 1] == 0;
        reference.push(if four { sc - 1 } else { sc });
        i = sc + 3;
    }
    assert_eq!(
        incremental, reference,
        "incremental scanning found different boundaries than a whole-buffer scan"
    );

    // ---- extradata assembly (CONFORMANCE-FINDINGS 26) round-trips through
    // `units`: whatever `parameter_sets` collected out of arbitrary bytes,
    // `assemble_extradata` lays out with a valid start-code convention, and
    // re-scanning the result finds exactly those units again, in order.
    // `units()` already trims trailing zero bits from a found unit, so a
    // collected unit never ends in a byte that could fuse with the next
    // delimiter into a different boundary — which is what makes the
    // round-trip exact rather than approximate.
    for kind in [HeaderKind::H264, HeaderKind::H265] {
        let sets = parameter_sets(data, Framing::AnnexB, kind);
        let assembled = assemble_extradata(sets.iter().copied());
        if sets.is_empty() {
            assert!(assembled.is_empty());
            continue;
        }
        assert_eq!(&assembled[..3], &[0, 0, 1], "first start code must be 3 bytes");
        let expected_len: usize =
            sets.iter().map(|u| u.len()).sum::<usize>() + 3 + 4 * (sets.len() - 1);
        assert_eq!(assembled.len(), expected_len, "assembled length disagrees with the unit sizes");

        let recovered: Vec<&[u8]> = units(&assembled, Framing::AnnexB).map(|n| n.data).collect();
        assert_eq!(recovered, sets, "assembled extradata does not round-trip through units()");
    }
});
