//! VP8 video encode and decode, following RFC 6386.
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
//! | [`encode`] | the all-intra encoder and its own bool writer |
//!
//! The boolean entropy decoder lives in `vaco-codec-msac`, shared with VP9;
//! uncompressed frame tags come from `vaco-parse-vpx` rather than a duplicate
//! parser.
//!
//! RFC 6386 §9.5 provides 1, 2, 4, or 8 coefficient-token partitions.
//! `decode::split_frame` reads row `r` from `r % num_partitions`. Measurements
//! across `vpxenc --token-parts={0,1,2,3}` at two resolutions and the
//! `vp80-04-partitions-*` vectors were byte-identical to `ffmpeg -c:v libvpx`.
//!
//! `-threads N` overlaps pictures, not rows within a picture. Mode, motion, and
//! token parsing remains serial because it owns entropy persistence and RFC
//! 6386 §9.7/§9.8 reference-slot updates. [`frame_task::Vp8FrameTask`] performs
//! reconstruction and loop filtering while the next frame parses. Decode order
//! is display order, so VP8 needs no reorder-buffer-driven row overlap.
//!
//! Output was byte-identical at 1, 2, 4, and 8 threads for 58 of 60 conformance
//! vectors; the two exclusions exercise the separately disclosed §9.1 display
//! rescale boundary. [`tables`] records every primary-spec transcription.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    reason = "RFC 6386 identifier and constant names (B_PRED, mv_ref_tree, coeff_probs, ...) are spec vocabulary, not doc-linkable Rust items"
)]

pub mod decode;
pub mod encode;
pub(crate) mod frame_task;
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
pub use encode::{VP8_ENCODER, Vp8Encoder};
