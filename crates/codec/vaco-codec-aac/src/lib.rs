//! AAC-LC decode (T3-03, epic #53), and the reason this crate is gated.
//!
//! # D4: why this is not in the default build
//!
//! AAC is legally RED, not merely off-by-default for a portability reason
//! the way e.g. a socket-dependent protocol crate is. `planning/00-decisions.md`
//! D4 and `planning/research/07-legal-patents-licensing.md` §5.2/§6: the Via
//! LA AAC pool is active, charges per **decoder or encoder unit** (not per
//! bitstream — AAC *remuxing*, which never instantiates a decoder, stays in
//! the default build and is already delivered by `vaco-parse-aac`), and has
//! not wound down. `epic #53`'s own title carries the feature name this
//! crate is gated behind: `patent-encumbered-aac-decode`.
//!
//! This is the **first** component in the tree to set `encumbered = true` in
//! its `vaco-component.toml` fragment — `cargo xtask patent-gate`'s own
//! output says as much ("no component in the tree is marked `encumbered =
//! true` yet") for every run before this one. The shape here is therefore
//! the precedent: `vaco-component.toml` sets `default = false` and
//! `encumbered = true` together (D4.1 requires both — an encumbered
//! component with `default = true` is refused by `gen-registry` outright),
//! and [`DECODER_AAC`] additionally sets `caps: Caps::PATENT_ENCUMBERED` in
//! code, which is the field [`vaco_codec_core::DecoderDesc::is_default_build_safe`]
//! reads. Both halves matter and neither substitutes for the other: the
//! registry fragment controls whether `vaco-codec-aac` is even compiled into
//! a given build (the D4.1 mechanism `cargo xtask patent-gate` actually
//! checks — see that gate's own doc for why it asserts on the compiled
//! feature list rather than on a manifest's stated intent), and the `Caps`
//! bit is a runtime-inspectable property of a `DecoderDesc` for any code
//! (a `-h decoder=` listing, a future gate) that walks the registry's
//! descriptors rather than its Cargo feature graph.
//!
//! # What is implemented (T3-03a / #443) and what is not (#444, #445)
//!
//! This crate currently resolves **configuration only**: `AudioSpecificConfig`
//! (via `vaco-parse-aac`, which already parses it fully for container-reporting
//! purposes — see [`config`]'s module doc for the small amount this crate adds
//! on top), the [`pce::ProgramConfigElement`] syntax in full (new — no other
//! crate in this workspace had a reason to parse one), raw-ADTS/LATM handover,
//! and object-type gating to AAC-LC only. It does not decode a single
//! spectral coefficient. [`decoder::AacDecoder::send_packet`] resolves a
//! packet's configuration completely and then returns
//! [`vaco_core::Error::Unsupported`], honestly, rather than either refusing
//! packets outright or fabricating PCM it cannot yet produce correctly.
//!
//! See `docs/codec/vaco-codec-aac.md` for the full writeup, including exactly
//! which `channelConfiguration` values are resolved today (1, 2, 6, and 0 via
//! a program config element) and why the rest (3, 4, 5, 7, 11, 12, 14) are
//! deliberately gated rather than guessed at without ISO/IEC 14496-3 Table
//! 42's exact element ordering in hand to check against.

#![forbid(unsafe_code)]

pub mod config;
pub mod decoder;
pub mod pce;
mod spectral_tables;
mod ics;
mod ics_stream;
mod scalefactor;
mod pulse;
mod raw_data_block;
mod section;
mod spectral;
mod swb_tables;
mod tns;
mod tns_apply;
mod reconstruct;

pub use config::{ChannelResolution, DecoderConfig};
pub use decoder::AacDecoder;
pub use pce::{ChannelElementRef, ProgramConfigElement, find_leading_program_config_element};

/// The registry descriptor for this crate's decoder.
///
/// `caps: Caps::PATENT_ENCUMBERED` is the code-level half of D4's gating —
/// see this module's own doc for the other half, the `vaco-component.toml`
/// fragment's `encumbered = true` / `default = false` pair, which is what
/// `cargo xtask patent-gate` actually checks.
pub const DECODER_AAC: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "aac",
    long_name: "AAC-LC (Advanced Audio Coding, Low Complexity)",
    id: ::vaco_codec_core::CodecId::Aac,
    media_type: ::vaco_core::MediaType::Audio,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED,
    supported_rates: &[],
    make: |limits| ::std::boxed::Box::new(decoder::AacDecoder::new(limits)),
};
