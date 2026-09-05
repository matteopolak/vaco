//! JPEG coded bitstream syntax (D-21b): marker segments and scan structure,
//! read and write.
//!
//! # What this is
//!
//! Every JPEG stream (ITU-T T.81) is a sequence of marker segments —
//! `0xFF <marker>` optionally followed by a 2-byte big-endian length and that
//! many bytes of payload — with one exception: after `SOS` (start of scan),
//! raw entropy-coded bytes run until the next marker that is not a restart
//! marker (`RST0`..`RST7`) and not a byte-stuffed `0xFF 0x00`. This crate
//! splits a file into exactly that structure ([`cbs::JpegCbs::split`]) and
//! reassembles it byte for byte ([`cbs::JpegCbs::assemble`]).
//!
//! JPEG's length-delimited segments make the split exact before payload parsing;
//! [`header`] adds typed, byte-for-byte access to `SOF0`/`SOF2`, `DQT`, and `DHT`.
//!
//! This crate depends on neither `vaco-codec-jpeg` nor its marker table
//! (`pub(crate)`, not reachable from outside that crate) — the handful of
//! marker byte values here are ITU-T T.81 Table B.1 constants, reproduced
//! independently rather than exposed across a crate boundary for six bytes.
//!
//! # How to change it
//!
//! Add a variant to [`cbs::JpegContent`] and its read/write pair in
//! `header.rs` for a fourth segment worth typing (`APP14`'s Adobe transform
//! field, say); everything else stays [`cbs::JpegContent::Raw`]. Add a real
//! `ffmpeg`-produced fixture and check it round-trips byte for byte before
//! trusting a change to field order.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for byte-oriented reading and writing; `vaco-codec-cbs`
//! for the `CbsCodec`/`Cbs` shape this crate fills in.

#![forbid(unsafe_code)]

pub mod cbs;
pub mod header;

pub use cbs::{JpegCbs, JpegContent, JpegFraming};
