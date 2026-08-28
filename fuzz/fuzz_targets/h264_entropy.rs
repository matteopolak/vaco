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
//! First two bytes select entropy mode and derived parameters (`nC`, block
//! kind/category, `cabac_init_idc`, `max_num_coeff`, `slice_qp`); the rest
//! is the bitstream CAVLC reads from, or the CABAC byte buffer
//! `CabacDecoder::new` wraps. `ContextCategory` needs 5 values and
//! `CabacInit` needs 4, which together no longer fit in the bits left over
//! in a single `u8` selector alongside entropy mode/`nC`/`max_num_coeff` —
//! an earlier version of this target packed `category` into the same two
//! bits already spent on `init`, which silently made `ChromaDc` (added
//! alongside chroma DC's `coded_block_flag` support) unreachable. A second
//! selector byte fixes that.
//!
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
use vaco_codec_h264::cabac_residual::{CabacInit, ContextCategory, ContextSet, residual_block_cabac};
use vaco_codec_h264::cavlc::residual_block_cavlc;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let Some((&selector0, rest)) = data.split_first() else {
        return;
    };
    let Some((&selector1, rest)) = rest.split_first() else {
        return;
    };

    let use_cabac = selector0 & 1 != 0;
    let nc: i32 = match (selector0 >> 1) & 0b111 {
        0 => -1,
        1 => -2,
        n => i32::from(n) - 2, // 0..=5, covering every VLC family plus the fixed-length one
    };
    let max_num_coeff: u8 = match (selector0 >> 4) & 0b11 {
        0 => 4,
        1 => 8,
        2 => 15,
        _ => 16,
    };
    let category = match selector1 & 0b111 {
        0 => ContextCategory::LumaDc,
        1 => ContextCategory::LumaAc,
        2 => ContextCategory::Luma4x4,
        3 => ContextCategory::ChromaDc,
        _ => ContextCategory::ChromaAc,
    };
    let slice_qp = (i32::from(selector0) % 52) as i8;

    let mut budget = Budget::new(Limits::default());

    if use_cabac {
        let mut dec = CabacDecoder::new(rest);
        let init = match (selector1 >> 3) & 0b11 {
            0 => CabacInit::IorSi,
            n => CabacInit::PSpB(n - 1),
        };
        let mut ctx = ContextSet::new(category, slice_qp, init);
        // Every input is well-formed at the type level; a malformed one is
        // expected to surface through `CabacDecoder::malformed()` or an
        // `Err`, never a panic or a hang.
        let _ = residual_block_cabac(&mut dec, &mut ctx, max_num_coeff.max(1), &mut budget);
        let _ = dec.malformed();
    } else {
        let mut r = BitReader::new(rest);
        let _ = residual_block_cavlc(&mut r, nc, max_num_coeff.max(1), &mut budget);
    }
});
