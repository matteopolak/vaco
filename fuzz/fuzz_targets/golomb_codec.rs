//! `vaco-codec-golomb`: the codec-level Exp-Golomb layer against arbitrary bytes.
//!
//! `golomb_arbitrary` already covers `vaco-bitstream`'s two codes. This target
//! covers what this crate adds, and asserts three things that target cannot:
//!
//! 1. **Termination.** Every read either advances the reader or flags it. A read
//!    that does neither is an infinite loop with extra steps, and an all-zero
//!    buffer is the input that finds it.
//! 2. **Agreement.** This crate's `ue_v` is a different shape from
//!    `vaco_bitstream::GolombRead::ue`. Two implementations that agree on valid
//!    input and diverge on malformed input are a differential bug waiting for
//!    someone to notice; the assertion here is over *arbitrary* bytes, so the
//!    malformed case is the case being tested.
//! 3. **Bounded reads report rather than run away.** `BoundedGolomb` with
//!    `Limits::tiny` must always come back — with an error is fine, with a hang
//!    is not.
//! fuzz-crate: vaco-codec-golomb
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::{BitReader, BitWriter, GolombRead};
use vaco_codec_golomb::{
    BoundedGolomb, ChromaArrayType, GolombDecode, GolombEncode, MbPartPredMode, map,
};
use vaco_limits::{Budget, Limits};

#[derive(Arbitrary, Debug)]
enum Op {
    Ue,
    Se,
    Te(u32),
    Me(bool, bool),
    UeK(u8),
    SeK(u8),
    Ue64,
    UeMax(u32),
    SeRange(i32, i32),
    TeChecked(u32),
    UeKMax(u8, u32),
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    script: Vec<Op>,
    /// Values fed back through the writer, to close the round trip.
    encode: Vec<u32>,
}

fn chroma(b: bool) -> ChromaArrayType {
    if b {
        ChromaArrayType::WithChroma
    } else {
        ChromaArrayType::Monochrome
    }
}

fn pred(b: bool) -> MbPartPredMode {
    if b {
        MbPartPredMode::Inter
    } else {
        MbPartPredMode::Intra
    }
}

fuzz_target!(|input: Input| {
    // ---- 1. every read makes progress or flags -----------------------------
    let mut r = BitReader::new(&input.data);
    for op in input.script.iter().take(4096) {
        let before = (r.bit_pos(), r.overrun());
        match *op {
            Op::Ue => {
                r.ue_v();
            }
            Op::Se => {
                r.se_v();
            }
            Op::Te(c) => {
                r.te_v(c);
            }
            Op::Me(c, p) => {
                r.me_v(chroma(c), pred(p));
            }
            Op::UeK(k) => {
                r.ue_k(u32::from(k));
            }
            Op::SeK(k) => {
                r.se_k(u32::from(k));
            }
            Op::Ue64 => {
                r.ue_v64();
            }
            Op::UeMax(m) => {
                let _ = r.ue_v_max(m);
            }
            Op::SeRange(lo, hi) => {
                let _ = r.se_v_range(lo, hi);
            }
            Op::TeChecked(c) => {
                let _ = r.te_v_checked(c);
            }
            Op::UeKMax(k, m) => {
                let _ = r.ue_k_max(u32::from(k), m);
            }
        }
        let after = (r.bit_pos(), r.overrun());
        assert!(
            after != before || after.1,
            "a read neither advanced the reader nor flagged it: {op:?}"
        );
        if after.1 {
            break;
        }
    }

    // ---- 2. this crate and vaco-bitstream must never disagree --------------
    let mut a = BitReader::new(&input.data);
    let mut b = BitReader::new(&input.data);
    for _ in 0..64 {
        let mine = GolombDecode::ue_v(&mut a);
        let theirs = GolombRead::ue(&mut b);
        assert_eq!(mine, theirs, "ue(v) implementations diverged");
        assert_eq!(a.bit_pos(), b.bit_pos(), "ue(v) consumed different bits");
        assert_eq!(a.overrun(), b.overrun(), "ue(v) flagged differently");
        if a.overrun() {
            break;
        }
    }

    // ---- 3. bounded reads always come back --------------------------------
    let mut reader = BitReader::new(&input.data);
    let mut budget = Budget::new(Limits::tiny());
    let mut g = BoundedGolomb::new(&mut reader, &mut budget);
    for op in input.script.iter().take(1024) {
        let ok = match *op {
            Op::UeMax(m) | Op::UeKMax(_, m) => g.ue_v(m).is_ok(),
            Op::SeRange(lo, hi) => g.se_v(lo, hi).is_ok(),
            Op::Te(c) | Op::TeChecked(c) => g.te_v(c).is_ok(),
            Op::Me(c, p) => g.me_v(chroma(c), pred(p)).is_ok(),
            Op::UeK(k) => g.ue_k(u32::from(k), u32::MAX).is_ok(),
            _ => g.u(9).is_ok(),
        };
        if !ok {
            break;
        }
    }

    // ---- 4. write then read is the identity, and the cost model agrees -----
    let mut w = BitWriter::new();
    let mut expect = Vec::new();
    for &v in input.encode.iter().take(512) {
        // u32::MAX needs a 32-zero prefix, which no reader here accepts.
        let v = v.min(u32::MAX - 1);
        let before = w.bit_len();
        w.put_ue_v(v);
        assert_eq!(
            w.bit_len() - before,
            u64::from(map::ue_bit_len(v)),
            "ue_bit_len disagreed with the writer for {v}"
        );
        expect.push(v);
    }
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    for &v in &expect {
        assert_eq!(r.ue_v(), v, "round trip lost {v}");
    }
    assert!(!r.overrun(), "round trip overran its own output");
});
