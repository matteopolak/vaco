//! Raw / headerless elementary-stream muxers: 40 registrations.
//!
//! The counterpart of `vaco-demux-raw` (FM-26a): the same three families,
//! reversed. Muxing a raw format is close to trivial — 39 of the 40
//! registrations write nothing but the packet payload, back to back, with no
//! header and no trailer (see [`raw`]) — which is why FM-26b's effort budget
//! (2.5 pw) is a fraction of FM-26a's (3.5 pw).
//!
//! # Layout
//!
//! | Module | Registrations |
//! |---|---|
//! | [`raw`] | 21 PCM formats, `rawvideo`, and 17 bitstream formats: 39 verbatim writers |
//! | [`y4m`] | `yuv4mpegpipe`: the one format with a real header and per-frame marker |
//!
//! 39 + 1 = 40, matching FM-26b and the muxer half of `ffmpeg -muxers`'
//! raw/elementary-stream family (captured under `LC_ALL=C` against ffmpeg
//! 8.1 — see `docs/format/vaco-mux-raw.md`). This is a **different set** from
//! `vaco-demux-raw`'s 48: `mpegvideo`/`mjpeg_2000`/`bitpacked`/`v210`/
//! `v210x`/`s337m`/`loas` have no muxer at all in the reference, and
//! `mpeg1video`/`mpeg2video` (which *do* exist as muxers) are registered by a
//! different crate — the reference files them under its legacy/misc muxer
//! set, not its raw-elementary-stream one, and this crate's scope follows
//! that split exactly (measured; see the docs file).
//!
//! # `CodecId` cannot yet name most of these codecs
//!
//! Exactly the gap `vaco-demux-raw` reports: `vaco_codec_core::CodecId` has
//! no `Rawvideo`/`Vc1`/`Mpeg4`/`Avs2`/`Avs3`/`Cavs`/`Dirac`/`Dnxhd`/`Vvc`/
//! `Evc`/`H261`/`H263` variant and no per-subtype PCM tag, so
//! `MuxerDesc::default_video`/`default_audio` is `None` for most of this
//! crate's registrations rather than the reference's specific codec. See
//! `crate::raw`'s registration table for the full measured divergence list.

#![forbid(unsafe_code)]

pub mod raw;
pub mod y4m;

use vaco_format_core::MuxerDesc;

/// Every muxer this crate registers.
#[must_use]
pub fn all_muxers() -> Vec<&'static MuxerDesc> {
    let mut out = Vec::new();
    out.extend(raw::RAW_MUXERS.iter().copied());
    out.push(&y4m::MUXER_YUV4MPEGPIPE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_forty_registrations() {
        assert_eq!(all_muxers().len(), 40);
    }

    #[test]
    fn every_name_is_unique() {
        let all = all_muxers();
        let mut names: Vec<&str> = all.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate muxer name registered");
    }
}
