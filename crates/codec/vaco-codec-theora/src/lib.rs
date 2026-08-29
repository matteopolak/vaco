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
//! # Verified against a real file, byte-exact per plane
//!
//! No genuine Theora *encoder* was available in this environment
//! (`ffmpeg -codecs` here lists Theora decode only), so this crate could
//! not generate its own ground truth the way this tree's other from-scratch
//! codecs do. It does not need to: `ffmpeg` decodes Theora, and real Theora
//! content is freely available — `ffmpeg`'s own FATE test suite carries
//! two Ogg/Theora fixtures (`https://fate-suite.ffmpeg.org/ogg/`). This
//! crate's `tests/oracle.rs` decodes one of them (`bear.ogv`, 320x180,
//! encoded by an old `ffmpeg`/`libtheora`) and compares every keyframe
//! against `ffmpeg -i bear.ogv -f rawvideo -pix_fmt yuv420p`'s own decode,
//! **Y, U and V checked separately** — an aggregate or luma-only check can
//! hide a chroma-only bug entirely, which is exactly what happened during
//! this verification (see below) and is why the check stays split.
//!
//! Result: **byte-exact on every plane**, at all three of `bear.ogv`'s
//! keyframes. A second real file (`ogg/empty_theora_packets.ogv`, 320x240,
//! a genuinely different encoder — native `libtheora`, not `ffmpeg`'s own)
//! gave the same result at its own nine keyframes. Getting here found and
//! fixed two real, structural bugs — full account in `tests/oracle.rs`'s
//! module doc — neither of them in the DCT/entropy/reconstruction pipeline
//! itself (unsurprising in hindsight, since luma was already byte-exact
//! before either fix): `vaco-demux-ogg` never packed Theora's comment and
//! setup headers into `extradata` at all (no real Ogg/Theora file could
//! reach this decoder through that container until that was fixed), and
//! this crate's own chroma picture-region crop used the wrong height,
//! corrupting the last several rows of every chroma plane while leaving
//! luma untouched. A third bug (the loop filter limit table's reconstructed
//! decode procedure using the wrong prefix convention, see [`setup`]'s
//! module doc) surfaced as an immediate, loud parse failure on the first
//! real file tried, rather than a silent pixel error.
//!
//! What this does *not* cover: inter-frame decode (out of scope by design,
//! see below), and the odd-offset/non-block-aligned picture-region cropping
//! cases [`frame::crop_plane`]'s doc already flags as unimplemented — both
//! `bear.ogv` and `empty_theora_packets.ogv` crop from a taller coded frame
//! down to a non-multiple-of-16 picture height, which already exercises the
//! *common* even-offset crop path, but an odd `PICX`/`PICY` has not been
//! seen on a real file. Two real files, three keyframes each on average,
//! is a real but modest sample — a decoder this well fuzz-tested and this
//! cleanly verified on what was tried is a reasonable thing to register,
//! not a proof against every possible real-world stream.
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
