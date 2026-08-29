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
//! be decoded in parallel: `decode_frame` parses the partition-size table
//! and reads macroblock row `r`'s tokens from partition `r % num_partitions`
//! (`decode::split_token_partitions`), which is the part multi-partition
//! streams actually need to decode *correctly* — previously every row read
//! from partition 0 regardless of the header's count, corrupting anything
//! past the first row of a multi-partition stream. Verified against
//! `vpxenc --token-parts={0,1,2,3}` (1/2/4/8 partitions) at two resolutions,
//! decoded output byte-identical to `ffmpeg -c:v libvpx` in every case, and
//! again now against the real `vp80-04-partitions-*` conformance vectors
//! (see `tests/conformance.rs`).
//!
//! **Actually running those per-partition decodes on separate OS threads is
//! not done, and here is the concrete reason rather than a vague one.**
//! `decode_macroblock`'s *mode/motion-vector* record for every macroblock in
//! the frame comes from one sequential bool-decoder walk over the *first*
//! partition (RFC 6386 §9.5 only splits the *token* partitions; the mode
//! stream is one bitstream for the whole frame, decoded strictly in raster
//! order because each macroblock's MV prediction context reads its
//! already-decoded above/left/above-left neighbours). Splitting only the
//! *reconstruction* half (token decode + IDCT + pixel write) across threads
//! is possible in principle — RFC 6386 §15.1 confirms the loop filter, and
//! by extension intra prediction's "already-constructed" neighbour pixels,
//! only ever depend on the macroblock *above* being fully reconstructed, not
//! on anything to its right or in a later row — but implementing it safely
//! (`#![forbid(unsafe_code)]`) needs every macroblock row to become a
//! separately-owned, ownership-transferred unit (the same technique
//! `vaco-codec-core::picture`'s `OnceLock`-per-band publish/wait model uses
//! for *cross-frame* pipelining), because two threads writing disjoint rows
//! of what is today one plain `Vec<u8>`-backed [`framebuf::Plane`] is
//! exactly the aliasing situation that model exists to avoid.
//! [`vaco_codec_core::threading::SliceThreadedDecoder`]'s actual shape does
//! not fit this directly either: `PictureWriter::split_bands_mut` hands out
//! *disjoint, non-communicating* band ranges to concurrent jobs (its own doc
//! comment: "each job holds a disjoint band range"), with no mechanism for
//! one job to read a row another job is still writing — appropriate for
//! genuinely independent tiles/slices, not for VP8's row-above dependency.
//! Wiring this up for real would mean either restructuring `Plane` into
//! per-row-published, ownership-transferred storage (a substantial rewrite
//! of every reconstruction call site in `decode.rs`, all of which currently
//! read and write an already-allocated plane by absolute pixel coordinate)
//! or extending `vaco-codec-core`'s threading primitives to support a
//! same-picture multi-writer wavefront — the latter is out of this crate's
//! ownership. Given the risk of a large rewrite to a decoder that is
//! currently verified byte-exact against `ffmpeg` on 58 of 60 real VP8 test
//! vectors (see `tests/conformance.rs`), that rewrite was not attempted this
//! pass. A single-threaded decode is byte-identical to any future
//! multi-threaded one by construction (there is only one implementation),
//! which is a vacuous, not a demonstrated, form of the "same output at any
//! thread count" property.
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
