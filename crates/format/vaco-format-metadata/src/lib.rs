//! Metadata/chapter/program/stream-group model, and the `MetadataConv`
//! driver.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`keys`] | The canonical generic metadata key names, e.g. `"title"`, `"artist"` |
//! | [`conv`] | [`conv::MetadataConv`] — one container's key-name table — and the driver that applies one |
//! | [`stream_group`] | [`stream_group::StreamGroup`] / [`stream_group::StreamGroupKind`] |
//!
//! # What is *not* in here
//!
//! [`vaco_format_core::Program`] and [`vaco_format_core::Chapter`] already
//! carry the generic program/chapter model — `FW-01` built them, and every
//! container's `Demuxer::programs`/`chapters` already returns them. This
//! crate re-exports both rather than defining a second, incompatible
//! `Program`/`Chapter` under a different name; see D19.
//!
//! `StreamGroup` has no such prior definition — it was sketched in the plan
//! (plan 18 §1.1) but never landed in `vaco-format-core`, and nothing there
//! reads one yet. It lives here so a container that wants to report a HEIF
//! `grid` tile set, for instance, has a type to build; wiring
//! `Demuxer::stream_groups()` into the trait itself is a `vaco-format-core`
//! change this crate cannot make, since that crate is not this work's to
//! edit. Recorded, not worked around.
//!
//! # Every container ships its own table; the driver is shared
//!
//! That is this crate's whole division of labour. `MetadataConv` is a table
//! *type* plus a lookup/apply driver; the individual mappings — `ID3v2` frame
//! IDs, `QuickTime` `©xxx` atoms, RIFF `INFO` chunk IDs, Vorbis-comment field
//! names — belong in the container crate that reads or writes them, each as
//! its own `&'static [ConvEntry]`. This crate does not author those tables:
//! a container crate that already has its own mapping (`vaco-format-id3`,
//! for one) is not duplicated here.

#![forbid(unsafe_code)]

pub mod conv;
pub mod keys;
pub mod stream_group;

pub use conv::{ConvEntry, Direction, MetadataConv};
pub use stream_group::{StreamGroup, StreamGroupIndex, StreamGroupKind, TileGrid};
pub use vaco_format_core::{Chapter, Program};
