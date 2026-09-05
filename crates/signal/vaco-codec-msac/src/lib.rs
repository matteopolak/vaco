//! Entropy engines shared by AV1, VP8 and VP9.
//!
//! VP8 and VP9 code their entire compressed payload (bar the uncompressed
//! frame header and, for VP9, the partition-size prefix) through a boolean
//! arithmetic coder: one bool at a time, at an 8-bit probability, optionally
//! shaped into a small alphabet by a binary tree. VP8's and VP9's engines
//! are numerically distinct — different initial fill, different exit
//! padding rule — which is why this crate carries two decoders rather than
//! one generic one, but the tree-walk on top of either is identical, so
//! [`tree::read_tree`] is written once and used by both.
//!
//! # AV1 multi-symbol decoding
//!
//! AV1's true multi-symbol range coder (`msac` in `libaom`'s own naming,
//! which lent this crate its name) decodes an N-ary symbol in one coding
//! step via a cumulative-distribution table. VP9 has no such thing — every
//! VP9 syntax element with more than two outcomes is a binary tree over the
//! same [`vp9::BoolDecoder`] this crate provides, confirmed against the VP9
//! Bitstream & Decoding Process Specification v0.6 §9.3. [`av1::SymbolDecoder`]
//! implements AV1 §8.2, including adaptive CDF updates, and is consumed by
//! `vaco-codec-av1` through its compatibility re-export.
//!
//! # Error model
//!
//! Neither decoder returns `Result` from a per-bool read. An
//! under-length partition has nothing sensible to fail *onto* mid-symbol —
//! the tree-walk must still terminate at a leaf — so both engines mirror
//! `vaco-codec-cabac`'s convention: reads past the end of the supplied
//! buffer return zero bits (VP8) or `newBit = 0` (VP9, which is what the
//! specification's own §9.2.2 prescribes for the equivalent case), and the
//! caller checks [`vp8::BoolDecoder::overrun`] / [`vp9::BoolDecoder::overrun`]
//! once per syntax structure rather than after every bin.
//! AV1 likewise pads reads; [`av1::SymbolDecoder::overrun`] reports exhaustion
//! beyond the fourteen padding bits permitted by AV1 §8.2.4.
//!
//! # Specification
//!
//! RFC 6386 (`rfc-6386`) §7 for VP8; the VP9 Bitstream & Decoding Process
//! Specification v0.6 (`vp9-bitstream-spec-v0.6`) §9.2-9.3 for VP9; AV1
//! specification (`aom-av1-spec`) §8.2 for AV1. Tables
//! are format-dictated constants (tree shapes, not expression) and are
//! transcribed from the primary specification text, not from any existing
//! decoder (D7).
//!
//! # Dependencies
//!
//! `vaco-core` for `Result`/`Error`, `vaco-bitstream` for AV1's `BitReader`.
//! VP8 and VP9 manage their own bit positions and input padding.
//! No external runtime dependencies.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "RFC 6386/VP9-spec identifier and constant names (DCT_0, mv_ref_tree, ...) are spec vocabulary, not doc-linkable Rust items"
)]

pub mod av1;
pub mod tree;
pub mod vp8;
pub mod vp9;

pub use av1::SymbolDecoder as Av1SymbolDecoder;
pub use tree::{Tree, read_tree, read_tree_at, write_tree, write_tree_at};
pub use vp8::BoolDecoder as Vp8BoolDecoder;
pub use vp9::{BoolDecoder as Vp9BoolDecoder, BoolEncoder as Vp9BoolEncoder};
