//! RTP depacketisers: turn a sequence of RTP payloads into complete access
//! units.
//!
//! # The actual count, versus "26"
//!
//! FM-41 asked for the count to be measured, not assumed, against two
//! authorities: `ffmpeg -h demuxer=rtp` and the RTP payload registry. The
//! first one turned out not to help — `ffmpeg -h demuxer=rtp` (see the
//! transcript in `vaco-demux-rtsp`'s crate docs) prints only the `rtp`
//! demuxer's own generic `AVOption`s (`rtp_flags`, `localaddr`,
//! `allowed_media_types`, ...); it does not enumerate the per-codec
//! depacketiser table at all, because that table lives inside the demuxer's
//! *implementation*, not its documented option surface. Reaching it would
//! mean either reading `FFmpeg` source (D7 forbids this) or black-box-probing
//! every candidate `rtpmap` encoding name against a live session, which
//! needs a real RTP stream per codec to get past SDP parsing into the
//! depacketiser itself — expensive enough, for 30+ candidates, that it was
//! not attempted for this pass.
//!
//! So this module's count is measured against the second authority instead:
//! RFC 3551's static table (`crate::payload`) plus the dynamic-payload RFCs
//! this crate's brief itself cites. **This crate implements depacketisers
//! resolving 22 distinct [`CodecId`]s** (see `registry::for_encoding`'s
//! match arms — that function is the actual count, this comment is a
//! transcription of it and could drift, so re-count there if it matters),
//! plus two structural special cases that are not a `CodecId` at all
//! (`MP2T`, which re-wraps a nested MPEG transport stream rather than
//! naming one codec, and RFC 2198 redundancy, which is a wrapper around
//! another payload type rather than a codec of its own) — 24 payload-type
//! mappings in total, across 13 implementation modules (several `CodecId`s
//! share one module: `mpeg12` covers both MPV and MPA, `xiph` covers both
//! Vorbis and Theora, `raw` covers every payload whose RTP framing is "one
//! packet, one frame, no header": PCMU, PCMA, L16, Opus, Speex, AMR,
//! AMR-WB, AC-3, and — via the same `Identity` type, since RFC 4587 does
//! not use a depacketiser-visible header either — H.261). That is not 26,
//! and the difference is almost entirely codecs this table cannot name at
//! all yet — see "Missing `CodecId` variants" below.
//!
//! | Module | RFC | `CodecId` |
//! |---|---|---|
//! | [`raw`] | 3551 (framing), various | `PcmMulaw`, `PcmAlaw`, `PcmS16be` (L16), `Opus`, `Ac3`, `AmrNb`, `AmrWb`, `Speex`, `H261` |
//! | [`mpeg12`] | 2250 | `Mpeg1video`/`Mpeg2video` (MPV), `Mp2` (MPA) |
//! | [`h263`] | 4629 | `H263` |
//! | [`h264`] | 6184 | `H264` |
//! | [`hevc`] | 7798 | `Hevc` |
//! | [`vp8`] | 7741 | `Vp8` |
//! | [`vp9`] | draft-ietf-payload-vp9 | `Vp9` |
//! | [`av1`] | AOM "RTP Payload Format For AV1" | `Av1` |
//! | [`aac`] | 3640 (`MPEG4-GENERIC`, generic mode) | `Aac` |
//! | [`xiph`] | 5215 | `Vorbis`, `Theora` |
//! | [`jpeg`] | 2435 | `Jpeg` |
//! | [`rawvideo`] | 4175 | `Rawvideo` |
//! | `mp2t` (in [`registry`]) | 2250 §2 (MPEG-2 TS over RTP) | none — see below |
//! | [`red`] | 2198 | wraps another entry, not a codec of its own |
//!
//! ## Missing `CodecId` variants
//!
//! `vaco-codec-core::CodecId` is `#[non_exhaustive]`, and per that crate's
//! own comment only it may add a variant — this crate does not own it and
//! does not attempt to. The following RFC 3551/3551-adjacent payloads have
//! **no implementation here** because there is no `CodecId` to hand a
//! depacketised frame to, not because the framing is hard:
//!
//! | Encoding | RFC 3551 PT | Would need |
//! |---|---|---|
//! | GSM | 3 | `CodecId::Gsm` |
//! | G722 | 9 | `CodecId::G722` |
//! | G728 | 15 | `CodecId::G728` |
//! | G729 | 18 | `CodecId::G729` |
//! | QCELP | 12 | `CodecId::Qcelp` |
//! | DVI4 | 5/6/16/17 | `CodecId::AdpcmImaRtp` (or reuse of an existing ADPCM variant with matching block layout — RTP's DVI4 has no block header, unlike `AdpcmImaWav`) |
//! | `CelB` | 25 | `CodecId::CelB` |
//! | DV (RFC 6469) | dynamic | `CodecId::DvVideo` |
//! | iLBC (RFC 3952) | dynamic | `CodecId::Ilbc` |
//!
//! `Mp2` is used for RFC 2250's `MPA` payload (PT 14) as the closest
//! existing variant — RFC 2250's audio header does not distinguish MPEG
//! Layer I/II/III at the RTP framing level (the layer is in the frame's own
//! header), so a real deployment mixing layers would misreport; documented
//! rather than silently assumed correct.
//!
//! `MP2T` (PT 33, RFC 2250 §2) is not a `CodecId` at all: the RTP payload is
//! a run of complete 188-byte MPEG-2 TS packets, and the right thing to do
//! with them is hand them to a nested `vaco-demux-mpegts` instance, the same
//! shape `vaco-demux-hls`'s `SegmentDemuxerProvider` already uses. This
//! crate's [`registry::mp2t_payload`] does the RTP-framing half (extracting
//! the aligned TS packets) and stops there — `vaco-demux-rtsp` documents the
//! nested-demux composition gap and why it is deferred.

use vaco_core::Result;

pub mod aac;
pub mod av1;
pub mod h263;
pub mod h264;
pub mod hevc;
pub mod jpeg;
pub mod mpeg12;
pub mod raw;
pub mod rawvideo;
pub mod red;
pub mod registry;
pub mod vp8;
pub mod vp9;
pub mod xiph;

pub use registry::{DepacketizerFactory, for_encoding};

/// Turns a sequence of RTP payloads (sharing one SSRC / payload type) into
/// complete access units.
///
/// Implementations see every RTP payload **in sequence-number order** —
/// `vaco-demux-rtsp` is responsible for reordering (`-reorder_queue_size`)
/// before calling this. A depacketiser that receives packets out of order
/// may emit a corrupted or incomplete unit; that is expected packet-loss
/// behaviour, not a bug to guard against here (RFC 3550 has no delivery
/// guarantee, and `FormatFlags::TS_DISCONT` on the registered demuxers is
/// what tells the rest of the pipeline "gaps are normal").
pub trait Depacketizer: Send {
    /// Feed one RTP packet's already-header-stripped payload (and the few
    /// header facts framing depends on: the marker bit and the RTP
    /// timestamp). Returns a complete access unit's bytes once one is
    /// ready, `Ok(None)` while still accumulating one (a fragmented NAL, a
    /// multi-packet JPEG scan, ...).
    ///
    /// # Errors
    /// [`vaco_core::Error::InvalidData`] for a payload whose own framing
    /// (fragmentation header, aggregation header, ...) is inconsistent —
    /// never a panic, since every byte here came off the wire.
    fn push(&mut self, marker: bool, timestamp: u32, payload: &[u8]) -> Result<Option<Vec<u8>>>;
}
