//! IMF (SMPTE ST 2067) demuxer.
//!
//! Layer 4. See the crate-level docs in `docs/format/vaco-format-imf.md`
//! for the full account; this comment covers only the module map.

#![forbid(unsafe_code)]

pub mod assetmap;
pub mod cpl;
pub mod demux;
pub mod fsio;
pub mod package;
pub mod pkl;
pub mod xml;

pub use demux::{DEMUXER, ImfDemuxer};
