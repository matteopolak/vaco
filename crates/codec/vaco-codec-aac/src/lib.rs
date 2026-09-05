//! AAC-LC decoding, narrow silence encoding, and patent-gating policy.
//!
//! # Patent gating
//!
//! The project treats AAC decoding as patent encumbered. The component
//! fragment therefore pairs `encumbered = true` with `default = false`, while
//! [`DECODER_AAC`] exposes [`vaco_codec_core::Caps::PATENT_ENCUMBERED`] to
//! runtime registry consumers. Both declarations are required: one controls
//! compilation and the other makes the policy inspectable after registration.
//!
//! AAC remuxing does not instantiate this decoder and remains available
//! through `vaco-parse-aac` in the default build.
//!
//! # Decoder scope
//!
//! The decoder resolves `AudioSpecificConfig` and program configuration,
//! parses raw data blocks, and reconstructs AAC-LC PCM through inverse
//! quantisation, stereo tools, TNS, IMDCT, windowing, and overlap-add.
//! Unsupported channel configurations and coupling elements are refused by
//! name rather than approximated.
//! [`AacLcSilenceAccessUnit`] writes a deliberately constrained raw AAC-LC
//! payload and its out-of-band configuration for exact silent mono or stereo
//! 44.1 or 48 kHz frames. [`AacLcSilenceEncoder`] only adds ADTS framing to
//! that payload; neither is registered as a general AAC encoder.
//!
//! See `docs/codec/vaco-codec-aac.md` for supported configurations, measured
//! reconstruction quality, and remaining refusal boundaries.

#![forbid(unsafe_code)]

pub mod config;
pub mod decoder;
mod encoder;
mod ics;
mod ics_stream;
pub mod pce;
mod pulse;
mod qmf;
mod raw_data_block;
mod reconstruct;
mod sbr_huffman_tables;
mod scalefactor;
mod section;
mod spectral;
mod spectral_tables;
mod swb_tables;
mod tns;
mod tns_apply;

pub use config::{ChannelResolution, DecoderConfig};
pub use decoder::AacDecoder;
pub use encoder::{AacLcSilenceAccessUnit, AacLcSilenceEncoder};
pub use pce::{ChannelElementRef, ProgramConfigElement, find_leading_program_config_element};

/// The registry descriptor for this crate's decoder.
///
/// `Caps::PATENT_ENCUMBERED` mirrors the component fragment's
/// `encumbered = true` and `default = false` compile-time policy at runtime.
pub const DECODER_AAC: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "aac",
    long_name: "AAC-LC (Advanced Audio Coding, Low Complexity)",
    id: ::vaco_codec_core::CodecId::Aac,
    media_type: ::vaco_core::MediaType::Audio,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED,
    supported_rates: &[],
    make: |limits| ::std::boxed::Box::new(decoder::AacDecoder::new(limits)),
};
