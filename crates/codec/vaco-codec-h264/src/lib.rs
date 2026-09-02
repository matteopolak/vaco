//! H.264/AVC entropy decoding (CAVLC and CABAC residual blocks) plus enough of the macroblock
//! layer to drive CAVLC across a whole real slice.
//!
//! # Gating
//!
//! `vaco-parse-h264` (NAL/RBSP framing, SPS/PPS, slice headers, POC, SEI, Annex-B/avcC)
//! reconstructs no samples and stays in the default build. This crate's entropy decoding does,
//! so it is `encumbered = true` / `default = false` from the moment it exists.
//!
//! # What this implements
//!
//! [`cavlc::residual_block_cavlc`] and [`cabac_residual::residual_block_cabac`] are the
//! residual-coefficient half of `residual_block()` — clause 7.3.5.3.1-2 for CAVLC, 7.3.5.3.3 for
//! CABAC. [`mb`] drives CAVLC across a whole real slice bit-exactly, and CABAC across a whole
//! real I/P slice structurally but not yet bit-exactly — see [`mb`]'s own doc for what it covers
//! and refuses (MBAFF, the 8x8 transform, `constrained_intra_pred_flag`'s substitution rule,
//! CABAC B slices). CAVLC's output is discarded rather than fed to reconstruction, so
//! [`decoder`] refuses CAVLC honestly.
//!
//! # Verification
//!
//! Both entropy functions run against hand-built fixtures plus an exhaustive prefix-conflict
//! self-consistency check over every CAVLC table. That check alone missed real errors later
//! found by checking every table entry against a primary ITU-T H.264 text directly: several
//! `COEFF_TOKEN_NC2` rows and over half of `RUN_BEFORE`'s highest-risk row were wrong despite
//! being prefix-free (`cavlc_tables.rs`'s own doc has the full list, and the two 4:2:2
//! chroma-DC columns that source could not cover).
//!
//! CAVLC is checked end-to-end against real `ffmpeg`/`libx264` streams, catching a skipped
//! macroblock that never updated its neighbours' `nC` state and a `more_rbsp_data()` check one
//! branch too late after a skip run. CABAC drives real `libx264 -coder cabac` I/P slices but is
//! not yet bit-exact: building it found context tables ignoring `cabac_init_idc`, an unread
//! chroma DC `coded_block_flag`, an `intra_chroma_pred_mode` never stored for a neighbour's
//! `ctxIdxInc`, an inverted clause 9.3.3.1.1.6 comparison, and a `coded_block_pattern`
//! derivation that reused the left neighbour's rule for the above term. None is bit-exact yet;
//! the bypass path separately round-trips cleanly across 243 calls.

#![forbid(unsafe_code)]

mod cabac_mb_tables;
pub mod cabac_residual;
pub mod cavlc;
mod cavlc_tables;
mod deblock;
pub mod decoder;
mod dequant;
mod frame_task;
mod interp;
mod intra;
pub mod mb;
mod motion;
mod reconstruct;
mod scan;
mod task_pool;

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
///
/// `Caps::FRAME_THREADS` is what
/// [`vaco_codec_core::Threading::required_caps`] asks a component to declare
/// before it may return [`vaco_codec_core::Threading::Frame`] from
/// `set_thread_count`, which this decoder does — see
/// `docs/codec/frame-threading.md`. It is a statement about the *capability*,
/// not about the default: `-threads` unstated still means one thread and
/// spawns nothing.
///
/// `Caps::DELAY` is deliberately not here even though `H264Decoder` builds its
/// own `Machine` with it. That flag is the machine's own "more than one output
/// may be buffered" policy, set where the machine is constructed; the
/// descriptor has never carried it, and adding it now would be a separate
/// change with its own registry-visible consequences to check.
pub const DECODER_H264: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "h264",
    long_name: "H.264 / AVC / MPEG-4 Part 10",
    id: ::vaco_codec_core::CodecId::H264,
    media_type: ::vaco_core::MediaType::Video,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED.union(::vaco_codec_core::Caps::FRAME_THREADS),
    supported_rates: &[],
    make: |limits| ::std::boxed::Box::new(decoder::H264Decoder::new(limits)),
};
