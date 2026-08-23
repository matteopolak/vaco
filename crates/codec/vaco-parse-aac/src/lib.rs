//! AAC `ADTS`/`LATM` and `AudioSpecificConfig` parsing (no decode).
//!
//! # Parsing is not decoding, and the distinction is load-bearing
//!
//! D9 makes AAC **RED**: the Via LA pool charges per encoder and per decoder
//! unit, so AAC *remuxing* stays in the default build while encode and decode
//! are gated. Reading an ADTS header or an `AudioSpecificConfig` implements no
//! decoder — it recovers facts about a bitstream — so this crate ships by
//! default. Nothing here reconstructs spectra and nothing here produces PCM,
//! and that is a boundary to keep rather than a stage to grow past.
//!
//! # What is in here
//!
//! | Module | Syntax | Specification |
//! |---|---|---|
//! | [`asc`] | `AudioSpecificConfig` | ISO/IEC 14496-3 subpart 1 §1.6.2.1 |
//! | [`adts`] | `adts_frame`, `adts_fixed_header`, `adts_variable_header` | ISO/IEC 14496-3 subpart 4 §4.4.1.1 |
//! | [`latm`] | `AudioSyncStream`, `AudioMuxElement`, `StreamMuxConfig` | ISO/IEC 14496-3 subpart 4 §1.7 |
//! | [`tables`] | the sampling-frequency index and channel-configuration tables | as above |
//!
//! MP4 carries the `AudioSpecificConfig` as an `esds`
//! `DecoderSpecificInfo` (ISO/IEC 14496-14 §5.6); a demuxer hands those bytes
//! straight to [`AudioSpecificConfig::parse`].
//!
//! # The one thing worth reading first: where the reported rate comes from
//!
//! `ffprobe`'s `sample_rate` and `channels` for an AAC stream are **not** the
//! ADTS header's fields, and they are not always the configuration's core
//! fields either.
//!
//! * With an `AudioSpecificConfig` — MP4, or LATM — the reported rate is the
//!   *extension* sampling frequency whenever `sbrPresentFlag` is set, and the
//!   channel count doubles from one to two when Parametric Stereo is signalled
//!   **or merely not denied**. [`AudioSpecificConfig::output_sample_rate`] and
//!   [`AudioSpecificConfig::output_channels`] implement exactly that, and their
//!   documentation carries the measured table.
//! * With **raw ADTS** there is no configuration at all: HE-AAC is signalled
//!   implicitly, inside the payload, and the reference recovers it by decoding.
//!   We cannot and do not. See the divergence section of
//!   `docs/codec/vaco-parse-aac.md`; it is the one place where a
//!   parse-only build cannot reproduce the reference's numbers.
//!
//! # Example
//!
//! ```
//! use vaco_parse_aac::AudioSpecificConfig;
//!
//! // AAC LC, 22050 Hz core, stereo, with explicit SBR at 44100 Hz —
//! // the configuration an HE-AAC MP4 carries in its `esds`.
//! let cfg = AudioSpecificConfig::parse(&[0x13, 0x90, 0x56, 0xe5, 0xa0])?;
//! assert_eq!(cfg.sampling_frequency, 22050);
//! assert_eq!(cfg.output_sample_rate(), 44100);
//! assert!(cfg.has_sbr());
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod adts;
pub mod asc;
pub mod latm;
pub mod tables;

pub use adts::{AdtsHeader, AdtsParser, MpegVersion};
pub use asc::{AudioObjectType, AudioSpecificConfig, Signal};
pub use latm::{FrameLengthType, LoasParser, MuxStream, StreamMuxConfig, SyncStreamHeader};
pub use tables::{
    SAMPLING_FREQUENCY, channels_for_config, frequency_for_index, layout_for_config, profile_name,
};

#[cfg(test)]
mod tests;

/// The registry descriptors for this crate's two parsers.
///
/// Two, not one, because the reference treats LATM/LOAS as a **separate codec**
/// rather than a framing of AAC — `ffprobe` prints `codec_name=aac_latm` — and
/// `CodecId` mirrors that. One descriptor per `CodecId` keeps the registry's
/// "two crates may not register the same name" check meaningful.
pub const PARSER: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "aac",
    long_name: "AAC (Advanced Audio Coding)",
    codecs: &[::vaco_codec_core::CodecId::Aac],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(adts::AdtsParser::new(limits)),
};

/// The LATM/LOAS descriptor. See [`PARSER`].
pub const PARSER_LATM: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "aac_latm",
    long_name: "AAC LATM (Advanced Audio Coding LATM syntax)",
    codecs: &[::vaco_codec_core::CodecId::AacLatm],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(latm::LoasParser::new(limits)),
};
