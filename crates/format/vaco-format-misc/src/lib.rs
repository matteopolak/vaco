//! Game-video, legacy-video and text/metadata containers that share one
//! shape: a small fixed header followed by typed, length-prefixed chunks.
//!
//! See `docs/format/vaco-format-misc.md` for what is implemented, what is
//! demux-only, and why.

#![forbid(unsafe_code)]

pub mod bink;
pub mod cdg;
pub mod ffmetadata;
pub mod flic;
pub mod ivf;
pub mod roq;
pub mod smk;
