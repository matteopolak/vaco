//! Theora video decode, native, keyframes only.
//!
//! `Vaco-Spec-Ref: theora-spec-20170603` (Theora Specification, Xiph.Org
//! Foundation, June 3 2017) is the normative source; VP3 has no separate
//! written specification.
//!
//! # Scope
//!
//! This crate decodes `FTYPE == 0` frames (section 7.1) and returns
//! [`vaco_core::Error::Unsupported`] for `FTYPE == 1` rather than attempting
//! motion compensation. Inter decode needs reference-frame storage, motion
//! vectors (section 7.5), and the whole-/half-pixel predictors (sections
//! 7.9.1.2/7.9.1.3). All-keyframe Ogg/Theora streams decode end to end; a
//! delta stream stops with a typed error on its first delta frame. Encoding is
//! out of scope.
//!
//! Theora is royalty-free and derived from the donated, patent-unencumbered
//! VP3, so this crate does not set `Caps::PATENT_ENCUMBERED`.
//!
//! # Evidence
//!
//! `tests/oracle.rs` compares two real Ogg/Theora fixtures against an ffmpeg
//! decode, checking Y, U and V independently. Both fixtures are byte-exact on
//! every tested keyframe. The oracle covers the common even-offset crop path;
//! odd-offset and non-block-aligned crops remain outside the implemented scope.
//!
//! # How to change it
//!
//! Inter-frame support starts in [`ident::PixelFormat`] and
//! [`blocks::FrameGeom`], then adds motion-vector and macroblock-mode decoding
//! before [`frame::decode_frame_payload`] can accept `FTYPE == 1`.
//!
//! # Configuration
//! [`decoder::TheoraDecoder::set_extradata`] checks `FMBW`/`FMBH` against
//! [`vaco_limits::Limits`] before constructing block-indexed tables; per-block
//! allocations use the same budget.
//!
//! # Dependencies
//! `vaco-codec-core`, `vaco-bitstream`, `vaco-frame`, `vaco-pixfmt`,
//! `vaco-packet`, and `vaco-limits`.

#![forbid(unsafe_code)]

mod blocks;
mod decoder;
mod frame;
mod huffman;
mod idct;
mod ident;
mod quant;
mod setup;
mod tokens;

pub use decoder::{DECODER_THEORA, TheoraDecoder};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_answers_to_its_own_name() {
        assert_eq!(DECODER_THEORA.name, "theora");
        assert_eq!(DECODER_THEORA.id, vaco_codec_core::CodecId::Theora);
    }
}
