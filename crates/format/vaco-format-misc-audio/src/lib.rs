//! Demuxers for legacy compressed-audio containers and the simple T3 audio
//! container tail: `wv`, `tta`, headerless ITU/3GPP speech codecs, `amr`,
//! `adx`, and a couple of plain fixed-header PCM wrappers.
//!
//! See `docs/format/vaco-format-misc-audio.md` for the family table and what
//! was deliberately left out.

#![forbid(unsafe_code)]

pub mod adx;
pub mod amr;
pub mod bfstm;
pub mod block;
pub mod brstm;
pub mod g723;
pub mod nistsphere;
pub mod pvf;
pub mod protracker;
pub mod qoa;
pub mod rawcodec;
pub mod sbc;
pub mod svag;
pub mod tta;
pub mod vag;
pub mod wavpack;
pub mod xa;
pub mod xm;
pub mod xwma;
