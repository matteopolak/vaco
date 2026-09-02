//! AV1 intra-only video decode: OBU layer through reconstructed pixels for a
//! key frame or intra-only frame, one tile or several.
//!
//! # Scope
//!
//! This crate closes the "decode a real intra AV1 keyframe end to end"
//! batch: OBU framing and sequence header (built on `vaco-parse-av1`, not
//! reimplemented — see below), the frame header's full intra path (tile
//! info, quantization, segmentation, delta-q/lf, the loop-filter/CDEF/
//! restoration/film-grain *syntax* so later bytes stay aligned, tx mode),
//! the §8.2 symbol decoder and its CDF adaptation rule, the tile/superblock
//! partition tree and mode-info walk, coefficient decoding, the inverse
//! transforms, and intra prediction including CFL.
//!
//! Out of scope, left for later work: inter prediction, deblocking/CDEF/
//! superres/loop restoration *application* (their header syntax is parsed
//! so the bitstream stays aligned, but the filters do not run, so output is
//! pre-in-loop-filter), film grain synthesis, frame threading/DPB, and
//! Argon conformance. An inter frame's header is rejected with
//! [`vaco_core::Error::Unsupported`] rather than guessed at.
//!
//! # Division of labour with `vaco-parse-av1`
//!
//! `vaco-parse-av1` already implements OBU framing, `sequence_header_obu()`
//! in full, and `AV1CodecConfigurationRecord` (`av1C`); this crate depends
//! on it for all three rather than reimplementing them (D14.1 permits a
//! `vaco-codec-*` crate depending on a `vaco-parse-*` crate, not the
//! reverse). Its own `frame_header` module covers only the intra path and
//! deliberately does not extend to `frame_size_with_refs()`.
//!
//! # Module map
//!
//! | Module | Contents |
//! |---|---|
//! | [`symbol`] | §8.2 symbol decoder, byte-for-byte off the specification text |
//! | [`cdf`] | Per-tile CDF context, adapted in place by [`symbol::SymbolDecoder::read_symbol`] |
//! | [`tables`] | Block/transform-size, scan-order and quantizer tables (§9.2/§9.3) |
//! | [`frame_header`] | `uncompressed_header()`'s intra path |
//! | [`transform`] | §7.13's inverse transforms and the 2D combine |
//! | [`predict`] | §7.11.2 intra prediction and §7.11.5 CFL |
//! | [`framebuf`] | The private reconstruction buffer intra prediction reads while writing (see `xtask/src/dup_check.rs`'s `DISTINCT` entries for why `vaco-codec-vp8`/`vaco-codec-vp9` have identically-named types) |
//! | [`decode`] | The tile/superblock/mode-info/residual walk and the [`vaco_codec_core::Decoder`] wiring |
//!
//! # Specification
//!
//! AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1
//! (`AOMedia`), registered as `aom-av1-spec` in `provenance/sources.toml`.
//! Default CDF tables (`cdf::default`) are mechanically extracted from the
//! specification's own §9.4 listing via `scripts/extract_cdf.py` rather
//! than retyped, and cross-checked by this crate's own tests rather than
//! against another implementation — a second transcription of the same
//! table is not an independent check. dav1d and libaom were not consulted,
//! to preserve this crate's clean-room provenance.
#![forbid(unsafe_code)]

pub mod cdf;
pub mod decode;
pub mod frame_header;
pub mod framebuf;
pub mod predict;
pub mod symbol;
pub mod tables;
pub mod transform;

pub use decode::{AV1_DECODER, Av1Decoder};
