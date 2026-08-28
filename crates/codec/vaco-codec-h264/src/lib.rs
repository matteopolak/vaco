//! H.264/AVC entropy decoding (T3-01d/#417 CAVLC, T3-01e/#418 CABAC), and
//! the far side of a line the previous dispatch drew rather than crossed.
//!
//! # Where the parse/decode line falls, and why this crate is gated
//!
//! `vaco-parse-h264` stays in the default build: NAL/RBSP framing, SPS/PPS,
//! slice headers, POC derivation, SEI, Annex-B/avcC conversion — none of it
//! reconstructs a sample, so none of it is `patent-encumbered-h264-decode`'s
//! concern (D4). This crate is the far side of that line: entropy decoding
//! *is* part of reconstructing a sample (its output is coefficient values
//! that feed a transform), so it is `encumbered = true` / `default = false`
//! from the moment it exists, following the precedent `vaco-codec-aac` set
//! (the first `encumbered = true` component in the tree) rather than waiting
//! until a full decode exists to gate it — `vaco-codec-aac`'s own module doc
//! explains why registering an honestly-partial gated decoder beats leaving
//! the component undiscoverable until it is finished.
//!
//! # What this dispatch implements, and the shape both entropy modes share
//!
//! [`cavlc::residual_block_cavlc`] and [`cabac_residual::residual_block_cabac`]
//! are each the residual-coefficient half of their respective
//! `residual_block()` process — clause 7.3.5.3.1-2 for CAVLC, 7.3.5.3.3 for
//! CABAC — parameterised by exactly what a caller *outside* the macroblock
//! layer can supply (`nC` for CAVLC, a caller-derived `coded_block_flag` for
//! CABAC, `max_num_coeff`/`ctxBlockCat` for both), and nothing a caller needs
//! neighbouring-macroblock state to derive. That is the same separation
//! `vaco-codec-msac` draws around VP8/VP9's bool decoders, applied to H.264:
//! this crate does not know what a macroblock is, and #419 onward (the
//! macroblock layer: `mb_type`, prediction, transform reconstruction) is
//! explicitly not this dispatch's scope.
//!
//! [`H264Decoder::send_packet`] locates a slice header far enough to resolve
//! `entropy_coding_mode_flag` and then returns
//! [`vaco_core::Error::Unsupported`], honestly, the same choice
//! `vaco-codec-aac` made for the gap between "configuration resolved" and
//! "PCM produced".
//!
//! # Verification: what could be checked here, and what could not
//!
//! Both entropy functions are exercised against hand-built fixtures derived
//! directly from this crate's own transcribed tables (a test-only VLC
//! encoder for CAVLC, `vaco-codec-cabac`'s own encoder for CABAC — the same
//! justification that crate gives for having one at all: an arithmetic
//! coder cannot be tested against a hand-written bit pattern any other way),
//! with exact-bit-length assertions written independently of the tables
//! themselves as the direct mitigation for today's `CODED_BLOCK_PATTERN`
//! lesson (a self-consistent table can still have one entry's length wrong).
//!
//! **What this could not be checked against**: a real slice's exact bit
//! consumption end-to-end. `SliceHeader::parse` correctly lands a
//! `BitReader` at the first bit of `slice_data()` on real `ffmpeg`-encoded
//! streams (already reference-tested by `vaco-parse-h264`), but
//! `residual_block_cavlc`/`residual_block_cabac` are only ever *one* of many
//! syntax elements a real slice's macroblock loop reads in sequence —
//! `mb_type` selects whether residual is even present, and CABAC in
//! particular is one continuous arithmetic stream where every preceding
//! bin's context update affects every later one. Driving either function
//! against real encoder output for a real "consumed exactly the declared
//! bits" measurement needs the macroblock loop that decides *when* to call
//! them and with what `nC`/`ctxBlockCat` — which is #419's job, not this
//! one's. Claiming that measurement now would be exactly the kind of
//! specification-only-dressed-as-verified gap the previous dispatch was
//! asked to stop making. What is true today is narrower and stated
//! precisely: both functions are specification-and-self-consistency tested,
//! plus bit-exact against their own hand-built fixtures.

#![forbid(unsafe_code)]

pub mod cabac_residual;
pub mod cavlc;
mod cavlc_tables;
pub mod decoder;

pub use cabac_residual::{CabacResidual, ContextCategory, ContextSet, residual_block_cabac};
pub use cavlc::{BlockKind, CavlcResidual, residual_block_cavlc};
pub use decoder::H264Decoder;

/// The registry descriptor for this crate's decoder.
///
/// `caps: Caps::PATENT_ENCUMBERED` is the code-level half of D4's gating,
/// mirroring `vaco-codec-aac::DECODER_AAC` exactly — see this module's own
/// doc, and that crate's, for the other half (the `vaco-component.toml`
/// fragment's `encumbered = true` / `default = false` pair, which is what
/// `cargo xtask patent-gate` actually checks).
pub const DECODER_H264: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "h264",
    long_name: "H.264 / AVC / MPEG-4 Part 10",
    id: ::vaco_codec_core::CodecId::H264,
    media_type: ::vaco_core::MediaType::Video,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED,
    supported_rates: &[],
    make: |limits| ::std::boxed::Box::new(decoder::H264Decoder::new(limits)),
};
