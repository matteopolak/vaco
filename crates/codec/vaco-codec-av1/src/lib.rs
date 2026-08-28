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
//! Explicitly out of scope, left to later issues and other agents per
//! `planning/ASSIGNMENTS.md`: inter prediction (#34), deblocking/CDEF/
//! superres/loop restoration *application* (#35 — this crate parses their
//! header syntax so the bitstream stays aligned, but does not run the
//! filters, so reconstructed pixels are pre-in-loop-filter), film grain
//! synthesis (#343), frame threading/DPB (#344), and Argon conformance
//! (#345). An inter frame's header is rejected with
//! [`vaco_core::Error::Unsupported`] rather than guessed at.
//!
//! # Division of labour with `vaco-parse-av1`
//!
//! `vaco-parse-av1` already implements OBU framing (both the flat
//! `obu_has_size_field` stream and Annex B's length-delimited form),
//! `sequence_header_obu()` in full, and `AV1CodecConfigurationRecord`
//! (`av1C`). This crate depends on it for all three rather than
//! reimplementing them (D14.1 permits a `vaco-codec-*` crate depending on a
//! `vaco-parse-*` crate; the restriction is the other direction). Its own
//! `frame_header` module covers only the intra path, and deliberately does
//! not extend to `frame_size_with_refs()` — decoding is this crate's job.
//!
//! # Module map
//!
//! | Module | Contents |
//! |---|---|
//! | [`symbol`] | The §8.2 symbol decoder: `init_symbol`/`read_symbol`/`read_bool`/`read_literal`/`exit_symbol`, byte-for-byte off the specification text |
//! | [`cdf`] | Per-tile CDF context: a copy of every default table this crate uses, adapted in place by [`symbol::SymbolDecoder::read_symbol`] |
//! | [`tables`] | Block/transform-size conversion tables, scan orders, quantizer lookup tables — §9.2/§9.3, mechanically extracted where the specification prints them in a parseable form |
//! | [`frame_header`] | `uncompressed_header()`'s intra path in full: tile info, quantization, segmentation, delta-q/lf, loop filter/CDEF/restoration/film-grain syntax, tx mode |
//! | [`transform`] | §7.13's inverse transforms (DCT/ADST/identity/Walsh-Hadamard) and the 2D combine |
//! | [`predict`] | §7.11.2's intra prediction (DC, directional, smooth, Paeth via the recursive filter) and §7.11.5's CFL |
//! | [`framebuf`] | The private reconstruction buffer intra prediction reads while writing (same reasoning as `vaco-codec-vp8`/`vaco-codec-vp9`'s identically-named types — see `xtask/src/dup_check.rs`'s `DISTINCT` entries) |
//! | [`decode`] | The tile/superblock/partition/mode-info/residual walk, and the [`vaco_codec_core::Decoder`] wiring |
//!
//! # Specification
//!
//! AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1
//! (`AOMedia`), fetched directly and registered as `aom-av1-spec` in
//! `provenance/sources.toml`. Default CDF tables (`cdf::default`) are
//! mechanically extracted from the specification's own §9.4 listing via
//! `scripts/extract_cdf.py` rather than retyped, and cross-checked by this
//! crate's own tests rather than against another implementation's source —
//! see that module's doc for why a second transcription of the same table
//! is not an independent check. dav1d and libaom (both Tier A per
//! `planning/research/07-legal-patents-licensing.md` §1.6.1) were not
//! consulted; if that changes for a specific piece, the module doc and a
//! `provenance/` entry will say so at that point.
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
