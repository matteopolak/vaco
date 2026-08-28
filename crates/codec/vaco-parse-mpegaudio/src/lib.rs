//! MPEG-1/2/2.5 audio (Layer I/II/III) and AC-3/E-AC-3 sync-frame parsing as
//! bitstream [`Parser`](vaco_codec_core::Parser)s (no decode).
//!
//! # What is in here
//!
//! | Module | Wraps | Specification |
//! |---|---|---|
//! | [`mpegaudio`] | `vaco_format_mpegaudio::header::MpegAudioHeader` | ISO/IEC 11172-3 / 13818-3 |
//! | [`ac3`] | `vaco_format_ac3::{syncinfo, bsi}` | ATSC A/52:2018 |
//!
//! Neither module re-derives the frame syntax: `vaco-format-mpegaudio` and
//! `vaco-format-ac3` already parse it precisely for their demuxers and
//! decoders, and D19 ("one definition per concept") is exactly the reason to
//! depend on them rather than re-deriving a second bit-rate table or a second
//! `bsi()` walk here. This crate's own job is thin: fold an already-parsed
//! header into [`vaco_codec_core::CodecParameters`] and drive the resync loop
//! a byte-stream `Parser` needs.
//!
//! ADTS/LATM/`AudioSpecificConfig` — the other half of P-03 — ship in
//! `vaco-parse-aac`, which registers `CodecId::Aac`/`AacLatm` separately: see
//! that crate's docs for why LATM is a distinct codec identity rather than a
//! framing of AAC.

#![forbid(unsafe_code)]

pub mod ac3;
pub mod mpegaudio;

pub use ac3::Ac3Parser;
pub use mpegaudio::MpegAudioParser;

/// The MPEG-1/2/2.5 Layer I/II/III descriptor.
///
/// One implementation, three codec identities: the frame syntax
/// [`MpegAudioParser`] walks is identical across layers and only the
/// `layer` field distinguishes them, so a single instance answers for
/// whichever of `Mp1`/`Mp2`/`Mp3` the registry asks for and reports the
/// codec it actually finds in the bitstream. A container that already
/// stated its own codec identity keeps it — [`vaco_codec_core::CodecParameters::fill_from`]
/// only fills in a field the container left unset — so this cannot
/// contradict a demuxer that already knows better; it only helps one that
/// does not, such as a raw `.mp3`/`.mp2` elementary stream.
pub const PARSER_MPEGAUDIO: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "mp3",
    long_name: "MP1/2/3 (MPEG audio layer 1/2/3)",
    codecs: &[
        ::vaco_codec_core::CodecId::Mp3,
        ::vaco_codec_core::CodecId::Mp2,
        ::vaco_codec_core::CodecId::Mp1,
    ],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(mpegaudio::MpegAudioParser::new(limits)),
};

/// The classic AC-3 descriptor.
pub const PARSER_AC3: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "ac3",
    long_name: "ATSC A/52A (AC-3)",
    codecs: &[::vaco_codec_core::CodecId::Ac3],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(ac3::Ac3Parser::new(limits)),
};

/// The E-AC-3 descriptor. Same implementation as [`PARSER_AC3`] —
/// [`ac3::Ac3Parser`] tells the two apart per frame from `bsid`, exactly as
/// `vaco_format_ac3::syncinfo::parse` does — registered separately because
/// the registry keys a [`vaco_codec_core::ParserDesc`] one name per fragment
/// and the reference reports them as different `codec_name`s.
pub const PARSER_EAC3: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "eac3",
    long_name: "ATSC A/52B (E-AC-3)",
    codecs: &[::vaco_codec_core::CodecId::Eac3],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(ac3::Ac3Parser::new(limits)),
};

#[cfg(test)]
mod tests;
