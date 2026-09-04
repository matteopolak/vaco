//! The MP4/MOV muxer: `ftyp`/`moov`/`mdat`, `-movflags faststart`, fragmented
//! output (`moof`/`traf`/`trun`), `sidx`, `mfra`, iTunes-style metadata and the
//! brand-variant containers (`ipod`, `ismv`, `f4v`, `psp`, `3gp`, `3g2`, `avif`).
//!
//! Box *bytes* are not defined here — `vaco-format-isom::writer` owns every box
//! layout this crate emits (D19: one definition per concept, shared with the
//! reader that already exists for each of them). This crate's job is entirely
//! about *when* to write what: accumulating samples into tables, choosing chunk
//! boundaries, deciding a fragment boundary, and driving the two-pass rewrite
//! `faststart` needs.
//! Video: H.264 (`avcC`), HEVC (`hvcC`), AV1 (`av1C`), VP8/VP9 (`vpcC`), MJPEG.
//! Audio: AAC (`esds`), Opus (`dOps`), FLAC (`dfLa`), MP3 (no config box, per
//! convention). `CodecParameters::extradata` is used **verbatim** as the
//! decoder configuration record for every one of these except H.264 and
//! HEVC, where an Annex-B-shaped buffer is first rewritten into a real
//! `avcC`/`hvcC` (`mux::resolve_nal_config`, which also decides whether the
//! samples need reframing — the two are one decision). A caller whose stream
//! has no extradata at all gets `check_bitstream`'s `extract_extradata`
//! request, same as any other `GLOBALHEADER` container.
//!
//! Progressive muxing: full sample tables (`stsd`/`stts`/`ctts`/`stsc`/
//! `stsz`/`stco`|`co64`/`stss`), chunked interleave, trailer rewrite,
//! `-movflags faststart`. Fragmented muxing: `moof`/`traf`/`tfhd`/`tfdt`/
//! `trun`, `empty_moov`, `default_base_moof`, `omit_tfhd_offset`,
//! `separate_moof`, `frag_keyframe`, `frag_every_frame`, `frag_duration`,
//! `frag_size`, `mfra`, and a buffered `sidx` for `dash`/`cmaf`. Metadata:
//! `udta ▸ meta ▸ ilst` iTunes tags, `covr` cover art, Nero (`chpl`)
//! chapters plus the `tref ▸ chap` reference, and the brand/compatible-brand
//! lists for `mp4`, `mov`, `ipod`, `ismv`, `f4v`, `psp`, `3gp`, `3g2` and
//! `avif`.
//! Common Encryption (`pssh`/`saiz`/`saio`/`senc`/`tenc`) is not implemented;
//! a caller asking for it gets [`vaco_core::Error::Unsupported`]. PCM and
//! AC-3/E-AC-3 have no sample-entry mapping yet. See
//! `docs/format/vaco-mux-mp4.md` for the complete list.

#![forbid(unsafe_code)]

pub mod brand;
pub mod entry;
pub mod fragmented;
pub mod meta;
pub mod mux;
pub mod options;
pub mod progressive;
pub mod track;

pub use brand::{
    MUXER_3G2, MUXER_3GP, MUXER_AVIF, MUXER_F4V, MUXER_IPOD, MUXER_ISMV, MUXER_MOV, MUXER_MP4,
    MUXER_PSP,
};
pub use mux::MovMuxer;
pub use options::{Brand, MovFlags, MuxOptions};
