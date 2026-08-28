//! FLAC decode and encode.
//!
//! Decode goes through the `claxon` crate rather than a from-scratch
//! bitstream reader — [`claxon_boundary`] is the single D11 adapter module
//! that names `claxon`; nothing else in this crate does, so swapping it for
//! a native decoder later touches one file. Encode is native: fixed block
//! size, `CONSTANT`/`VERBATIM`/fixed-predictor/LPC subframes and single-
//! partition Rice residual coding, written from the FLAC specification
//! (RFC 9639) rather than derived from either the decode path or any
//! reference source. LPC analysis (autocorrelation, Levinson-Durbin,
//! quantisation) is `vaco-codec-dsp-lpc` (D-07), not reimplemented here —
//! see [`lpc`]'s module doc. See [`encoder`]'s module doc for exactly what
//! that encoder does and does not implement (no stereo decorrelation, no
//! multi-partition residuals, no windowing before the LPC analysis — none
//! of which affect correctness).
//!
//! FLAC is patent-free (D9): both directions ship unconditionally, with no
//! `encumbered` feature gate.
//!
//! # Module map
//!
//! | Module | Contents |
//! |---|---|
//! | [`streaminfo`] | Building, finding and reading the 34-byte `STREAMINFO` metadata block |
//! | [`claxon_boundary`] | The D11 adapter: the only file that names `claxon` |
//! | [`decoder`] | [`decoder::FlacDecoder`], a [`vaco_codec_core::Decoder`] |
//! | [`fixed`] | The five fixed polynomial predictors (encode side) |
//! | [`lpc`] | LPC subframe analysis and quantisation, over `vaco-codec-dsp-lpc` (encode side) |
//! | [`rice`] | Rice/Golomb residual coding (encode side) |
//! | [`crc`] | The frame header CRC-8 and frame footer CRC-16 |
//! | [`encoder`] | [`encoder::FlacEncoder`], a [`vaco_codec_core::Encoder`] |

#![forbid(unsafe_code)]

pub mod claxon_boundary;
pub mod crc;
pub mod decoder;
pub mod encoder;
pub mod fixed;
pub mod lpc;
pub mod rice;
pub mod streaminfo;

pub use decoder::FlacDecoder;
pub use encoder::FlacEncoder;

use vaco_codec_core::{Caps, CodecId, Decoder, DecoderDesc, Encoder, EncoderDesc};
use vaco_core::MediaType;
use vaco_limits::Limits;

fn make_decoder(limits: Limits) -> Box<dyn Decoder> {
    Box::new(FlacDecoder::new(limits))
}

fn make_encoder(limits: Limits) -> Box<dyn Encoder> {
    Box::new(FlacEncoder::new(limits))
}

/// This build's FLAC decoder descriptor.
pub static DECODER_FLAC: DecoderDesc = DecoderDesc {
    name: "flac",
    long_name: "FLAC (Free Lossless Audio Codec), decode via claxon",
    id: CodecId::Flac,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_decoder,
};

/// This build's FLAC encoder descriptor.
pub static ENCODER_FLAC: EncoderDesc = EncoderDesc {
    name: "flac",
    long_name: "FLAC (Free Lossless Audio Codec), native encode",
    id: CodecId::Flac,
    media_type: MediaType::Audio,
    caps: Caps::empty(),
    supported_rates: &[],
    make: make_encoder,
};
