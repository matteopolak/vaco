//! AC-3 / E-AC-3 syncframe and BSI (bitstream information) parsing.
//!
//! Shared by `vaco-demux-raw::ac3` (which needs only frame length, sample
//! rate and a channel count) and `vaco-codec-ac3` (which needs every BSI
//! field to know exactly where the audio blocks start). Mirrors
//! `vaco-format-mpegaudio`'s role for the MPEG audio family: one header
//! parser two callers share, rather than the demuxer re-deriving what the
//! decoder already parses precisely.
//!
//! `vaco-demux-raw::ac3` predates this crate and keeps its own minimal inline
//! copy of the syncframe-length arithmetic rather than depending on it — see
//! the follow-up to unify them, tracked outside this source tree.

#![forbid(unsafe_code)]

pub mod bsi;
pub mod syncinfo;
pub mod tables;

pub use bsi::{Bsi, BsiError};
pub use syncinfo::{FrameKind, SyncInfo};
