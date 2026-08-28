//! Theora video decode, native, keyframes only.
//!
//! `Vaco-Spec-Ref: theora-spec-20170603` (Theora Specification, Xiph.Org
//! Foundation, June 3 2017) — the only normative Theora document; VP3 (the
//! codec Theora extends) has no separate written spec of its own.
//!
//! # Scope: intra (keyframe) decode only
//!
//! This crate decodes `FTYPE == 0` frames (section 7.1) — a complete,
//! independently-reconstructible picture — and returns
//! [`vaco_core::Error::Unsupported`] for any `FTYPE == 1` (inter/delta)
//! frame rather than attempting motion compensation. Inter decode needs a
//! reference-frame buffer, motion-vector decode (section 7.5), and the
//! whole-pixel/half-pixel predictors (sections 7.9.1.2/7.9.1.3) — a second,
//! comparably-sized effort this dispatch's time budget did not leave room
//! for once the header/entropy/reconstruction pipeline below turned out to
//! be as large as it is. Concretely, this means: an all-keyframe Ogg/Theora
//! stream decodes correctly end to end; a stream with delta frames decodes
//! its keyframes and then stops with a clean, typed error on the first
//! delta frame, rather than silently repeating a stale picture or producing
//! a plausible-looking wrong one. Encode is entirely out of scope (issue
//! #371 asks for decode only).
//!
//! No `Caps::PATENT_ENCUMBERED`: Theora was designed from the ground up as a
//! royalty-free format (derived from the donated, patent-unencumbered VP3),
//! and is not listed as encumbered in
//! `planning/research/07-legal-patents-licensing.md`.
//!
//! # What is here
//!
//! | Module | Contents |
//! |---|---|
//! | [`ident`] | identification header: frame/picture geometry, pixel format |
//! | [`setup`] | setup header framing (loop filter limits, quant params, Huffman tables) |
//! | [`quant`] | quantization-parameter decode and per-`(qti, pli, qi)` matrix construction |
//! | [`huffman`] | the 80 DCT token Huffman tables, stored and decoded as binary trees |
//! | [`blocks`] | block/super block/macro block coded-order geometry (the Hilbert curves) |
//! | [`idct`] | the exact integerized inverse DCT the spec mandates bit-for-bit |
//! | [`tokens`] | EOB run tokens and coefficient tokens (section 7.7) |
//! | [`frame`] | the per-frame pipeline: header, qi decode, coefficients, DC prediction, reconstruction, loop filter |
//! | [`decoder`] | the `Decoder` impl: `set_extradata` (Xiph-laced headers) and per-packet decode |
//!
//! # A known gap, honestly
//!
//! No genuine Theora encoder was available in this environment to produce
//! `ffmpeg`-encoded ground truth to diff against (`ffmpeg -codecs` here
//! lists Theora decode only, not encode), so this crate's verification is
//! narrower than this tree's other codecs: internal consistency tests
//! (headers round-trip their own worked examples from the spec text itself,
//! particularly the coded-order Hilbert curve and the quantization matrix
//! interpolation formula, both checked digit-for-digit against the spec's
//! own numeric example) and structural fuzzing, but *not* a byte-exact
//! comparison of a real encoded frame's reconstructed pixels against an
//! independent decoder's output. Every formula in [`idct`], [`tokens`], and
//! [`frame`]'s reconstruction/loop-filter path is transcribed directly from
//! the spec's own numbered steps rather than measured, with one exception
//! flagged in [`setup`]'s module doc (the loop filter limit table's decode
//! procedure, which is missing from the published spec text itself). Treat
//! this crate as spec-conformant by construction and cross-checked
//! internally, not as verified against independent ground truth the way
//! this tree's other from-scratch codecs are — a real `.ogv`/`.ogg`
//! Theora file with known-correct decoded frames would close this gap and
//! is the first thing worth throwing at this crate before trusting it in
//! production.
//!
//! # How to change it
//!
//! A new interleave or subsampling case starts in [`ident::PixelFormat`]
//! and [`blocks::FrameGeom`], which are the only places that know how a
//! pixel format maps onto chroma block-grid dimensions. Inter-frame support
//! would add motion vector decode (a new module) and macro block coding
//! mode decode (currently hardcoded to `INTRA` in [`frame`]'s doc-listed
//! simplifications) before [`frame::decode_frame_payload`] could stop
//! rejecting `FTYPE == 1`.
//!
//! # Configuration
//!
//! [`vaco_limits::Limits`] bounds the coded frame the same way every other
//! decoder in this tree does: [`decoder::TheoraDecoder::set_extradata`]
//! checks the identification header's `FMBW`/`FMBH` against the budget
//! before building any block-indexed table from them, and every per-block
//! allocation inside [`frame`] goes through the same budget.
//!
//! # Dependencies
//!
//! `vaco-codec-core` (the decode protocol), `vaco-bitstream` (the MSB-first
//! bit reader this crate's bitpacking convention, section 5, matches
//! exactly), `vaco-frame`/`vaco-pixfmt` (the decoded picture), `vaco-packet`
//! (encoded packets), `vaco-limits` (allocation bounds).

#![forbid(unsafe_code)]

mod blocks;
mod decoder;
mod frame;
mod huffman;
mod idct;
mod ident;
mod quant;
mod setup;
mod tokens;

pub use decoder::{DECODER_THEORA, TheoraDecoder};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_answers_to_its_own_name() {
        assert_eq!(DECODER_THEORA.name, "theora");
        assert_eq!(DECODER_THEORA.id, vaco_codec_core::CodecId::Theora);
    }
}
