//! `vaco-codec-h264`'s residual-block entropy decode against arbitrary
//! bytes, both entropy modes.
//!
//! Neither [`residual_block_cavlc`] nor [`residual_block_cabac`] is
//! reachable from a real slice yet (the macroblock layer, #419+, is not
//! implemented — see the crate's own module doc), so there is no
//! `H264Decoder::send_packet` path exercising them today. This target calls
//! both directly instead, which is exactly the shape the dispatch that
//! built this crate asked for: an arithmetic/VLC decoder taking
//! attacker-controlled `nC`/`ctxBlockCat`/`max_num_coeff` alongside the bit
//! data itself is a classic `slow-unit-` source if any loop's bound is
//! merely believed rather than measured (CAVLC's `level_prefix` unary run
//! and CABAC's `coeff_abs_level_minus1` truncated-unary prefix and `EGk`
//! suffix are exactly that shape) — this target is the check that every
//! bound in both functions is real.
//!
//! First byte selects entropy mode and derived parameters (`nC`, block
//! kind/category, `max_num_coeff`, `slice_qp`); the rest is the bitstream
//! CAVLC reads from, or the CABAC byte buffer `CabacDecoder::new` wraps.
//! Every input, well-formed or not, must:
//!
//! * Never panic (`#![forbid(unsafe_code)]` throughout — nothing here can
//!   segfault, but a wrong array-index bound or an integer overflow in the
//!   fuzzing profile's overflow-checked build is still a `Result::Err`
//!   turned into a panic if any `?` were replaced with an `.unwrap()`, which
//!   this target itself avoids by matching on the `Result` explicitly).
//! * Never hang — every loop in both functions has a ceiling derived from
//!   the input's own declared shape (`max_num_coeff`, `U_COFF`, the VLC
//!   tables' own maximum code length), never from the bitstream continuing
//!   to supply bits forever.
//!
//! fuzz-crate: vaco-codec-h264

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_bitstream::BitReader;
use vaco_codec_cabac::CabacDecoder;
use vaco_codec_h264::cabac_residual::{ContextCategory, ContextSet, residual_block_cabac};
use vaco_codec_h264::cavlc::residual_block_cavlc;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };

    let use_cabac = selector & 1 != 0;
    let nc: i32 = match (selector >> 1) & 0b111 {
        0 => -1,
        1 => -2,
        n => i32::from(n) - 2, // 0..=5, covering every VLC family plus the fixed-length one
    };
    let max_num_coeff: u8 = match (selector >> 4) & 0b11 {
        0 => 4,
        1 => 8,
        2 => 15,
        _ => 16,
    };
    let category = match (selector >> 6) & 0b11 {
        0 => ContextCategory::LumaDc,
        1 => ContextCategory::LumaAc,
        2 => ContextCategory::Luma4x4,
        _ => ContextCategory::ChromaAc,
    };
    let slice_qp = (i32::from(selector) % 52) as i8;

    let mut budget = Budget::new(Limits::default());

    if use_cabac {
        let mut dec = CabacDecoder::new(rest);
        let mut ctx = ContextSet::new(slice_qp);
        // Every input is well-formed at the type level; a malformed one is
        // expected to surface through `CabacDecoder::malformed()` or an
        // `Err`, never a panic or a hang.
        let _ = residual_block_cabac(&mut dec, &mut ctx, category, max_num_coeff.max(1), &mut budget);
        let _ = dec.malformed();
    } else {
        let mut r = BitReader::new(rest);
        let _ = residual_block_cavlc(&mut r, nc, max_num_coeff.max(1), &mut budget);
    }
});
