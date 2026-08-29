//! Apple `ProRes` decode, native, from the freely-published SMPTE RDD 36:2022
//! bitstream specification.
//!
//! `Vaco-Spec-Ref: smpte-rdd36-2022` (SMPTE Registered Disclosure Document
//! RDD 36:2022, "Apple `ProRes` Bitstream Syntax and Decoding Process",
//! <https://pub.smpte.org/doc/rdd36/20220909-pub/rdd36-2022.pdf>) — an RDD is
//! SMPTE's freely-published disclosure class (unlike a full Standard), and
//! is the sole source this crate is built from: every syntax element, VLC
//! codebook adaptation table, scan pattern, quantization formula, and pixel
//! reconstruction formula below transcribes clauses 4 through 7 directly.
//!
//! # Scope: decode only
//!
//! `planning/research/07-legal-patents-licensing.md` places `ProRes` at
//! "Decode AMBER / Encode RED" — Apple's objection (per its own support
//! page) targets *encoders*, not decode, and the project's own SS5.1 default
//! build list carries "`ProRes` decode" unconditionally. This crate therefore
//! implements [`vaco_codec_core::Decoder`] only; no `Encoder` exists here and
//! none should be added without a fresh legal read. `vaco-codec-dnxhd`, the
//! sibling crate issue #41 also names for DNxHD/VC-3, was not attempted in
//! this pass.
//!
//! # What is covered
//!
//! The full RDD 36 decode pipeline: frame/picture/slice header parsing,
//! Golomb-Rice/exponential-Golomb combination-code entropy decode (DC
//! difference, AC run/level, both adaptive per clauses 7.1.1.3/7.1.1.4), the
//! progressive and interlaced block scan patterns (clause 7.2.2), slice
//! inverse-scan (7.2.1), inverse quantization with custom or default weight
//! matrices (7.3), the classical IEEE-1180 8x8 IDCT (7.4, reused from
//! [`vaco_codec_dsp_idct::mpeg2`] rather than duplicated — D19), and pixel
//! reconstruction (7.5) for both progressive and interlaced (field-pair)
//! frames, 4:2:2 and 4:4:4 chroma sampling, and the lossless alpha channel
//! (8- and 16-bit).
//!
//! Bit depth is not a bitstream syntax element (RDD 36 §1 notes the RDD does
//! not cover the container format that would normally signal it via `FourCC`).
//! Measured against real `ffmpeg -c:v prores_ks` output across every
//! documented `FourCC`-to-profile mapping: every real-world `ProRes` profile
//! pairs `chroma_format == 2` (4:2:2) with 10-bit samples and
//! `chroma_format == 3` (4:4:4) with 12-bit samples, with no counterexample
//! in any profile Apple has ever shipped, so [`decoder`] derives bit depth
//! from `chroma_format` alone rather than needing the `FourCC` this crate's
//! `Decoder` interface has no channel to receive anyway.
//!
//! # What is cut
//!
//! - **Interlaced fields**: the syntax and pixel-interleave rules (clause
//!   7.5.3) are implemented, but no real interlaced `ProRes` fixture was
//!   available to verify against in this session; progressive frames are the
//!   verified path (see `tests/oracle.rs`).
//! - **`endOfData`/`endOfStructure` bitstream-version escape hatches**
//!   (clause 5, "Bitstream Versions") beyond version 1: this crate refuses
//!   an unrecognised `bitstream_version` rather than guessing at future
//!   syntax, exactly as clause 6.4 requires.
//!
//! # Dependencies
//!
//! [`vaco_codec_dsp_idct::mpeg2`] for the IDCT (same IEEE-1180 accuracy
//! contract RDD 36 Annex A specifies, so reusing it is not just D19 hygiene —
//! it is *already* the right transform). [`vaco_bitstream`] for the MSB-first
//! bit reader.

#![forbid(unsafe_code)]

mod coeff;
mod decoder;
mod golomb;
mod header;
mod scan;

pub use decoder::{DECODER_PRORES, ProresDecoder};
