//! Utility checksum muxers: `crc`, `md5`, `hash`, `framecrc`, `framemd5`,
//! `framehash`, `streamhash` — the differential-testing oracle the rest of
//! this project's format and codec crates are checked against (FM-20, issue
//! #572).
//!
//! # Why exact text layout matters more here than anywhere else
//!
//! Every other container in this workspace is checked against the reference
//! by running both through *these* muxers and diffing the text. If a field is
//! reordered, a separator is off by one space, or a checksum uses the wrong
//! variant, every downstream comparison becomes noise rather than signal —
//! there is no way to tell "the demuxer under test is wrong" from "the oracle
//! is wrong" once that happens. Every line format and algorithm choice in
//! this crate was therefore captured by probing the installed reference
//! (ffmpeg 8.1, `LC_ALL=C`) rather than recalled, and the probe transcripts
//! are kept in each module's docs and in `docs/format/vaco-mux-hash.md` so the
//! next person does not have to re-derive them.
//!
//! # The one non-obvious finding
//!
//! `crc` and `framecrc` are **not** CRC-32 despite the name — they compute
//! Adler-32 (RFC 1950), and `framecrc`'s per-packet variant seeds it
//! `(a=0, b=0)` rather than the standard `(a=1, b=0)`. Real CRC-32 is what
//! `-hash crc32` selects on the generic `hash`/`framehash`/`streamhash`
//! family. See `crate::algo`'s module docs for how this was established (two
//! probes disagreeing by a small, structured amount, not a coin flip) and
//! `docs/format/vaco-mux-hash.md` for the full transcript.
//!
//! # Layout
//!
//! | Module | Registrations | Shape |
//! |---|---|---|
//! | [`whole`] | `crc`, `md5`, `hash` | one line for the whole file |
//! | [`frame`] | `framecrc`, `framemd5`, `framehash` | header block, one line per packet |
//! | [`stream`] | `streamhash` | one line per stream, no header |
//!
//! # `uncodedframecrc` is not registered here
//!
//! It hashes *decoded frames*, not packets, and needs per-frame geometry
//! (width/height/pixel format for video; sample format/layout/count for
//! audio) that in the reference comes from the `AVFrame` itself, not the
//! stream. [`vaco_format_core::Muxer::write_packet`] receives a
//! [`vaco_packet::Packet`] and the [`vaco_codec_core::CodecParameters`] frozen
//! at [`vaco_format_core::Muxer::add_stream`] — no per-call frame geometry,
//! and no guarantee a packet's bytes are a stride-free, tightly packed plane
//! the way an `AVFrame` filled a raw encoder's packet in the reference. Doing
//! this properly needs either a frame-level hook this trait does not have, or
//! a documented assumption ("payloads are always tightly packed, geometry
//! never changes mid-stream") this crate is not positioned to make on the
//! trait's behalf. Per the brief for issue #572: implement the seven packet
//! muxers, report the gap precisely, leave `uncodedframecrc` open. It is not
//! registered in `vaco-component.toml` and there is no module for it.

#![forbid(unsafe_code)]

/// The checksum algorithms, re-exported from their single owner.
///
/// This used to be a module in this crate. `vaco-probe` had a near-identical
/// one — same fifteen names, same labels, its own enum spelled `HashAlg` — and
/// both crates declared `crc`, `md-5`, `sha1` and `sha2` directly, which
/// `cargo xtask owner-gate` reported as a D11 violation. The merge is
/// `crates/core/vaco-hash`.
///
/// It matters more than duplication usually does: here the checksum **is** the
/// printed output, and `framemd5` is one of the differential harness's own
/// oracles (D6). An oracle with a private copy of the algorithm is not one.
pub use vaco_hash as algo;
pub mod frame;
pub mod header;
pub mod stream;
pub mod whole;

use vaco_codec_core::CodecId;
use vaco_format_core::{Muxer, MuxerDesc};

use algo::HashAlgo;
use frame::FrameHashMuxer;
use stream::StreamHashMuxer;
use whole::WholeHashMuxer;

/// Every registration in this crate declares the same defaults: measured,
/// `ffmpeg -h muxer=<name>` prints `Default video codec: rawvideo.` and
/// `Default audio codec: pcm_s16le.` for all seven (and for `uncodedframecrc`,
/// which this crate does not register — see the crate docs).
const DEFAULT_VIDEO: Option<CodecId> = Some(CodecId::Rawvideo);
const DEFAULT_AUDIO: Option<CodecId> = Some(CodecId::PcmS16le);

/// `crc`: whole-file Adler-32 (see [`whole`]), `CRC=0x%08x`.
pub const MUXER_CRC: MuxerDesc = MuxerDesc {
    name: "crc",
    long_name: "CRC testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(WholeHashMuxer::crc(sink)?) as Box<dyn Muxer>),
};

/// `md5`: whole-file MD5, `MD5=<hex>`.
pub const MUXER_MD5: MuxerDesc = MuxerDesc {
    name: "md5",
    long_name: "MD5 testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(WholeHashMuxer::md5(sink)?) as Box<dyn Muxer>),
};

/// `hash`: whole-file digest, `<ALGO>=<hex>`. This registration's `open`
/// fixes the algorithm at the reference's own default (SHA-256); there is no
/// options channel from [`MuxerDesc::open`] to select another one (see
/// `docs/format/vaco-mux-hash.md`) — a caller wanting `-hash <other>`
/// constructs [`WholeHashMuxer::hash`] directly instead of going through this
/// descriptor.
pub const MUXER_HASH: MuxerDesc = MuxerDesc {
    name: "hash",
    long_name: "Hash testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(WholeHashMuxer::hash(sink, HashAlgo::Sha256)?) as Box<dyn Muxer>),
};

/// `framecrc`: one line per packet, bespoke per-packet Adler-32 (see
/// [`frame`]).
pub const MUXER_FRAMECRC: MuxerDesc = MuxerDesc {
    name: "framecrc",
    long_name: "framecrc testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(FrameHashMuxer::framecrc(sink)?) as Box<dyn Muxer>),
};

/// `framemd5`: one line per packet, MD5.
pub const MUXER_FRAMEMD5: MuxerDesc = MuxerDesc {
    name: "framemd5",
    long_name: "Per-frame MD5 testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(FrameHashMuxer::framemd5(sink)?) as Box<dyn Muxer>),
};

/// `framehash`: one line per packet, any algorithm (fixed at SHA-256 through
/// this descriptor — see [`MUXER_HASH`]'s doc for why).
pub const MUXER_FRAMEHASH: MuxerDesc = MuxerDesc {
    name: "framehash",
    long_name: "Per-frame hash testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(FrameHashMuxer::framehash(sink, HashAlgo::Sha256)?) as Box<dyn Muxer>),
};

/// `streamhash`: one line per stream, no header (see [`stream`]).
pub const MUXER_STREAMHASH: MuxerDesc = MuxerDesc {
    name: "streamhash",
    long_name: "Per-stream hash testing",
    extensions: &[],
    default_video: DEFAULT_VIDEO,
    default_audio: DEFAULT_AUDIO,
    open: |sink| Ok(Box::new(StreamHashMuxer::new(sink, HashAlgo::Sha256)?) as Box<dyn Muxer>),
};

/// Every muxer this crate registers.
#[must_use]
pub fn all_muxers() -> Vec<&'static MuxerDesc> {
    vec![
        &MUXER_CRC,
        &MUXER_MD5,
        &MUXER_HASH,
        &MUXER_FRAMECRC,
        &MUXER_FRAMEMD5,
        &MUXER_FRAMEHASH,
        &MUXER_STREAMHASH,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_seven_registrations() {
        assert_eq!(all_muxers().len(), 7);
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

    #[test]
    fn every_descriptor_opens() {
        use vaco_format_core::vacoraw::MemorySink;
        for desc in all_muxers() {
            let sink = Box::new(MemorySink::new());
            assert!((desc.open)(sink).is_ok(), "{} failed to open", desc.name);
        }
    }
}
