//! The ISO base media file format box layer — MP4, MOV, 3GP, CMAF and fMP4.
//!
//! This is **not** the demuxer. `vaco-demux-mp4` is, and it is built on this:
//! everything here is structure, tables and the arithmetic that turns them into
//! answers, with no I/O policy, no packet emission and no opinion about which
//! sample to play next.
//!
//! ```
//! use vaco_format_isom::{IsoFile, build};
//!
//! // A one-track file with two samples in one chunk at offset 400.
//! let spec = build::TrackSpec {
//!     timescale: 1000,
//!     media_duration: 200,
//!     stbl: build::StblSpec {
//!         stts: vec![(2, 100)],
//!         stsc: vec![(1, 2, 1)],
//!         stco: vec![400],
//!         stsz: vec![64, 32],
//!         stss: vec![1],
//!         ..build::StblSpec::default()
//!     },
//!     ..build::TrackSpec::default()
//! };
//! let bytes = build::file(b"isom", 1000, 200, &[spec]);
//!
//! let file = IsoFile::parse(&bytes, 0)?;
//! let movie = file.movie.expect("a moov");
//! let track = movie.tracks.first().expect("a trak");
//!
//! // Sample 1 sits immediately after sample 0 inside the same chunk.
//! let s = track.sample_table.sample(1).expect("sample 1");
//! assert_eq!((s.offset, s.size, s.dts), (464, 32, 100));
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`boxes`] | box headers, flat iteration, the two bounded searches |
//! | [`fourcc`] | [`FourCc`] and the box-type constants |
//! | [`scan`] | locating top-level boxes over [`vaco_io::IoContext`] without reading them |
//! | [`movie`] | `ftyp`, `mvhd`, `trak ▸ mdia ▸ minf ▸ stbl` assembly |
//! | [`stbl`] | the sample tables and the sample → byte-offset mapping |
//! | [`table`] | fixed-stride table views and their decimated summaries |
//! | [`edit`] | `elst` and the presentation ↔ media timeline |
//! | [`frag`] | `mvex`/`moof`/`traf`/`trun`/`sidx`/`tfra` |
//! | [`stsd`] | sample entries, configuration boxes, the codec tables |
//! | [`esds`] | the MPEG-4 descriptor tree |
//! | [`fixed`] | 16.16 / 8.8 / 2.30 and the display matrix |
//! | [`lang`] | the packed ISO-639-2/T language field |
//! | [`probe`] | content scoring, with the measurements it is based on |
//! | [`build`] | fixture construction for tests, benchmarks and fuzz targets |
//! | [`writer`] | production box writers, for `vaco-mux-mp4` |
//!
//! # The three design decisions worth knowing
//!
//! **1. Nothing proportional to the sample count is allocated.** A sample table
//! is a `&[u8]` plus a stride; entry *i* is decoded on demand. Declared counts
//! are clamped against the payload the box actually carries, which is exact
//! because every one of these tables has a fixed entry width. A `stsz` claiming
//! four billion samples in a twelve-byte box describes one sample, not four
//! gigabytes. [`table`] has the arithmetic.
//!
//! **2. Random access is a first-class path, not iteration in disguise.** A
//! seek asks "which sample is at time *t*" and "where is sample *n*" repeatedly,
//! and neither may walk from zero. Both are answered by a binary search over a
//! **decimated prefix sum** whose size is capped by a constant, so query cost
//! and memory are both independent of the input. [`stbl::SampleCursor`] is the
//! separate O(1) sequential path for the demux loop.
//!
//! **3. No recursion is reachable from input.** [`boxes::BoxIter`] is flat, the
//! known tree is a fixed nest of loops, and the two generic searches use an
//! explicit worklist with a depth cap. A megabyte of nested `moov` boxes is a
//! bounded walk, not a stack overflow — there is a test that builds exactly
//! that.
//!
//! # Reference behaviour
//!
//! Where this crate reproduces something `ffmpeg`/`ffprobe` 8.1 does rather
//! than something a specification says, the measurement is recorded next to the
//! code: [`stbl::SampleTable::dts_shift`] (D17 — the reference shifts DTS where
//! the spec shifts composition times), [`movie::Track::reported_duration`],
//! [`edit::EditList::simple_shift`], [`frag`]'s byte-addressing cases and
//! [`probe`]'s score table. `docs/format/vaco-format-isom.md` lists the
//! commands each was obtained with, so they can be re-derived when the pinned
//! reference version moves.

#![forbid(unsafe_code)]

pub mod boxes;
pub mod build;
pub mod edit;
pub mod esds;
pub mod fixed;
pub mod fourcc;
pub mod frag;
pub mod lang;
pub mod movie;
pub mod probe;
pub mod scan;
pub mod stbl;
pub mod stsd;
pub mod table;
pub mod writer;

pub use boxes::{BoxHeader, BoxIter, FullBox, IsoBox};
pub use edit::{EditEntry, EditList, Segment, Timeline};
pub use esds::EsDescriptor;
pub use fixed::DisplayMatrix;
pub use fourcc::FourCc;
pub use frag::{
    FragmentSample, MovieFragment, SampleFlags, SegmentIndex, TrackExtends, TrackFragment,
    TrackFragmentRandomAccess,
};
pub use lang::Language;
pub use movie::{FileType, IsoFile, MediaHeader, Movie, MovieHeader, Track, TrackHeader};
pub use probe::probe;
pub use scan::{BoxSpan, ScanError, TopLevelScanner};
pub use stbl::{Sample, SampleCursor, SampleTable};
pub use stsd::{AudioSampleEntry, CodecConfig, ConfigFlavour, SampleEntry, VisualSampleEntry};
pub use table::{EntryTable, RunIndex};

/// The descriptor a registry would hold for the ISOBMFF family.
///
/// Named as the reference names it, because `-f mp4` has to select it and
/// `format_name` is printed verbatim (D9: interface names are interface facts).
pub const FORMAT_NAME: &str = "mov,mp4,m4a,3gp,3g2,mj2";

/// The long name the reference prints.
pub const FORMAT_LONG_NAME: &str = "QuickTime / MOV";

#[cfg(test)]
pub(crate) mod testutil {
    pub(crate) use crate::build::*;
}
