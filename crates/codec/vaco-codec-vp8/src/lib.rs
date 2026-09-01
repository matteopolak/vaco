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
//! | [`encode`] | the all-intra skeleton encoder and its own bool writer (issue #302, C-17a) |
//!
//! The boolean entropy decoder itself lives in `vaco-codec-msac` (D-04),
//! shared with VP9; header syntax parsing for the uncompressed frame tag
//! reuses `vaco-parse-vpx` rather than re-deriving it.
//!
//! # Threading (issue #301, `C-16d`)
//!
//! RFC 6386 §9.5's multiple DCT-coefficient token partitions (1, 2, 4 or 8,
//! `log2_nbr_of_dct_partitions` in the header) exist precisely to let rows
//! be decoded in parallel: `decode::split_frame` parses the partition-size
//! table and reads macroblock row `r`'s tokens from partition
//! `r % num_partitions` (`decode::split_token_partitions`), which is the
//! part multi-partition streams actually need to decode *correctly* —
//! previously every row read from partition 0 regardless of the header's
//! count, corrupting anything past the first row of a multi-partition
//! stream. Verified against `vpxenc --token-parts={0,1,2,3}` (1/2/4/8
//! partitions) at two resolutions, decoded output byte-identical to
//! `ffmpeg -c:v libvpx` in every case, and again against the real
//! `vp80-04-partitions-*` conformance vectors (see `tests/conformance.rs`).
//! **This is not the axis `-threads N` uses** — see below.
//!
//! `-threads N` overlaps *pictures*, not one picture's own macroblock rows.
//! `decode::split_frame`'s per-macroblock parse (mode/motion-vector/token
//! decode) stays a single sequential bool-decoder walk over the first
//! partition, on the
//! caller's own thread, in decode order — that half is cheap and is where
//! the reference semantics (entropy persistence, RFC 6386 §9.7/§9.8's
//! reference-slot bookkeeping) live, exactly the case
//! `vaco_codec_core::threading`'s module doc argues should stay serial. Once
//! a frame's tokens are fully parsed, its own reconstruction and loop filter
//! (`frame_task::Vp8FrameTask`) run on a worker thread while the *next*
//! frame's own token decode proceeds on the caller's — VP8 needs nothing
//! more elaborate because, unlike a codec with B-frames, decode order is
//! always display order, so there is no reorder buffer whose depth would
//! otherwise force finer-grained overlap. See [`frame_task`]'s own module
//! doc for why picture granularity was chosen over the row-banded design
//! `vaco-codec-h264`/`vaco-codec-hevc` use, and for the measured cost of the
//! one deliberate trade that design makes (materialising a whole reference
//! picture before a task can read it, at every thread count).
//!
//! Verified byte-identical at `-threads` 1/2/4/8 against the same 58/60 VP8
//! test-vector corpus `tests/conformance.rs` already checks byte-exactness
//! with — see that module's doc for the two vectors excluded for an
//! unrelated, disclosed reason (display-rescale, RFC 6386 §9.1).
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
