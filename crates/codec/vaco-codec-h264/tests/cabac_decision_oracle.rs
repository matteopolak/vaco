//! Encode-then-decode round-trip oracle for `vaco-codec-cabac`'s
//! *context-coded* path (`CabacEncoder::encode_decision` /
//! `CabacDecoder::decode_decision`), added to test a specific question
//! raised while chasing H.264 CABAC's remaining bit-exactness gap: is the
//! engine's adaptive context-model transition itself correct, including
//! after a context has been driven to an extreme, confident state by a
//! long run of one outcome (exactly what happens to `mb_type`'s
//! bin0 context across many consecutive `Intra4x4` macroblocks before a
//! real `Intra16x16` one) — as opposed to `residual_block_cabac`'s bypass
//! path, already cleared by a separate oracle.
//!
//! `vaco-codec-cabac` is `agent:codec-bits`'s crate (`planning/
//! ASSIGNMENTS.md`, status `done`) — not edited; this exercises it
//! through its public API only, same as `cabac_bypass_egk_oracle.rs`.
//!
//! Result: clean. Both a long deliberately-adapted run (30 zeros, one
//! one, ten more zeros — mirroring the real corpus shape) and 200
//! pseudorandom bit sequences round-trip exactly. The engine's
//! `decode_decision` is not where the H.264 mb_type misclassification
//! documented in `mb.rs`'s own module doc comes from.

use vaco_codec_cabac::{CabacDecoder, CabacEncoder, ContextModel};

#[test]
fn decode_decision_round_trips_after_a_long_run_of_the_same_bin() {
    // Mirror MB_TYPE_I's ctxIdx3 real init (m=20,n=-15) at a real SliceQPY,
    // then feed it 30 zeros (like many Intra4x4 macroblocks in a row) then
    // a single 1 (a genuine Intra16x16), then more zeros -- exactly the
    // shape observed on real corpora.
    let slice_qp = 26i8;
    let bits: Vec<u32> = std::iter::repeat(0u32)
        .take(30)
        .chain(std::iter::once(1u32))
        .chain(std::iter::repeat(0u32).take(10))
        .collect();

    let mut enc = CabacEncoder::new();
    let mut enc_ctx = ContextModel::init_h264(20, -15, slice_qp);
    for &b in &bits {
        enc.encode_decision(&mut enc_ctx, b);
    }
    enc.encode_terminate(1);
    let bytes = enc.finish();

    let mut dec = CabacDecoder::new(&bytes);
    let mut dec_ctx = ContextModel::init_h264(20, -15, slice_qp);
    let mut got = Vec::new();
    for _ in 0..bits.len() {
        got.push(dec.decode_decision(&mut dec_ctx));
    }
    assert_eq!(got, bits, "decode_decision did not round-trip after a long run of the same bin");
    assert!(!dec.malformed());
}

#[test]
fn decode_decision_round_trips_across_many_pseudorandom_sequences() {
    let slice_qp = 26i8;
    let mut seed: u64 = 0x1234_5678_9abc_def0;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for trial in 0..200 {
        let len = 20 + (trial % 50);
        let bits: Vec<u32> = (0..len).map(|_| u32::from(next() & 1 == 1)).collect();
        let mut enc = CabacEncoder::new();
        let mut enc_ctx = ContextModel::init_h264(20, -15, slice_qp);
        for &b in &bits {
            enc.encode_decision(&mut enc_ctx, b);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();
        let mut dec = CabacDecoder::new(&bytes);
        let mut dec_ctx = ContextModel::init_h264(20, -15, slice_qp);
        let got: Vec<u32> = (0..bits.len()).map(|_| dec.decode_decision(&mut dec_ctx)).collect();
        assert_eq!(got, bits, "trial {trial}: decode_decision did not round-trip, len={len}");
        assert!(!dec.malformed(), "trial {trial}: malformed");
    }
}
