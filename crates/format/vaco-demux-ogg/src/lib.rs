//! The Ogg demuxer: RFC 3533 page/packet framing, chained and multiplexed
//! logical bitstreams, and per-codec granule-position → timestamp mapping.
//!
//! # Why Ogg gets its own crate
//!
//! D3/D4/D9 make Opus, Vorbis and FLAC the royalty-free default, and Ogg is
//! their native container — the only one of the three formats plan 18
//! §3.4.5 names that ships no timestamp of its own on any packet, deriving
//! everything from one page-level field instead. Reach it wrong and every
//! duration downstream is wrong with it — see `crate::granule` for how this
//! crate measured, rather than assumed, each codec's mapping.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`page`] | the page header, its segment table, and Ogg's own CRC-32 (`crc`) |
//! | [`codec`] | identifying a logical stream from its first packet, and reading the fixed identification-header fields the granule mapping needs |
//! | [`granule`] | granule position → timestamp, per codec, and the page-anchored packet-timestamp assignment engine |
//! | [`demux`] | page reading, packet reassembly across page continuations, chained/multiplexed stream discovery |
//! | [`probe`] | content detection, measured against `ffprobe 8.1` |
//!
//! `crates/format/vaco-mux-ogg` is a sibling crate that reuses [`page`] to
//! build pages rather than parse them — the same D19 pattern
//! `vaco-mux-flv`/`vaco-demux-flv` already use for their shared AMF0 model.
//!
//! # What is solid, and what is not — read this before trusting a number
//!
//! * **Page and packet framing**: solid. Lacing (including the
//!   exact-multiple-of-255 trailing-zero case and the open-continuation
//!   case), the CRC, and resynchronisation on a corrupt or missing page are
//!   all covered by unit and property tests, and the fuzz target drives the
//!   whole demuxer end to end.
//! * **Opus and Vorbis granule mapping**: measured against real files from
//!   this build's `ffmpeg 8.1` (`libopus` and the native `vorbis` encoder —
//!   see `crate::granule`'s doc comments for the exact commands and the
//!   numbers that came back). Opus additionally reaches
//!   `vaco-parse-opus`'s exact per-packet duration through `ParserProvider`,
//!   for streams discovered during [`OggDemuxer::open`] — see the `demux`
//!   module docs for the one case that cannot (a chained stream discovered
//!   later, because the frozen `Demuxer::read_packet` carries no provider).
//! * **FLAC**: the container framing (page/packet layer, `STREAMINFO`
//!   fields) is measured against a real file; the granule-to-timestamp
//!   mapping (plain sample count) is measured too, but per-packet timing
//!   inside a page uses the same equal-division fallback as an unrecognised
//!   codec rather than parsing each FLAC frame header's own block-size
//!   field — exact for a constant block size, approximate across a change.
//! * **Theora and Speex**: implemented from the public specification only.
//!   **No Theora or Speex encoder exists in this environment** (`ffmpeg
//!   -encoders` was checked; both are absent), so neither mapping has been
//!   measured against a real file — see `crate::granule` and `crate::codec`
//!   for exactly which fields that affects.
//! * **Duration estimation is not implemented.** `Demuxer::duration()` uses
//!   the trait's own `None` default rather than the tail scan
//!   `vaco-demux-mpegts` performs for the same reason (no in-band total).
//! * **Seeking is byte-only.** `SeekTarget::Timestamp` returns
//!   `Error::Unsupported`; there is no index and no bisection oracle yet.
//!
//! See `docs/format/vaco-demux-ogg.md` for the full accounting, including
//! the exact `ffmpeg`/`ffprobe` invocations every measured number above
//! came from.

#![forbid(unsafe_code)]

pub mod codec;
pub mod crc;
pub mod demux;
pub mod granule;
pub mod page;
pub mod probe;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, FormatOptions, ParserProvider};
use vaco_io::MediaSource;

pub use demux::{FLAGS, OggDemuxStats, OggDemuxer};
pub use probe::probe;

/// The registry descriptor.
///
/// One name, `"ogg"`, answering for every extension the reference groups
/// under it — `oga`/`ogv`/`ogx`/`opus`/`spx` are aliases with a different
/// default codec on the *muxer* side (see `vaco-mux-ogg`) and identical
/// framing on this side, since nothing about page/packet parsing depends on
/// which codec is inside.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ogg",
    long_name: "Ogg",
    extensions: &["ogg", "oga", "ogv", "ogx", "opus", "spx"],
    mime_types: &[
        "application/ogg",
        "audio/ogg",
        "video/ogg",
        "audio/opus",
        "audio/speex",
    ],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(OggDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_answers_to_the_names_the_cli_uses() {
        assert!(DEMUXER.matches_name("ogg"));
        assert!(DEMUXER.matches_extension("/tmp/x.opus"));
        assert!(DEMUXER.matches_extension("/tmp/x.OGA"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_declared_flags_are_the_ones_the_models_depend_on() {
        use vaco_format_core::FormatFlags;
        assert!(FLAGS.contains(FormatFlags::SHOW_IDS));
        assert!(FLAGS.contains(FormatFlags::GENERIC_INDEX));
    }
}
