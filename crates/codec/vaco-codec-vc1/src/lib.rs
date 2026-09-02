//! VC-1 / WMV3 decode (SMPTE ST 421:2013, freely published by SMPTE from
//! <https://pub.smpte.org/pub/st421/st0421-2013.pdf> — Tier A per D7, a
//! published Standard) plus SMPTE RP 227 for the Annex J/L
//! decoder-initialization metadata layout
//! (<https://multimedia.cx/mirror/rp227.pdf>). `Vaco-Spec-Ref: smpte-st421-2013`
//!
//! # Legal status: encumbered
//!
//! VC-1 is patent-encumbered: developed by Microsoft and later placed under
//! an MPEG-LA-administered pool of contributor patents covering VC-1
//! encode/decode, with no ruling yet on this project's own exposure. Per
//! D4, this crate is registered `encumbered = true` / `default = false` —
//! the same gate `vaco-codec-h264` and `vaco-codec-aac` use — pending an
//! owner decision, not shipped green by default.
//!
//! # Scope: Simple/Main profile, progressive I-frame only
//!
//! VC-1's full syntax (Advanced profile, interlace, P/B inter prediction,
//! in-loop deblocking, overlap smoothing, range reduction, multi-resolution
//! up-sampling) is comparable in scope to H.264 baseline plus its own
//! transform family. Implemented and verified against a real bitstream:
//! decoder-initialization metadata (`STRUCT_C`, Annex J.2/Table 263, plus
//! this crate's own width/height extension — see [`header`]'s docs); the
//! progressive I-picture header (SS7.1.1); the Annex A 8x8 inverse
//! transform; AC/DC dequantization (SS8.1.3.3/.8); and intra-only
//! macroblock/block decode (Table 27, High Rate Intra/Inter coding sets
//! only — see [`decoder`]'s docs for what is and is not transcribed).
//!
//! # Verification
//!
//! Cross-checked bit-by-bit by hand against a real Main-profile RCV fixture
//! from FFmpeg's public FATE corpus before any decoder code was written;
//! see `tests/oracle.rs` for the fixture's exact header values and result.
//!
//! # Dependencies
//!
//! [`vaco_codec_vlc`] for VLC tables; [`vaco_bitstream::BitReader`] for the
//! MSB-first bitstream. Not `vaco-codec-mpegvideo`: its shared machinery is
//! all inter-prediction or MPEG-style zigzag/dequant, none of which this
//! I-frame-only, differently-shaped crate reaches or shares.

#![forbid(unsafe_code)]

mod decoder;
mod header;
mod tables;
mod transform;

pub use decoder::{DECODER_VC1, Vc1Decoder};
