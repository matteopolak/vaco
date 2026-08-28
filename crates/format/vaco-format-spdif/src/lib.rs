//! IEC 61937 (S/PDIF compressed-audio encapsulation) and SMPTE 337M.
//!
//! Both wrap a compressed audio frame — AC-3, in everything this crate
//! supports — inside what looks like a 16-bit PCM stream: a fixed preamble
//! (`Pa Pb Pc Pd`), the frame's bytes with every adjacent pair byte-swapped,
//! then zero padding out to a fixed burst size. See [`iec61937`] for the
//! shared shape and exactly what was measured against real `ffmpeg -f
//! spdif` output, [`demux`]/[`mux`] for the `spdif` demuxer/muxer, and
//! [`s337m`] for why the `s337m` demuxer is a thin, honestly-scoped wrapper
//! around the same code.
//!
//! # A real, measured asymmetry worth calling out up front
//!
//! `ffmpeg -h demuxer=spdif` (8.1) lists **no** `Common extensions:` line at
//! all — not even `spdif` — while `-h muxer=spdif` lists `spdif` **and** a
//! `Mime type:` line the demuxer has neither of, and the two directions'
//! long names differ by punctuation (`"IEC 61937 (compressed data in
//! S/PDIF)"` for the demuxer, `"IEC 61937 (used on S/PDIF - IEC958)"` for
//! the muxer). Every one of those is reproduced here exactly as measured,
//! not "corrected" to look more symmetric.
//!
//! # The one codec this crate can verify
//!
//! `-h muxer=spdif` states `Default audio codec: ac3`, and it is also the
//! only codec this crate's demuxer and muxer were checked against each
//! other with — a full remux round trip against a real `ffmpeg`-produced
//! `.spdif` file is byte-identical (`tests/reference_files.rs`). MPEG-1
//! layer 2/3, DTS and E-AC-3 all have well-defined IEC 61937 data-type
//! numbers (5, 11, 21 — read straight off real captures, see
//! `iec61937.rs`), but this crate does not know their `Pd` unit convention
//! or padding size without measuring it the same way AC-3's was measured,
//! so `Error::Unsupported` is what a burst of any of those types gets
//! today, rather than a guess.

#![forbid(unsafe_code)]

mod ac3;
pub mod demux;
pub mod iec61937;
pub mod mux;
pub mod s337m;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, MuxerDesc, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

pub use demux::SpdifDemuxer;
pub use mux::SpdifMuxer;
pub use s337m::S337mDemuxer;

/// Content sniff for `spdif`: a valid `Pa Pb` sync, an AC-3 data type, and
/// (for the strongest signal) an AC-3 sync frame that itself parses. There
/// is no extension to fall back to — see the module docs on why.
#[must_use]
pub fn probe(data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    let head: Vec<u8> = (0..iec61937::HEADER_LEN)
        .map(|i| data.get(i).unwrap_or(0))
        .collect();
    let Some(header) = iec61937::BurstHeader::parse(&head, false) else {
        return vaco_format_core::ProbeScore::NONE;
    };
    if header.data_type() != iec61937::DATA_TYPE_AC3 {
        return vaco_format_core::ProbeScore::CONTENT;
    }
    let Ok(payload_len) = header.ac3_payload_len_bytes() else {
        return vaco_format_core::ProbeScore::CONTENT;
    };
    let frame: Vec<u8> = (0..payload_len.min(8))
        .map(|i| data.get(iec61937::HEADER_LEN + i).unwrap_or(0))
        .collect();
    let unswapped = iec61937::unswap_payload(&frame, frame.len());
    if ac3::parse(&unswapped).is_some() {
        vaco_format_core::ProbeScore::MAX
    } else {
        vaco_format_core::ProbeScore::CONTENT
    }
}

/// The `spdif` demuxer registry descriptor. No extensions, no MIME type —
/// see the module docs.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "spdif",
    long_name: "IEC 61937 (compressed data in S/PDIF)",
    extensions: &[],
    mime_types: &[],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

/// The `spdif` muxer registry descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "spdif",
    long_name: "IEC 61937 (used on S/PDIF - IEC958)",
    extensions: &["spdif"],
    default_video: None,
    default_audio: Some(vaco_codec_core::CodecId::Ac3),
    open: open_muxer,
};

/// The `s337m` demuxer registry descriptor. Demux-only, matching the
/// reference — there is no `-h muxer=s337m`.
pub const S337M_DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "s337m",
    long_name: "SMPTE 337M",
    extensions: &[],
    mime_types: &[],
    flags: crate::s337m::FLAGS,
    probe: s337m::probe,
    open: open_s337m_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    // AC-3 is fully framed by the burst; there is no in-band parser state
    // to hand off.
    Ok(Box::new(SpdifDemuxer::open(src)?))
}

fn open_s337m_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(S337mDemuxer::open(src)?))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "return type is fixed by MuxerDesc::open; SpdifMuxer::new cannot fail"
)]
fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn vaco_format_core::Muxer>> {
    Ok(Box::new(SpdifMuxer::new(sink)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demuxer_descriptor_answers_to_its_name_and_has_no_extension() {
        assert!(DEMUXER.matches_name("spdif"));
        assert!(!DEMUXER.matches_extension("/tmp/x.spdif"));
    }

    #[test]
    fn the_muxer_descriptor_answers_to_its_name_and_default_codec() {
        assert!(MUXER.matches_name("spdif"));
        assert_eq!(MUXER.extensions, &["spdif"]);
        assert_eq!(
            MUXER.default_codec(vaco_core::MediaType::Audio),
            Some(vaco_codec_core::CodecId::Ac3)
        );
    }

    #[test]
    fn the_s337m_descriptor_answers_to_its_name() {
        assert!(S337M_DEMUXER.matches_name("s337m"));
    }

    #[test]
    fn the_probe_rejects_prose() {
        let data = vaco_format_core::ProbeData::new(b"The quick brown fox jumps over.");
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::NONE);
    }
}
