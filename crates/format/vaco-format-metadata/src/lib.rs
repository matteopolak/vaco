//! Metadata/chapter/program/stream-group model, and the `MetadataConv`
//! driver.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`keys`] | The canonical generic metadata key names, e.g. `"title"`, `"artist"` |
//! | [`conv`] | [`conv::MetadataConv`] — one container's key-name table — and the driver that applies one |
//! | [`stream_group`] | re-export of [`vaco_format_core::stream_group`] |
//!
//! # What is *not* in here
//!
//! [`vaco_format_core::Program`] and [`vaco_format_core::Chapter`] already
//! carry the generic program/chapter model — `FW-01` built them, and every
//! container's `Demuxer::programs`/`chapters` already returns them. This
//! crate re-exports both rather than defining a second, incompatible
//! `Program`/`Chapter` under a different name; see D19.
//!
//! `StreamGroup` is the same story now: it started here, and moved to
//! [`vaco_format_core::stream_group`] the day `Demuxer::stream_groups()`
//! joined the trait. The re-export keeps this crate's path working.
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

pub use vaco_format_core::stream_group;

pub use conv::{ConvEntry, Direction, MetadataConv};
pub use vaco_format_core::{
    Chapter, Program, StreamGroup, StreamGroupIndex, StreamGroupKind, TileGrid,
};
