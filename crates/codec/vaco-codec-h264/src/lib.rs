//! H.264/AVC entropy decoding (T3-01d/#417 CAVLC, T3-01e/#418 CABAC), the
//! far side of a line the previous dispatch drew rather than crossed, plus
//! enough of the macroblock layer (#419) to drive CAVLC across a whole real
//! slice and measure it.
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
//! `vaco-codec-msac` draws around VP8/VP9's bool decoders, applied to H.264.
//! [`mb`] is the macroblock layer that now sits above both, far enough
//! along to drive [`cavlc::residual_block_cavlc`] across a whole real CAVLC
//! slice bit-exactly, and to drive [`cabac_residual::residual_block_cabac`]
//! across a whole real CABAC I/P slice structurally (mb_type, mb_skip_flag,
//! coded_block_pattern, mb_qp_delta, ref_idx, mvd, coded_block_flag) though
//! not yet bit-exactly; see [`mb`]'s own module doc for exactly what it
//! covers and what it explicitly refuses (MBAFF, the 8x8 transform,
//! `constrained_intra_pred_flag`'s substitution rule, CABAC B slices).
//! Prediction, motion compensation, transform and reconstruction remain
//! #420 onward.
//!
//! [`H264Decoder::send_packet`] locates a slice header far enough to resolve
//! `entropy_coding_mode_flag` and then returns
//! [`vaco_core::Error::Unsupported`], honestly, the same choice
//! `vaco-codec-aac` made for the gap between "configuration resolved" and
//! "PCM produced" — [`mb::decode_slice_cavlc`] is not wired into it yet,
//! since nothing it reads is kept beyond what bit consumption needs.
//!
//! # Verification: what could be checked here, and what could not
//!
//! Both entropy functions are exercised against hand-built fixtures derived
//! directly from this crate's own tables (a test-only VLC encoder for
//! CAVLC, `vaco-codec-cabac`'s own encoder for CABAC — the same
//! justification that crate gives for having one at all: an arithmetic
//! coder cannot be tested against a hand-written bit pattern any other
//! way), plus an exhaustive pairwise prefix-conflict self-consistency
//! check across every CAVLC table, kept permanently
//! (`cavlc.rs::tests::every_coeff_token_table_is_prefix_free_and_matches_its_own_length`).
//! That check, and a from-recollection first pass, are what the
//! `CODED_BLOCK_PATTERN` lesson asked for as a floor — this crate went
//! further, re-fetching a primary edition of the ITU-T H.264 text and
//! checking every CAVLC table entry against it directly. That pass found
//! the self-consistency check alone had missed real errors (several
//! `COEFF_TOKEN_NC2` rows, and over half of `RUN_BEFORE`'s highest-risk
//! row, were wrong despite being prefix-free) — see `cavlc_tables.rs`'s own
//! module doc for the exhaustive list and for the two columns (the 4:2:2
//! chroma-DC case) that source could not cover and remain
//! self-consistency-only.
//!
//! **What is now checked end-to-end**: [`mb::decode_slice_cavlc`] against
//! two real `ffmpeg 8.1`/`libx264 -coder cavlc` elementary streams
//! (`tests/macroblock_layer.rs`, `tests/macroblock_layer_simple.rs`) — I/P/B
//! slices, multiple slices per picture, multiple reference frames, skipped
//! and coded macroblocks alike — every slice asserted to end with nothing
//! but `rbsp_slice_trailing_bits()` unconsumed. This measurement caught two
//! real bugs no hand-built fixture or self-consistency check could have: a
//! skipped macroblock never updating its neighbours' `nC` state (needs a
//! P/B slice with `mb_skip_run` — an I-only corpus cannot reach it), and
//! `more_rbsp_data()` being checked one branch too late after a nonzero
//! skip run (needs a multi-slice picture whose non-final slice ends
//! mid-skip-run — a single-slice corpus cannot reach it either). See
//! `mb.rs`'s own module doc and `docs/codec/vaco-codec-h264.md` for both,
//! in full.
//!
//! **What is not**: full bit-exactness for CABAC's macroblock layer.
//! [`mb::decode_slice_cabac`] now exists and drives I/P slices through
//! real `libx264 -coder cabac` corpora (`tests/macroblock_layer_cabac.rs`),
//! and building it caught two real bugs the same way the CAVLC pass did —
//! `cabac_residual.rs`'s context tables were a single shared table
//! ignoring `cabac_init_idc` entirely (not merely imprecise, structurally
//! wrong), and chroma DC's `coded_block_flag` was never actually read, both
//! now fixed against primary text. But bit consumption still diverges
//! partway through all three corpora, including an all-intra one built
//! specifically to rule out P-slice causes, in a way not root-caused within
//! this dispatch — see [`mb`]'s own module doc and
//! `docs/codec/vaco-codec-h264.md` for the exact minimal repro. Reporting
//! that honestly, rather than claiming the same bit-exact bar CAVLC holds,
//! is the same choice a previous dispatch on this project was asked to
//! make for the specification-only-dressed-as-verified gap.

#![forbid(unsafe_code)]

pub mod cabac_residual;
mod cabac_mb_tables;
pub mod cavlc;
mod cavlc_tables;
pub mod mb;
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
