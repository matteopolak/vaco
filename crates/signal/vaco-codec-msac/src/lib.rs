//! Boolean-entropy engines shared by VP8 and VP9 — D-04.
//!
//! Both codecs code their entire compressed payload (bar the uncompressed
//! frame header and, for VP9, the partition-size prefix) through a boolean
//! arithmetic coder: one bool at a time, at an 8-bit probability, optionally
//! shaped into a small alphabet by a binary tree. VP8's and VP9's engines
//! are numerically distinct — different initial fill, different exit
//! padding rule — which is why this crate carries two decoders rather than
//! one generic one, but the tree-walk on top of either is identical, so
//! [`tree::read_tree`] is written once and used by both.
//!
//! # What is *not* here
//!
//! AV1's true multi-symbol range coder (`msac` in `libaom`'s own naming,
//! which lent this crate its name) decodes an N-ary symbol in one coding
//! step via a cumulative-distribution table. VP9 has no such thing — every
//! VP9 syntax element with more than two outcomes is a binary tree over the
//! same [`vp9::BoolDecoder`] this crate provides, confirmed against the VP9
//! Bitstream & Decoding Process Specification v0.6 §9.3. AV1 decode is out
//! of scope for the package this crate was built for, so its multi-symbol
//! engine is not implemented here.
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
//!
//! # Specification
//!
//! RFC 6386 (`rfc-6386`) §7 for VP8; the VP9 Bitstream & Decoding Process
//! Specification v0.6 (`vp9-bitstream-spec-v0.6`) §9.2-9.3 for VP9. Tables
//! are format-dictated constants (tree shapes, not expression) and are
//! transcribed from the primary specification text, not from any existing
//! decoder (D7).
//!
//! # Dependencies
//!
//! `vaco-core` for `Result`/`Error`, `vaco-bitstream` for nothing beyond
//! byte-slice access (both engines manage their own bit position, since
//! neither the VP8 nor the VP9 bool decoder's shift-in rule matches
//! `BitReader`'s big-endian bit cursor closely enough to reuse it directly).
//! No external runtime dependencies.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "RFC 6386/VP9-spec identifier and constant names (DCT_0, mv_ref_tree, ...) are spec vocabulary, not doc-linkable Rust items"
)]

pub mod tree;
pub mod vp8;
pub mod vp9;

pub use tree::{Tree, read_tree, read_tree_at};
pub use vp8::BoolDecoder as Vp8BoolDecoder;
pub use vp9::BoolDecoder as Vp9BoolDecoder;
