//! Exp-Golomb decoding against arbitrary bytes.
//!
//! The property that matters most here is **termination**. `ue` counts leading
//! zeros and caps the prefix; without that cap an all-zero buffer is an infinite
//! loop, which is the classic parser hang. Every call is required to either
//! advance the reader or flag it, which is the same progress contract
//! `vaco_limits::ProgressGuard` enforces on components.
//! fuzz-crate: vaco-bitstream
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::{BitReader, GolombRead};
use vaco_limits::ProgressGuard;

#[derive(Arbitrary, Debug)]
enum Op {
    Ue,
    Se,
    UeLong,
    UeGolombK(u8),
    UeMax(u32),
    SeRange(i32, i32),
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    script: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let mut r = BitReader::new(&input.data);
    // A codeword is at least one bit, so a script step that neither consumes a
    // bit nor flags the reader is a hang in the making.
    let mut guard = ProgressGuard::with_max_stalls(4);

    for op in &input.script {
        let before = r.bit_pos();
        let flagged_before = r.overrun();
        match *op {
            Op::Ue => {
                r.ue();
            }
            Op::Se => {
                r.se();
            }
            Op::UeLong => {
                r.ue_long();
            }
            Op::UeGolombK(k) => {
                r.ue_golomb_k(u32::from(k % 17));
            }
            Op::UeMax(max) => {
                if let Ok(v) = r.ue_max(max) {
                    assert!(v <= max, "ue_max returned a value above its ceiling");
                }
            }
            Op::SeRange(lo, hi) => {
                let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                if let Ok(v) = r.se_range(lo, hi) {
                    assert!((lo..=hi).contains(&v), "se_range returned out of range");
                }
            }
        }
        assert!(r.bit_pos() >= before, "the reader moved backwards");
        // The progress contract applies only while the reader is healthy: the
        // *first* read that cannot make sense of the bitstream must flag it.
        // After that a read may consume nothing at all — which is exactly what
        // stops a parser ignoring the flag from spinning on zero-length
        // codewords — so a stall is expected and is not a finding.
        if !flagged_before {
            let progressed = r.bit_pos() > before || r.overrun();
            guard
                .tick(progressed)
                .expect("a read on a healthy reader neither consumed a bit nor flagged it");
        }
    }

    // Once flagged, a reader stays flagged and reports nothing left.
    if r.overrun() {
        assert_eq!(r.bits_left(), 0);
        assert!(r.check().is_err());
    }
});
