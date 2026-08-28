//! VP8 video decode, RFC 6386 — closes epic C-16.
//!
//! # What is here
//!
//! | Module | RFC 6386 section |
//! |---|---|
//! | [`header`] | §9 frame header, §10 segmentation |
//! | [`predict`] | §12 intra prediction (16x16, 8x8 chroma, ten 4x4 submodes) |
//! | [`tokens`] | §13 DCT coefficient decode |
//! | [`transform`] | §14 dequantisation, inverse WHT/DCT |
//! | [`loopfilter`] | §15 simple and normal deblocking filters |
//! | [`mv`] | §16.3-§16.4, §17 motion vector decode and prediction |
//! | [`interpolate`] | §18 sub-pixel motion compensation |
//! | [`framebuf`] | the three reference-frame slots (last/golden/altref) |
//! | [`decode`] | the per-macroblock orchestration and [`Decoder`](vaco_codec_core::Decoder) impl |
//!
//! The boolean entropy decoder itself lives in `vaco-codec-msac` (D-04),
//! shared with VP9; header syntax parsing for the uncompressed frame tag
//! reuses `vaco-parse-vpx` rather than re-deriving it.
//!
//! # Threading
//!
//! Not implemented. RFC 6386 §9.5's multiple DCT-coefficient token
//! partitions exist precisely to let a decoder split residual decode across
//! threads; this crate reads only the first (or only) partition. See
//! `planning/TECH-DEBT.md` for the row this leaves open against C-16d's
//! threading requirement.
//!
//! # Specification
//!
//! RFC 6386 (`rfc-6386`), "VP8 Data Format and Decoding Guide". Tables are
//! transcribed from the primary specification text (its own tree
//! definitions, probability tables and lookup tables), not from any
//! existing decoder (D7/D15) — see [`tables`]'s module doc for the two
//! places a pure numeric constant was pulled from the RFC's own reference
//! decoder appendix rather than its narrative prose, which D7 permits for
//! format-dictated data.
//!
//! # Dependencies
//!
//! `vaco-codec-msac` (bool decoder), `vaco-parse-vpx` (frame tag), `vaco-frame`/
//! `vaco-pool` (the emitted picture), `vaco-pixfmt`, `vaco-packet`,
//! `vaco-codec-core` (the `Decoder` trait and `Machine`), `vaco-limits`
//! (`Budget`-bounded allocation for every buffer sized from the
//! attacker-controlled frame header).

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "RFC 6386 identifier and constant names (B_PRED, mv_ref_tree, coeff_probs, ...) are spec vocabulary, not doc-linkable Rust items"
)]

pub mod decode;
pub mod framebuf;
pub mod header;
pub mod interpolate;
pub mod loopfilter;
pub mod mv;
pub mod predict;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use decode::{VP8_DECODER, Vp8Decoder};
