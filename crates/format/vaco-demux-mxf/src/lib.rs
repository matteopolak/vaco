//! The MXF (Material eXchange Format) demuxer.
//!
//! # Source
//!
//! SMPTE ST 377-1 (file format: KLV, partitions, header metadata, index
//! tables, the primer pack), ST 379-1 (the Generic Container), ST 386
//! (the D-10 mapping), ST 390 (OP-Atom), ST 336 (the KLV encoding protocol),
//! RP 210 (the metadata dictionary the Universal Labels come from). Clean
//! room from those documents (D7/D15), cross-checked against real files
//! `ffmpeg 8.1` wrote (`ffmpeg -f lavfi -i testsrc=... -f mxf out.mxf`, `-f
//! mxf_opatom`) per D6/D17 — every place a value was measured rather than
//! read from the spec text says so in its own module's docs.
//!
//! # One crate, four layers
//!
//! MXF's layers are not separable: the index table refers to the essence
//! container, which refers to the structural metadata, which is keyed by
//! the KLV primer. Splitting them across crates would mean four crates that
//! cannot be understood or tested apart, which is why this is one crate for
//! all four.
//!
//! | Layer | Module | What it owns |
//! |---|---|---|
//! | KLV | [`ber`], [`klv`], [`partition`], [`primer`] | BER lengths, one Key-Length-Value triplet, the Partition Pack and Random Index Pack, the local-tag → UL primer table |
//! | Structural metadata | [`localset`], [`properties`], [`metadata`], [`descriptor`] | The `Tag Length Value` item form header metadata and index tables share; the RP210 property dictionary; the instance-UID-keyed graph (`Preface` → `ContentStorage` → `Package` → `Track` → `Sequence` → `StructuralComponent`); turning a descriptor into `CodecParameters` |
//! | Essence containers | [`essence`] | Generic Container essence element keys, frame-wrapped vs clip-wrapped, the track-number match that binds an essence element to a `Track` |
//! | Index tables / demux | [`index`], [`demux`] | The Index Table Segment (CBE and VBE), and [`demux::MxfDemuxer`], which drives all three layers below it |
//!
//! [`ul`] sits underneath all four: the 16-byte Universal Label type and
//! every well-known key this crate recognises.
//!
//! # Scope
//!
//! Read [`demux`]'s module docs and this crate's closing report (in the
//! commit that introduced it) for exactly what is measured, what is
//! spec-derived and unexercised, and what is out of scope entirely — OP-Atom
//! is supported as "one essence track per file", not as "discover the
//! sibling files a real OP-Atom edit is split across"; D-10 is supported
//! through the same Generic Container code path as everything else, without
//! its own fixed-frame-size fast path, because this crate could not produce
//! a byte-correct D-10 sample with the installed `ffmpeg 8.1` to check one
//! against (documented, not hidden, in the closing report).
//!
//! ```no_run
//! use vaco_demux_mxf::MxfDemuxer;
//! use vaco_format_core::Demuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes = std::fs::read("clip.mxf").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = MxfDemuxer::open(src, &NoParsers)?;
//! for s in demux.streams() {
//!     println!("{:?} {:?}", s.media_type(), s.params.codec_id);
//! }
//! let _ = demux.read_packet();
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod ber;
pub mod demux;
pub mod descriptor;
pub mod essence;
pub mod index;
pub mod klv;
pub mod localset;
pub mod metadata;
pub mod partition;
pub mod primer;
pub mod properties;
pub mod ul;

#[cfg(test)]
mod testutil;

pub use demux::MxfDemuxer;

use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider};
use vaco_io::MediaSource;

/// Behavioural flags, reachable through `DemuxerDesc::flags`.
///
/// `SHOW_IDS`: a Track's `TrackID` is a real container-stated identifier
/// this crate reports as [`vaco_format_core::Stream::id`] — the same
/// rationale `vaco-demux-mp4` gives for its own `track_ID`. No other flag
/// applies: MXF states its own edit-unit-indexed timestamps (neither
/// `NOTIMESTAMPS` nor `TS_DISCONT`), has real picture dimensions, and a
/// valid file always has at least one stream.
pub const FLAGS: FormatFlags = FormatFlags::SHOW_IDS;

/// [`FLAGS`], as a function — kept so `DemuxerDesc::flags` and any caller
/// wanting the value without the registry can share one definition (D19).
#[must_use]
pub const fn format_flags() -> FormatFlags {
    FLAGS
}

/// Extensions and MIME types `ffprobe 8.1` associates with MXF.
const EXTENSIONS: &[&str] = &["mxf"];
const MIME_TYPES: &[&str] = &["application/mxf"];

/// The registry descriptor. Named by `vaco-component.toml`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mxf",
    long_name: "MXF (Material eXchange Format)",
    extensions: EXTENSIONS,
    mime_types: MIME_TYPES,
    flags: FLAGS,
    probe,
    open: open_boxed,
};

/// How many bytes of a variable-offset run-in (ST 377-1 §6.2, spec-derived —
/// see [`probe`]'s docs) this crate will scan before giving up.
const MAX_RUN_IN_SCAN: usize = 65536;

/// Content probe.
///
/// A real file starts with the Header Partition Pack key at offset 0 —
/// measured against `out.mxf` and `opatom.mxf` alike, and it is why this
/// returns [`ProbeScore::MAGIC_CHECKED`] there, matching `ffprobe 8.1`'s
/// measured `probe_score=100` for both.
///
/// The spec additionally permits up to 64 KiB of arbitrary "run-in" bytes
/// before that key (§6.2), for embedding an MXF file inside another
/// container's framing. No file in this crate's corpus uses one, so the
/// fallback scan below is spec-derived and unexercised, scored at
/// [`ProbeScore::VARIABLE_OFFSET`] rather than the full magic score.
fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if key_at(data, 0).is_some_and(ul::Ul::is_any_partition_pack) {
        return ProbeScore::MAGIC_CHECKED;
    }
    let scan_limit = data.len().min(MAX_RUN_IN_SCAN);
    for offset in 1..scan_limit {
        if key_at(data, offset).is_some_and(ul::Ul::is_any_partition_pack) {
            return ProbeScore::VARIABLE_OFFSET;
        }
    }
    ProbeScore::NONE
}

fn key_at(data: &ProbeData<'_>, offset: usize) -> Option<ul::Ul> {
    let mut bytes = [0u8; 16];
    for (i, slot) in bytes.iter_mut().enumerate() {
        *slot = data.get(offset.checked_add(i)?)?;
    }
    Some(ul::Ul::new(bytes))
}

fn open_boxed(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(MxfDemuxer::open(src, parsers)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn probe_scores_a_real_header_partition_pack_at_offset_zero() {
        let bytes = testutil::header_partition_pack_bytes();
        let data = ProbeData::new(&bytes);
        assert_eq!(probe(&data), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn probe_rejects_prose() {
        let data = ProbeData::new(b"Four score and seven years ago our fathers brought forth");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_only_ever_returns_a_value_from_the_convention_table() {
        for bytes in [
            &b""[..],
            b"\0",
            &testutil::header_partition_pack_bytes(),
            b"not mxf at all",
        ] {
            let data = ProbeData::new(bytes);
            let score = probe(&data);
            assert!(
                score == ProbeScore::NONE
                    || score == ProbeScore::MAGIC_CHECKED
                    || score == ProbeScore::VARIABLE_OFFSET,
                "unexpected score {score:?}"
            );
        }
    }
}
