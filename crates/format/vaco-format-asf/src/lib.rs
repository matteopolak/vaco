//! The shared ASF (Advanced Systems Format) object model: GUIDs, the object
//! header walk, and codec-ID mapping — everything `vaco-demux-asf` and
//! `vaco-mux-asf` would otherwise each define their own copy of (D19).
//!
//! # Source
//!
//! Microsoft, *"Advanced Systems Format (ASF) Specification"*, Revision
//! 01.20.06 (the publicly published specification, distributed through
//! Microsoft's Open Specifications programme). Cited throughout this crate
//! and its two sibling crates as `[ASF] §N.N`. This is a clean-room
//! implementation from that document (D7/D15): `~/repos/FFmpeg` was not
//! opened, and every byte layout below traces to a table in the spec text,
//! not to any implementation's source or headers.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`guid`] | [`guid::Guid`] — the 128-bit little-endian-ish object identifier |
//! | [`well_known`] | Every standard GUID `[ASF] §10` names |
//! | [`object`] | [`object::ObjectHeader`]/[`object::ObjectIter`] — the 24-byte object prefix and the walk over a run of them |
//! | [`codec`] | Codec-ID mapping for the Audio/Video Media Types, bridging `vaco-format-riff` |
//!
//! # Why this crate exists rather than living in the demuxer
//!
//! Both `vaco-demux-asf` and `vaco-mux-asf` need the same GUID table and the
//! same object-header arithmetic — a mux round-trip test that builds bytes
//! with one crate's idea of a GUID and reads them back with another's is not
//! testing anything. D19 ("one definition per concept") is exactly this
//! situation, the same way `vaco-format-riff` is `vaco-demux-avi` and
//! `vaco-mux-avi`'s shared chunk grammar.
//!
//! # What this crate deliberately does not do
//!
//! It does not read or write files. There is no `IoContext` dependency here at
//! all — only pure functions over byte slices and `Vec<u8>` builders, so the
//! two sibling crates decide how (and how much of) an ASF file to hold in
//! memory at once.

#![forbid(unsafe_code)]

pub mod codec;
pub mod guid;
pub mod object;
pub mod well_known;

pub use guid::Guid;
pub use object::{Object, ObjectHeader, ObjectIter};
