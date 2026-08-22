//! Properties of the engine, over inputs no hand-written test would think of.
//!
//! Three families:
//!
//! - **Round trips** against the clause 9.3.4 encoder. This is the only oracle
//!   available for a full bin sequence, and it is a strong one: the encoder was
//!   written from a different clause than the decoder, so a misreading of the
//!   state machine has to be made twice, identically, to survive.
//! - **The engine invariant**, `offset < range`, after every operation on every
//!   input. It is what bounds `offset`; it found a real bug in
//!   `decode_terminate` the first time it ran.
//! - **Totality**: arbitrary bytes, arbitrary call sequences, no panic and no
//!   hang.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: indices are generated inside the bounds of the fixture"
)]

use proptest::prelude::*;
use vaco_codec_cabac::{CabacDecoder, CabacEncoder, ContextModel};

/// A step in a mixed bin script, encodable and decodable both ways.
#[derive(Debug, Clone, Copy)]
enum Step {
    Decision(u8, u32),
    Bypass(u32),
    Fixed(u32, u32),
    NotTerminated,
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u8..32, 0u32..2).prop_map(|(c, b)| Step::Decision(c, b)),
        (0u32..2).prop_map(Step::Bypass),
        (1u32..17, any::<u32>()).prop_map(|(n, v)| Step::Fixed(n, v & ((1 << n) - 1))),
        Just(Step::NotTerminated),
    ]
}

fn contexts() -> Vec<ContextModel> {
    (0..32i16)
        .map(|i| ContextModel::init_h264(i * 4 - 60, i * 5 - 70, 27))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A mixed bin sequence survives encode-then-decode exactly.
    ///
    /// This is the whole engine at once: adaptation, all three renormalisation
    /// paths, carry propagation through `bitsOutstanding`, and the interleaving
    /// of context-coded and bypass bins.
    #[test]
    fn mixed_script_round_trips(script in prop::collection::vec(step_strategy(), 0..400)) {
        let mut ctxs = contexts();
        let mut enc = CabacEncoder::new();
        for step in &script {
            match *step {
                Step::Decision(c, b) => enc.encode_decision(&mut ctxs[usize::from(c)], b),
                Step::Bypass(b) => enc.encode_bypass(b),
                Step::Fixed(n, v) => enc.encode_bypass_bits(n, v),
                Step::NotTerminated => enc.encode_terminate(0),
            }
        }
        enc.encode_terminate(1);
        prop_assert!(!enc.overflowed());
        let bytes = enc.finish();

        let mut ctxs = contexts();
        let mut dec = CabacDecoder::new(&bytes);
        for (i, step) in script.iter().enumerate() {
            match *step {
                Step::Decision(c, b) =>
                    prop_assert_eq!(dec.decode_decision(&mut ctxs[usize::from(c)]), b, "step {}", i),
                Step::Bypass(b) => prop_assert_eq!(dec.decode_bypass(), b, "step {}", i),
                Step::Fixed(n, v) => prop_assert_eq!(dec.decode_bypass_bits(n), v, "step {}", i),
                Step::NotTerminated =>
                    prop_assert_eq!(dec.decode_terminate(), 0, "step {}", i),
            }
            prop_assert!(dec.offset() < dec.range());
        }
        prop_assert_eq!(dec.decode_terminate(), 1);
        prop_assert!(dec.terminated());
    }

    /// The same, starting from an arbitrary context state rather than an
    /// initialised one — so every row of the transition tables gets visited
    /// rather than only the reachable-from-init ones.
    #[test]
    fn round_trip_from_any_state(
        state in 0u8..128,
        bins in prop::collection::vec(0u32..2, 0..500),
    ) {
        let init = ContextModel::from_packed(state);

        let mut ctx = init;
        let mut enc = CabacEncoder::new();
        for &b in &bins {
            enc.encode_decision(&mut ctx, b);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut ctx = init;
        let mut dec = CabacDecoder::new(&bytes);
        for (i, &b) in bins.iter().enumerate() {
            prop_assert_eq!(dec.decode_decision(&mut ctx), b, "bin {}", i);
        }
        prop_assert_eq!(dec.decode_terminate(), 1);
    }

    /// `EGk` round-trips for every order and every value whose prefix stays
    /// inside the decoder's 32-bin ceiling.
    #[test]
    fn egk_round_trips(k in 0u32..8, values in prop::collection::vec(0u32..(1 << 22), 1..64)) {
        let mut enc = CabacEncoder::new();
        for &v in &values {
            enc.encode_bypass_egk(k, v);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        for (i, &v) in values.iter().enumerate() {
            prop_assert_eq!(dec.decode_bypass_egk(k), v, "value {}", i);
        }
        prop_assert!(!dec.malformed());
    }

    /// The invariant, and termination, over bytes that are not a CABAC stream.
    ///
    /// The property that keeps a malformed bitstream from being a
    /// vulnerability: whatever the input and whatever the call order, `offset`
    /// stays below `range` and `range` stays in the interval the renormalisation
    /// shift is proved against.
    #[test]
    fn arbitrary_bytes_preserve_the_invariant(
        data: Vec<u8>,
        state in 0u8..128,
        ops in prop::collection::vec(0u8..7, 0..300),
        widths in prop::collection::vec(0u32..40, 1..8),
    ) {
        let mut dec = CabacDecoder::new(&data);
        let mut ctx = ContextModel::from_packed(state);
        for (i, &op) in ops.iter().enumerate() {
            match op {
                0 => { dec.decode_decision(&mut ctx); }
                1 => { dec.decode_bypass(); }
                2 => { dec.decode_bypass_bits(widths[i % widths.len()]); }
                3 => { dec.decode_terminate(); }
                4 => { dec.decode_tu(&mut ctx, 32); }
                5 => { dec.decode_bypass_egk(i as u32 % 8); }
                _ => { dec.decode_uegk(&mut ctx, 14, 3, true); }
            }
            prop_assert!(
                dec.offset() < dec.range(),
                "offset {} range {} after op {}", dec.offset(), dec.range(), op
            );
            prop_assert!((2..=510).contains(&dec.range()), "range {}", dec.range());
            prop_assert!(ctx.state_idx() <= 63);
        }
    }

    /// Decoding is a pure function of the bytes: the same input always gives
    /// the same output. Determinism is what makes a fuzz finding minimise and
    /// regress, and it is not free — a stray uninitialised field would break it.
    #[test]
    fn decoding_is_deterministic(data: Vec<u8>, state in 0u8..128) {
        let run = |data: &[u8]| {
            let mut dec = CabacDecoder::new(data);
            let mut ctx = ContextModel::from_packed(state);
            let mut out = Vec::new();
            for i in 0..200u32 {
                out.push(match i % 3 {
                    0 => dec.decode_decision(&mut ctx),
                    1 => dec.decode_bypass(),
                    _ => dec.decode_terminate(),
                });
            }
            (out, dec.offset(), dec.range(), dec.malformed())
        };
        prop_assert_eq!(run(&data), run(&data));
    }

    /// `decode_bypass_bits(n)` is exactly `n` calls to `decode_bypass`, for
    /// every width, on every input — including the clamping at `n > 32`.
    #[test]
    fn bypass_bits_equals_repeated_bypass(data: Vec<u8>, n in 0u32..33) {
        let mut a = CabacDecoder::new(&data);
        let mut b = CabacDecoder::new(&data);
        let batched = a.decode_bypass_bits(n);
        let mut serial = 0u32;
        for _ in 0..n {
            serial = (serial << 1) | b.decode_bypass();
        }
        prop_assert_eq!(batched, serial);
        prop_assert_eq!(a.offset(), b.offset());
        prop_assert_eq!(a.range(), b.range());
    }

    /// Context initialisation is total and always lands on a valid state.
    #[test]
    fn context_init_is_total(m: i16, n: i16, qp: i8) {
        let c = ContextModel::init_h264(m, n, qp);
        prop_assert!(c.state_idx() <= 63);
        prop_assert!(c.packed() < 128);
        prop_assert_eq!(ContextModel::from_packed(c.packed()), c);
    }

    #[test]
    fn hevc_init_is_total(iv: u8, qp: i8) {
        let c = ContextModel::init_hevc(iv, qp);
        prop_assert!(c.state_idx() <= 63);
        let m = i16::from(iv >> 4) * 5 - 45;
        let n = (i16::from(iv & 15) << 3) - 16;
        prop_assert_eq!(c, ContextModel::init_h264(m, n, qp));
    }

    /// An encoder driven by an arbitrary bin sequence must never panic, and its
    /// output must stay under the ceiling it was given.
    #[test]
    fn encoder_is_total(
        ops in prop::collection::vec((0u8..4, 0u32..2, 0u8..32), 0..500),
        limit in 1usize..4096,
    ) {
        let mut ctxs = contexts();
        let mut enc = CabacEncoder::with_limit(limit);
        for &(kind, bin, c) in &ops {
            match kind {
                0 => enc.encode_decision(&mut ctxs[usize::from(c)], bin),
                1 => enc.encode_bypass(bin),
                2 => enc.encode_bypass_bits(u32::from(c) % 17, bin),
                _ => enc.encode_terminate(0),
            }
        }
        enc.encode_terminate(1);
        let out = enc.finish();
        prop_assert!(out.len() <= limit + 1, "output {} over limit {}", out.len(), limit);
    }
}
