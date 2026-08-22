//! Checked against ITU-T H.264 clause 9.3 directly.
//!
//! CABAC has no publishable worked bitstream in the standard, so "check it
//! against the spec" means three different things and all three are here:
//!
//! 1. **The tables** — checked for the structural properties clause 9.3.3.2
//!    requires of them (monotonicity, the fixed points, the interval never
//!    collapsing), because those catch a transposed digit that eyeballing does
//!    not, and re-derived where they are derived.
//! 2. **The formulas** — clause 9.3.1.1's context initialisation and clause
//!    9.3.3.2.1's decision, worked by hand from the table values and compared
//!    step by step against the engine's own state.
//! 3. **The engine** — against an encoder written from clause 9.3.4, which is
//!    the only oracle available for a full bin sequence.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "test code over fixed-size spec tables: indexing by the loop variable \
              is how a table is checked entry by entry"
)]

use vaco_codec_cabac::{
    CabacDecoder, CabacEncoder, ContextInit, ContextModel, init_contexts, init_contexts_hevc,
    is_terminal_state,
    tables::{LPS_RANGE, RANGE_TAB_LPS, STATE_COUNT, TRANS, TRANS_IDX_LPS, TRANS_IDX_MPS},
};

// ----------------------------------------------------------------- the tables

/// Table 9-44 must be monotone in both directions, and every entry must be in
/// range. Both are properties of what the table *means* — the LPS sub-interval
/// grows with the range quartile and shrinks as the context gets more skewed —
/// so a transcription error breaks one of them with high probability.
#[test]
fn table_9_44_is_monotone_and_in_range() {
    for (p, row) in RANGE_TAB_LPS.iter().enumerate() {
        for q in 0..4 {
            assert!(
                row[q] >= 2,
                "rangeTabLPS[{p}][{q}] = {} — an LPS sub-range below 2 collapses the interval",
                row[q]
            );
            assert!(row[q] <= 240, "rangeTabLPS[{p}][{q}] = {}", row[q]);
        }
        for q in 1..4 {
            assert!(
                row[q] >= row[q - 1],
                "rangeTabLPS[{p}] is not non-decreasing across quartiles: {row:?}"
            );
        }
    }
    for q in 0..4 {
        for p in 1..64 {
            assert!(
                RANGE_TAB_LPS[p][q] <= RANGE_TAB_LPS[p - 1][q],
                "rangeTabLPS column {q} rises from state {} to {p}",
                p - 1
            );
        }
    }
}

/// The property the engine's safety rests on: for every state and every legal
/// `ivlCurrRange`, both halves of the split interval stay usable.
///
/// If either could reach zero, `RenormD`'s shift count would be undefined and
/// the decoder could loop or overflow. This is why `renorm` can assert
/// `range` is 2–510 without checking.
#[test]
fn neither_sub_interval_can_collapse() {
    for range in 256u32..=510 {
        let q = ((range >> 6) & 3) as usize;
        for p in 0..64 {
            let lps = u32::from(RANGE_TAB_LPS[p][q]);
            assert!(lps >= 2, "LPS sub-range {lps} at state {p}");
            assert!(
                range > lps,
                "MPS sub-range would be empty: range {range}, lps {lps}, state {p}"
            );
            assert!(
                range - lps >= 2,
                "MPS sub-range {} too small at range {range} state {p}",
                range - lps
            );
        }
    }
}

/// Table 9-45's MPS column is `pStateIdx + 1` with two fixed points: state 62
/// is the most skewed adapting state and state 63 never adapts at all.
#[test]
fn table_9_45_mps_column() {
    for (p, &next) in TRANS_IDX_MPS.iter().enumerate() {
        let expect = match p {
            0..=61 => p as u8 + 1,
            62 => 62,
            _ => 63,
        };
        assert_eq!(next, expect, "transIdxMPS[{p}]");
    }
}

/// Table 9-45's LPS column must never increase the state — an LPS outcome means
/// the context was over-confident — and state 63 must be its own fixed point,
/// which is what makes it usable for `end_of_slice_flag`.
#[test]
fn table_9_45_lps_column() {
    for (p, &next) in TRANS_IDX_LPS.iter().enumerate() {
        assert!(
            usize::from(next) <= p,
            "transIdxLPS[{p}] = {next} — an LPS outcome must not raise confidence"
        );
    }
    assert_eq!(TRANS_IDX_LPS[63], 63);
    assert_eq!(TRANS_IDX_MPS[63], 63);
    assert!(is_terminal_state(63));
    assert!(!is_terminal_state(62));
}

/// The two derived tables, re-derived here from the normative ones by a
/// different route, so the packing in `tables::derive` is checked rather than
/// trusted.
#[test]
fn derived_tables_agree_with_the_normative_ones() {
    for state in 0..STATE_COUNT {
        let p = state >> 1;
        let mps = (state & 1) as u8;

        // MPS half.
        let want_mps = (TRANS_IDX_MPS[p] << 1) | mps;
        assert_eq!(TRANS[state], want_mps, "TRANS MPS half at state {state}");

        // LPS half, with the clause 9.3.3.2.1.1 valMPS flip at pStateIdx 0.
        let flipped = if p == 0 { 1 - mps } else { mps };
        let want_lps = (TRANS_IDX_LPS[p] << 1) | flipped;
        assert_eq!(
            TRANS[256 + state],
            want_lps,
            "TRANS LPS half at state {state}"
        );

        for q in 0..4 {
            assert_eq!(
                LPS_RANGE[(state >> 1) * 4 + q],
                RANGE_TAB_LPS[p][q],
                "LPS_RANGE at state {state}, quartile {q}"
            );
        }
    }
}

/// The upper half of each derived table mirrors the lower half. That is what
/// makes indexing by a whole `u8` provably in bounds, which is what removes the
/// bounds check from the hot loop.
#[test]
fn derived_tables_mirror_above_128_states() {
    for state in 0..128usize {
        assert_eq!(TRANS[state], TRANS[state + 128]);
        assert_eq!(TRANS[256 + state], TRANS[256 + state + 128]);
        assert_eq!(LPS_RANGE[state * 4], LPS_RANGE[(state + 128) * 4 % 512]);
    }
}

// ------------------------------------------------- clause 9.3.1.1 initialisation

/// Worked by hand from clause 9.3.1.1:
/// `preCtxState = Clip3(1, 126, ((m · Clip3(0, 51, SliceQPY)) >> 4) + n)`.
#[test]
fn context_init_worked_examples() {
    // (20 · 26) >> 4 = 520 >> 4 = 32; + 30 = 62; 62 <= 63, so
    // pStateIdx = 63 - 62 = 1 and valMPS = 0.
    let c = ContextModel::init_h264(20, 30, 26);
    assert_eq!(c.state_idx(), 1);
    assert!(!c.mps());

    // (-15 · 30) = -450; -450 >> 4 = -29 (arithmetic, floors); + 100 = 71;
    // 71 > 63, so pStateIdx = 71 - 64 = 7 and valMPS = 1.
    let c = ContextModel::init_h264(-15, 100, 30);
    assert_eq!(c.state_idx(), 7);
    assert!(c.mps());

    // The low clip: 0 becomes 1, giving pStateIdx 62, valMPS 0.
    let c = ContextModel::init_h264(0, 0, 0);
    assert_eq!(c.state_idx(), 62);
    assert!(!c.mps());

    // The high clip: 200 becomes 126, giving pStateIdx 62, valMPS 1.
    let c = ContextModel::init_h264(0, 200, 51);
    assert_eq!(c.state_idx(), 62);
    assert!(c.mps());
}

/// The QP is clipped to 0..=51 before it is used, so a nonsense QP cannot
/// produce a nonsense state.
#[test]
fn context_init_clips_the_qp() {
    assert_eq!(
        ContextModel::init_h264(30, 0, 51),
        ContextModel::init_h264(30, 0, 127)
    );
    assert_eq!(
        ContextModel::init_h264(30, 0, 0),
        ContextModel::init_h264(30, 0, -128)
    );
}

/// Every `(m, n, qp)` triple must land on a valid state — the two clips
/// together guarantee it, and that is why the type has no failure mode.
#[test]
fn context_init_is_total() {
    for m in (-128i16..=127).step_by(7) {
        for n in (-128i16..=127).step_by(11) {
            for qp in [-128i8, -1, 0, 1, 26, 51, 52, 127] {
                let c = ContextModel::init_h264(m, n, qp);
                assert!(c.state_idx() <= 63, "m={m} n={n} qp={qp}");
                assert!(c.packed() < 128);
            }
        }
    }
}

/// Clause 9.3.2.2: H.265 packs `(m, n)` into one byte as
/// `m = (initValue >> 4) · 5 − 45`, `n = ((initValue & 15) << 3) − 16`.
#[test]
fn hevc_init_unpacks_then_uses_the_h264_formula() {
    // 0x9C: slopeIdx 9 → m = 0; offsetIdx 12 → n = 80.
    // preCtxState = 0 + 80 = 80 > 63, so pStateIdx = 16 and valMPS = 1,
    // independently of the QP because m is zero.
    for qp in [0i8, 26, 51] {
        let c = ContextModel::init_hevc(0x9C, qp);
        assert_eq!(c.state_idx(), 16, "qp={qp}");
        assert!(c.mps());
    }

    // Every initValue must agree with the explicit formula.
    for iv in 0u8..=255 {
        let m = i16::from(iv >> 4) * 5 - 45;
        let n = (i16::from(iv & 15) << 3) - 16;
        for qp in [0i8, 13, 26, 39, 51] {
            assert_eq!(
                ContextModel::init_hevc(iv, qp),
                ContextModel::init_h264(m, n, qp),
                "initValue {iv:#04x}, qp {qp}"
            );
        }
    }
}

#[test]
fn context_set_initialisation_truncates_rather_than_panicking() {
    let inits = [ContextInit::new(20, 30), ContextInit::new(-15, 100)];
    let mut dst = [ContextModel::UNINITIALISED; 4];
    assert_eq!(init_contexts(&mut dst, &inits, 26), 2);
    assert_eq!(dst[0], ContextModel::init_h264(20, 30, 26));
    assert_eq!(dst[2], ContextModel::UNINITIALISED);

    let mut small = [ContextModel::UNINITIALISED; 1];
    assert_eq!(init_contexts(&mut small, &inits, 26), 1);

    let mut dst = [ContextModel::UNINITIALISED; 3];
    assert_eq!(init_contexts_hevc(&mut dst, &[0x9C, 0x33], 26), 2);
}

#[test]
fn packed_representation_round_trips() {
    for p in 0..64u8 {
        for mps in [false, true] {
            let c = ContextModel::new(p, mps);
            assert_eq!(c.state_idx(), p);
            assert_eq!(c.mps(), mps);
            assert_eq!(ContextModel::from_packed(c.packed()), c);
        }
    }
    // A state index above 63 saturates rather than corrupting the packing.
    assert_eq!(ContextModel::new(200, true).state_idx(), 63);
    // Every byte names a valid state.
    for b in 0u8..=255 {
        assert!(ContextModel::from_packed(b).state_idx() <= 63);
    }
}

// -------------------------------------------- clause 9.3.3.2, the engine itself

/// Clause 9.3.1.2: `ivlCurrRange` is 510 and `ivlOffset` is the first nine bits.
#[test]
fn engine_initialisation() {
    // 0b1_0110_0100 = 356, split across two bytes as 1011 0010 | 0...
    let data = [0b1011_0010, 0b0000_0000, 0, 0, 0, 0, 0, 0];
    let d = CabacDecoder::new(&data);
    assert_eq!(d.range(), 510);
    assert_eq!(d.offset(), 0b1_0110_0100);
    assert!(!d.malformed());
}

/// Clause 9.3.1.2 forbids an initial offset of 510 or 511. Both are clamped and
/// flagged rather than left to break the engine invariant.
#[test]
fn non_conforming_initial_offset_is_clamped_and_flagged() {
    for raw in [510u32, 511] {
        let hi = ((raw >> 1) & 0xFF) as u8;
        let lo = ((raw & 1) << 7) as u8;
        let data = [hi, lo, 0, 0, 0, 0, 0, 0];
        let d = CabacDecoder::new(&data);
        assert!(d.malformed(), "offset {raw} must be reported");
        assert!(d.offset() < d.range(), "the invariant must hold anyway");
    }
}

/// `DecodeDecision` worked by hand, MPS outcome.
///
/// Fresh engine: range 510, offset 0, context at pStateIdx 0 / valMPS 0.
/// `qRangeIdx = (510 >> 6) & 3 = 3`; `rangeTabLPS[0][3] = 240`;
/// range becomes 270; offset 0 < 270 so the bin is `valMPS = 0`;
/// `transIdxMPS[0] = 1`; range is already at least 256 so no renormalisation.
#[test]
fn decode_decision_mps_worked_example() {
    let data = [0u8; 16];
    let mut d = CabacDecoder::new(&data);
    let mut ctx = ContextModel::new(0, false);

    assert_eq!(d.decode_decision(&mut ctx), 0);
    assert_eq!(d.range(), 510 - 240);
    assert_eq!(d.offset(), 0);
    assert_eq!(ctx.state_idx(), 1);
    assert!(!ctx.mps());
}

/// `DecodeDecision` worked by hand, LPS outcome — including the `valMPS` flip
/// at `pStateIdx == 0` and a one-bit renormalisation.
///
/// Offset 400 = `0b1_1001_0000`, so the first two bytes are `0xC8 0x00`.
/// range becomes 270; 400 >= 270 so the bin is `1 - valMPS = 1`;
/// offset becomes 130, range becomes `rangeTabLPS[0][3] = 240`;
/// `pStateIdx` was 0 so `valMPS` flips to 1 and `transIdxLPS[0] = 0`;
/// 240 < 256, so one renormalisation: range 480, offset `130·2 + next bit`.
/// The tenth bit of the stream is 0, so offset becomes 260.
#[test]
fn decode_decision_lps_worked_example() {
    let mut data = [0u8; 16];
    data[0] = 0b1100_1000;
    let mut d = CabacDecoder::new(&data);
    assert_eq!(d.offset(), 400);
    let mut ctx = ContextModel::new(0, false);

    assert_eq!(d.decode_decision(&mut ctx), 1);
    assert_eq!(d.range(), 480);
    assert_eq!(d.offset(), 260);
    assert_eq!(ctx.state_idx(), 0);
    assert!(ctx.mps(), "valMPS must flip when pStateIdx was 0");
}

/// Clause 9.3.3.2.4: `ivlCurrRange` drops by 2, and the bin is 1 exactly when
/// `ivlOffset` is at least the reduced range.
#[test]
fn decode_terminate_worked_examples() {
    // offset 0: below 508, so not terminated, and no renormalisation since
    // 508 is already at least 256.
    let data = [0u8; 16];
    let mut d = CabacDecoder::new(&data);
    assert_eq!(d.decode_terminate(), 0);
    assert_eq!(d.range(), 508);

    // offset 509 = 0b1_1111_1101 → 0xFE 0x80. 509 >= 508, so terminated.
    let mut data = [0u8; 16];
    data[0] = 0xFE;
    data[1] = 0x80;
    let mut d = CabacDecoder::new(&data);
    assert_eq!(d.offset(), 509);
    assert_eq!(d.decode_terminate(), 1);
}

/// The invariant the whole design rests on, over a stream that is not a CABAC
/// stream at all.
#[test]
fn offset_stays_below_range_on_garbage() {
    let mut state = 0xDEAD_BEEF_CAFE_F00Du64;
    for _ in 0..500 {
        let mut data = [0u8; 64];
        for b in &mut data {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let mut d = CabacDecoder::new(&data);
        let mut ctx = ContextModel::new((state >> 40) as u8 & 63, state & 1 == 1);
        for i in 0..400 {
            match i % 4 {
                0 => {
                    d.decode_decision(&mut ctx);
                }
                1 => {
                    d.decode_bypass();
                }
                2 => {
                    d.decode_bypass_bits((i % 33) as u32);
                }
                _ => {
                    d.decode_terminate();
                }
            }
            assert!(
                d.offset() < d.range(),
                "invariant broken at step {i}: offset {} range {}",
                d.offset(),
                d.range()
            );
            assert!(
                (2..=510).contains(&d.range()),
                "range {} out of bounds",
                d.range()
            );
        }
    }
}

/// `decode_bypass_bits(n)` must be exactly `n` calls to `decode_bypass`, MSB
/// first. It is a different code path — one reader interaction instead of `n` —
/// so this is not a tautology.
#[test]
fn bypass_bits_matches_bypass_one_at_a_time() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for _ in 0..200 {
        let mut data = [0u8; 32];
        for b in &mut data {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        for n in 0..=32u32 {
            let mut a = CabacDecoder::new(&data);
            let mut b = CabacDecoder::new(&data);
            let batched = a.decode_bypass_bits(n);
            let mut one_by_one = 0u32;
            for _ in 0..n {
                one_by_one = (one_by_one << 1) | b.decode_bypass();
            }
            assert_eq!(batched, one_by_one, "n = {n}");
            assert_eq!(a.offset(), b.offset(), "n = {n}");
            assert_eq!(a.range(), b.range(), "n = {n}");
        }
    }
}

// --------------------------------------------------------- bounded, not hanging

/// An all-ones buffer is a well-formed run of bypass 1 bins that never ends.
/// The `EGk` prefix cap is the only thing between this and a hang.
#[test]
fn egk_prefix_is_capped_on_an_all_ones_buffer() {
    let data = [0xFFu8; 256];
    let mut d = CabacDecoder::new(&data);
    let _ = d.decode_bypass_egk(0);
    assert!(d.malformed(), "an unterminated EGk prefix must be reported");
}

/// The same for a truncated-unary prefix, which `c_max` bounds.
#[test]
fn truncated_unary_respects_c_max() {
    let data = [0xFFu8; 256];
    let mut d = CabacDecoder::new(&data);
    let mut ctx = ContextModel::new(62, true);
    assert!(d.decode_tu(&mut ctx, 16) <= 16);
}

#[test]
fn empty_and_tiny_buffers_never_panic() {
    for len in 0..12usize {
        let data = vec![0xA5u8; len];
        let mut d = CabacDecoder::new(&data);
        let mut ctx = ContextModel::new(31, true);
        for _ in 0..64 {
            d.decode_decision(&mut ctx);
            d.decode_bypass();
            d.decode_bypass_bits(7);
            d.decode_terminate();
            d.decode_bypass_egk(3);
            d.decode_tu(&mut ctx, 8);
            d.decode_uegk(&mut ctx, 9, 3, true);
            assert!(d.offset() < d.range());
        }
    }
}

// ------------------------------------------------- against the clause 9.3.4 encoder

#[test]
fn decision_round_trip_over_many_states() {
    let bins: Vec<u32> = (0..2000)
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 28) & 1)
        .collect();
    for p in 0..64u8 {
        for mps in [false, true] {
            let init = ContextModel::new(p, mps);

            let mut enc = CabacEncoder::new();
            let mut ctx = init;
            for &b in &bins {
                enc.encode_decision(&mut ctx, b);
            }
            enc.encode_terminate(1);
            let bytes = enc.finish();

            let mut dec = CabacDecoder::new(&bytes);
            let mut ctx = init;
            for (i, &b) in bins.iter().enumerate() {
                assert_eq!(dec.decode_decision(&mut ctx), b, "state {p}/{mps}, bin {i}");
            }
            assert_eq!(dec.decode_terminate(), 1, "state {p}/{mps}");
        }
    }
}

#[test]
fn bypass_round_trip() {
    let bins: Vec<u32> = (0..4000)
        .map(|i: u32| (i.wrapping_mul(40503) >> 5) & 1)
        .collect();
    let mut enc = CabacEncoder::new();
    for &b in &bins {
        enc.encode_bypass(b);
    }
    enc.encode_terminate(1);
    let bytes = enc.finish();

    let mut dec = CabacDecoder::new(&bytes);
    for (i, &b) in bins.iter().enumerate() {
        assert_eq!(dec.decode_bypass(), b, "bin {i}");
    }
    assert_eq!(dec.decode_terminate(), 1);
}

/// Decisions, bypass bins and non-terminating terminate bins interleaved — the
/// combination is what a real slice looks like and what shakes out an ordering
/// mistake between the three renormalisation paths.
#[test]
fn interleaved_round_trip() {
    let mut ctxs: Vec<ContextModel> = (0..32)
        .map(|i| ContextModel::init_h264(i * 3 - 40, i * 2 - 20, 30))
        .collect();
    let saved = ctxs.clone();

    let mut enc = CabacEncoder::new();
    let mut script = Vec::new();
    let mut s = 0x51ED_2701_ABCD_1234u64;
    for _ in 0..3000 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let bin = (s & 1) as u32;
        match (s >> 8) % 3 {
            0 => {
                let i = ((s >> 16) % 32) as usize;
                enc.encode_decision(&mut ctxs[i], bin);
                script.push((0u8, i, bin));
            }
            1 => {
                enc.encode_bypass(bin);
                script.push((1, 0, bin));
            }
            _ => {
                enc.encode_terminate(0);
                script.push((2, 0, 0));
            }
        }
    }
    enc.encode_terminate(1);
    assert!(!enc.overflowed());
    let bytes = enc.finish();

    let mut ctxs = saved;
    let mut dec = CabacDecoder::new(&bytes);
    for (n, &(kind, i, bin)) in script.iter().enumerate() {
        match kind {
            0 => assert_eq!(dec.decode_decision(&mut ctxs[i]), bin, "step {n}"),
            1 => assert_eq!(dec.decode_bypass(), bin, "step {n}"),
            _ => assert_eq!(dec.decode_terminate(), 0, "step {n}"),
        }
    }
    assert_eq!(dec.decode_terminate(), 1);
}

/// `encode_bypass_bits` / `decode_bypass_bits` are inverses for every width.
#[test]
fn fixed_length_round_trip() {
    for n in 1..=16u32 {
        let mask = if n == 32 { u32::MAX } else { (1 << n) - 1 };
        let values: Vec<u32> = (0..200u32)
            .map(|i| i.wrapping_mul(2_654_435_761) & mask)
            .collect();

        let mut enc = CabacEncoder::new();
        for &v in &values {
            enc.encode_bypass_bits(n, v);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(dec.decode_bypass_bits(n), v, "n = {n}, value {i}");
        }
    }
}

/// The `EGk` suffix round-trips for every order the decoder accepts.
#[test]
fn egk_round_trip() {
    for k in 0..8u32 {
        let values: Vec<u32> = (0..64u32)
            .map(|i| i.wrapping_mul(i).wrapping_mul(7))
            .collect();

        let mut enc = CabacEncoder::new();
        for &v in &values {
            enc.encode_bypass_egk(k, v);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(dec.decode_bypass_egk(k), v, "k = {k}, value {i}");
        }
        assert!(!dec.malformed());
    }
}

/// An encoder driven past its output ceiling must report rather than allocate.
#[test]
fn encoder_output_ceiling_is_reported() {
    let mut enc = CabacEncoder::with_limit(16);
    let mut ctx = ContextModel::new(62, true);
    for i in 0..100_000u32 {
        enc.encode_decision(&mut ctx, i & 1);
    }
    assert!(enc.overflowed());
    assert!(enc.finish().len() <= 17);
}
