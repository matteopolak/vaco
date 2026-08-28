//! VP9 coded bitstream syntax (D-21a): the uncompressed header and the
//! superframe index, read and write.
//!
//! # What this is, and how it differs from `vaco-parse-vpx`
//!
//! `vaco-parse-vpx::vp9` reads just enough of `uncompressed_header()` to
//! populate `CodecParameters` and stops. This crate reads the *whole* header
//! — loop filter, quantisation, segmentation and tile parameters included —
//! because a coded-bitstream-syntax layer needs the one thing that partial
//! reader has no use for: the exact byte offset the header ends at, so
//! everything past it (the compressed header and tile data, opaque
//! boolean-arithmetic-coded bytes) can be carried through unedited.
//!
//! This crate depends on nothing from `vaco-parse-vpx` or `vaco-codec-vp9`
//! deliberately — both were under active ownership elsewhere in the tree
//! while this was written, and [`crate::header`] needs a strictly larger
//! parse than either already has (see that module's own doc). The
//! superframe-index algorithm in [`crate::superframe`] is consequently a
//! second, small implementation of the same ~20-line algorithm
//! `vaco-parse-vpx::superframe::last_subframe` already has — a known,
//! documented duplicate to resolve in a future consolidation pass once both
//! crates are free, not a discovery made and ignored.
//!
//! # How it works
//!
//! [`header::Vp9Header`] is [`vaco_codec_cbs::CbsCodec::Content`] for
//! [`cbs::Vp9Cbs`]; [`superframe`] provides the framing
//! ([`vaco_codec_cbs::CbsCodec::Framing`]) that splits one container sample
//! into its constituent coded frames. See each module's own doc for the
//! syntax it covers and how it was verified.
//!
//! # How to change it
//!
//! Add a field to [`header::FrameHeader`] and its read/write pair in
//! `header.rs`; add a fixture (ideally real `libvpx-vp9` output, captured via
//! `ffmpeg -c:v libvpx-vp9` to an IVF file and read back with a small script —
//! see that module's test doc) and check it round-trips byte for byte before
//! trusting a change to the field order.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for reading and writing; `vaco-codec-cbs` for the
//! `CbsCodec`/`Cbs` shape this crate fills in.

#![forbid(unsafe_code)]

pub mod cbs;
pub mod header;
pub mod superframe;

pub use cbs::{Vp9Cbs, Vp9Content};
pub use header::{FrameHeader, Vp9Header};
