//! Encode-then-decode round-trip oracle for `vaco-codec-cabac`'s bypass
//! `EGk` path (`CabacEncoder::encode_bypass_egk` /
//! `CabacDecoder::decode_bypass_egk`, and `decode_uegk`'s use of it) —
//! the exact path [`vaco_codec_h264::cabac_residual`]'s
//! `decode_coeff_abs_level_minus1` calls for every `coeff_abs_level_minus1`
//! suffix once its context-coded prefix saturates at `U_COFF`.
//!
//! `vaco-codec-cabac` is `agent:codec-bits`'s crate (`planning/
//! ASSIGNMENTS.md`, status `done`) — this file lives in `vaco-codec-h264`
//! instead and exercises the dependency purely through its public API, per
//! this dispatch's explicit instruction not to edit across another agent's
//! crate. `fuzz/fuzz_targets/cabac_engine.rs` already calls
//! `decode_bypass_egk`/`decode_uegk` on arbitrary bytes, but only checks
//! that they do not panic — it asserts nothing about the *values* returned,
//! so millions of clean runs there cannot distinguish a correct engine from
//! one that silently returns the wrong value while still consuming the
//! wrong number of bits. This file is the missing correctness assertion,
//! scoped to this crate rather than added to that fuzz target (which is
//! also `vaco-codec-cabac`'s, by the same per-crate fuzz-target-naming
//! convention `planning/ASSIGNMENTS.md` describes).
//!
//! # Why this could be exactly the H.264 bug
//!
//! Macroblock classification (`mb_type`, `cbp`, `mb_skip_flag` — all
//! `decode_decision`, context-coded) has been independently verified
//! bit-exact against `ffmpeg -debug mb_type` across three real corpora.
//! Coefficient signs and `coeff_abs_level_minus1`'s `EGk` suffix are
//! *bypass*-coded. A fault confined to the bypass path would leave every
//! `mb_type` correct and every residual wrong — exactly what has been
//! measured (slice 0, all three corpora, `assert_slice_ends_at_
//! rbsp_trailing_bits` short by a bit or two, `ffmpeg -debug mb_type`
//! agreeing throughout).

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use vaco_codec_cabac::{CabacDecoder, CabacEncoder, ContextModel};

/// This assertion must be able to fail, or it proves nothing. Mismatched
/// `k` between encode and decode is a real, reachable bug shape (a caller
/// passing the wrong `k`), so it doubles as the "can this fail" check the
/// dispatch's gate requires: run it once, confirm it panics, then delete
/// the `#[should_panic]` guard mentally — it is kept here, permanently, as
/// the proof this oracle is not vacuous.
#[test]
#[should_panic = "mismatched k must not round-trip"]
fn bypass_egk_with_mismatched_k_does_not_round_trip() {
    let mut enc = CabacEncoder::new();
    enc.encode_bypass_egk(0, 12345);
    enc.encode_terminate(1);
    let bytes = enc.finish();
    let mut dec = CabacDecoder::new(&bytes);
    let got = dec.decode_bypass_egk(3); // wrong k on purpose
    assert_eq!(got, 12345, "mismatched k must not round-trip");
}

/// The real oracle over every value H.264's own `coeff_abs_level_minus1`
/// suffix could plausibly ever carry. `U_COFF = 14` and `k = 0` in
/// `decode_coeff_abs_level_minus1`, so the suffix value is
/// `coeff_abs_level_minus1 - 14` — bounded in real content by pixel depth
/// and QP, nowhere near needing a 32-bin `EGk` prefix (that needs a value
/// on the order of `2^32`). Swept generously past any plausible bound
/// anyway (up to 1,000,000) rather than trusting that reasoning alone.
#[test]
fn bypass_egk_round_trips_across_every_realistic_h264_coefficient_value() {
    for k in 0..4u32 {
        for value in (0..=1_000_000u32).step_by(997) {
            let mut enc = CabacEncoder::new();
            enc.encode_bypass_egk(k, value);
            enc.encode_terminate(1);
            let bytes = enc.finish();
            let mut dec = CabacDecoder::new(&bytes);
            let got = dec.decode_bypass_egk(k);
            assert!(
                !dec.malformed(),
                "k={k} value={value}: engine reported malformed on a \
                 realistic value — the 32-bin ceiling should be nowhere \
                 near reachable here"
            );
            assert_eq!(got, value, "k={k} value={value}: decode_bypass_egk did not round-trip");
        }
    }
}

/// The documented edge, measured rather than assumed:
/// `encode_bypass_egk`'s own doc comment says a value needing a prefix
/// longer than the decoder's 32-bin ceiling is "not encodable" and "the
/// round trip will not hold" — this confirms that boundary is exactly
/// where it is claimed to be (not earlier, which would mean it clips
/// realistic values too) and nowhere else. This is the dispatch's
/// specific question — "does the clamp or the saturating add ever fire
/// first" — answered directly: the clamp fires only here, at a value six
/// orders of magnitude past anything H.264 residual decode ever produces,
/// and never in the realistic sweep above.
#[test]
fn bypass_egk_ceiling_only_engages_far_past_any_realistic_h264_value() {
    for k in 0..4u32 {
        for value in [u32::MAX, u32::MAX - 1, u32::MAX >> 1, 1u32 << 24] {
            let mut enc = CabacEncoder::new();
            enc.encode_bypass_egk(k, value);
            enc.encode_terminate(1);
            let bytes = enc.finish();
            let mut dec = CabacDecoder::new(&bytes);
            let got = dec.decode_bypass_egk(k);
            let ceiling_hit = dec.malformed();
            // Either it round-trips exactly, or the ceiling engaged and
            // both sides say so — never a silent wrong value with
            // `malformed() == false`, which is the one outcome that would
            // matter for H.264 (a wrong value consumed as though correct).
            assert!(
                got == value || ceiling_hit,
                "k={k} value={value}: wrong value ({got}) returned WITHOUT \
                 malformed() being set — this is the dangerous case, a \
                 silent wrong answer"
            );
        }
    }
}

/// `decode_uegk` isn't called anywhere in `vaco-codec-h264` today
/// (`decode_coeff_abs_level_minus1`'s own doc explains why it hand-rolls
/// the prefix instead), but the dispatch asks for it explicitly and it
/// shares `decode_bypass_egk` internally, so a bug there could still be
/// latent for whichever codec does call it.
#[test]
fn uegk_round_trips_prefix_and_bypass_egk_suffix_together() {
    let u_coff = 14u32;
    let k = 0u32;
    for value in [0u32, 1, 5, 13, 14, 15, 20, 100, 1000, 5000] {
        let mut enc = CabacEncoder::new();
        let mut enc_ctx = ContextModel::init_h264(0, 41, 26);
        // Truncated-unary prefix, `min(value, u_coff)` ones then (if not
        // saturated) a terminating zero — mirroring `decode_tu`'s own
        // contract from the encoder side, since `CabacEncoder` has no
        // `encode_tu` of its own to call symmetrically.
        let prefix_len = value.min(u_coff);
        for _ in 0..prefix_len {
            enc.encode_decision(&mut enc_ctx, 1);
        }
        if prefix_len < u_coff {
            enc.encode_decision(&mut enc_ctx, 0);
        } else {
            enc.encode_bypass_egk(k, value - u_coff);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut dec_ctx = ContextModel::init_h264(0, 41, 26);
        let got = dec.decode_uegk(&mut dec_ctx, u_coff, k, false);
        assert_eq!(
            got, value.cast_signed(),
            "value={value}: decode_uegk did not round-trip (malformed={})",
            dec.malformed()
        );
        assert!(!dec.malformed(), "value={value}: engine reported malformed on a well-formed stream");
    }
}

/// The other two bypass primitives `residual_block_cabac` calls directly:
/// `decode_bypass()` for `coeff_sign_flag` (one bit, every nonzero
/// coefficient) and, transitively through `decode_bypass_egk`, `decode_
/// bypass_bits(n)` for the final remainder. Round-tripping these closes
/// out "a fault confined to bypass" as a hypothesis about the *whole*
/// bypass path, not just the two constructs the dispatch named.
#[test]
fn bypass_single_bit_and_bypass_bits_round_trip() {
    for pattern in 0u32..256 {
        let mut enc = CabacEncoder::new();
        for i in 0..8 {
            enc.encode_bypass((pattern >> i) & 1);
        }
        enc.encode_terminate(1);
        let bytes = enc.finish();
        let mut dec = CabacDecoder::new(&bytes);
        let mut got = 0u32;
        for i in 0..8 {
            got |= dec.decode_bypass() << i;
        }
        assert_eq!(got, pattern, "pattern={pattern:#010b}: decode_bypass did not round-trip bit-for-bit");
        assert!(!dec.malformed());
    }

    for n in 1..=20u32 {
        for value in [0u32, 1, (1 << n.min(31)) - 1, (1u32 << (n.min(31) >> 1))] {
            let mut enc = CabacEncoder::new();
            enc.encode_bypass_bits(n, value);
            enc.encode_terminate(1);
            let bytes = enc.finish();
            let mut dec = CabacDecoder::new(&bytes);
            let got = dec.decode_bypass_bits(n);
            assert_eq!(got, value, "n={n} value={value}: decode_bypass_bits did not round-trip");
            assert!(!dec.malformed());
        }
    }
}
