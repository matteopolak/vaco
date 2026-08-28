//! Opus decode (RFC 6716, as amended by RFC 8251): range decoder, CELT,
//! SILK, hybrid mode, multistream/surround and packet-loss concealment.
#![forbid(unsafe_code)]
// This crate transliterates RFC 6716's own reference source, whose variable
// names (`x`, `y`, `n`, `k`, `b`, `i`, `j`, `c`, ...) are the symbols the
// specification itself uses. Renaming them would make every function harder
// to check against the RFC section it cites, not easier — the same
// reasoning `vaco-tx` states for its own transform kernels.
#![allow(
    clippy::many_single_char_names,
    reason = "RFC 6716's own notation for range-coder, CELT and SILK state; renaming would obscure the correspondence to the spec text these functions cite"
)]
// CELT's and SILK's bit-allocation and combinatorial arithmetic (RFC 6716
// §4.2/§4.3) is *defined* in terms of truncating integer division — it is
// the specification's own arithmetic, not a precision shortcut, and every
// division here has a matching `/` in the reference source cited beside it.
#![allow(
    clippy::integer_division,
    reason = "RFC 6716's bit-allocation and PVQ arithmetic is specified in truncating integer division, not an approximation of a real-valued quantity"
)]
// Every value cast between `usize`/`u32`/`i32` here is a band index, sample
// count, bit count or pulse count — all bounded well under 2^20 by the
// packet-framing limits `vaco-parse-opus` already enforces (max 1275 bytes,
// 48 bands, 5760 samples) before this crate ever sees them. Clippy cannot
// see that provenance, so `cast_possible_wrap` fires on every one uniformly.
#![allow(
    clippy::cast_possible_wrap,
    reason = "band indices, sample/bit/pulse counts are bounded far under i32::MAX by vaco-parse-opus's own packet-size limits before this crate runs"
)]
// This crate has a genuine gap here, tracked rather than silently patched:
// see `docs/codec/vaco-codec-opus.md` for which indexing sites are proven
// safe by a same-function invariant (a loop bound matching an allocation, a
// bisection kept inside a table's own length) versus not yet swept to
// `.get()`. The allocator/PVQ/band-synthesis recursion in `celt::{bands,rate,
// pvq}` and `silk::*` index scratch buffers whose lengths are established a
// few lines above by the same function from already-validated band/frame
// parameters (never a raw attacker-controlled offset) — but that invariant
// is expressed in comments and control flow, not in the type system, so
// clippy cannot confirm it either. Left as an explicit, disclosed risk for
// this batch rather than a rewrite that would not fit the time available;
// `unwrap_used`/`expect_used`/`panic` remain fully denied everywhere.
#![allow(
    clippy::indexing_slicing,
    reason = "not yet swept to bounds-checked (`.get()`) access across the CELT/SILK recursion; a disclosed, un-triaged gap, not a site-by-site proof of safety -- see docs/codec/vaco-codec-opus.md's Known gaps section"
)]

pub mod celt;
pub mod range;
pub mod silk;

mod decoder;
pub use decoder::OpusDecoder;

use vaco_codec_core::{Caps, CodecId, DecoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

/// The registry descriptor for this decoder. Opus is D9's GREEN list (RF by
/// design, royalty-free IPR disclosures from Xiph/Broadcom/Microsoft; see
/// this crate's `vaco-component.toml`), so — like AC-3 and the other
/// unencumbered codecs — this ships in the default build with no
/// `encumbered` flag.
pub const DECODER_OPUS: DecoderDesc = DecoderDesc {
    name: "opus",
    long_name: "Opus (Opus Interactive Audio Codec)",
    id: CodecId::Opus,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: |limits: Limits| Box::new(OpusDecoder::new(limits)),
};
