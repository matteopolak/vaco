//! The EBML layer, format-agnostic: element IDs, variable-length integers,
//! the element header, a reader over both an in-memory slice and a seekable
//! stream, and a writer for both.
//!
//! # Why this crate exists
//!
//! Two containers in this workspace are built on EBML — Matroska/`WebM`
//! (`vaco-demux-matroska`, `vaco-mux-matroska`) — and RFC 8794 itself is a
//! generic binary container format with exactly one other implementation
//! detail worth sharing across them: **the schema is not part of EBML**.
//! RFC 8794 defines the VINT grammar, the element header, and the
//! unknown-size termination rule (section 6.2); which element IDs exist,
//! which element may contain which, and what each one means is a property of
//! the format built on top — Matroska's own element tree, defined by RFC 9559
//! and kept in `vaco-demux-matroska::ebml::schema`.
//!
//! That split is what this crate's own boundary follows: everything here
//! operates on a bare `u32` element ID and never asks what it means.
//! [`stack::Stack::terminations_for`] is the clearest example — the
//! mechanism (walk outward from the innermost open frame) is generic, but the
//! answer to "is this ID a legal child of that one" is supplied by the
//! caller's own schema as a closure, not looked up here.
//!
//! # What moved here, and from where
//!
//! Prior to this crate, `vaco-demux-matroska::ebml` held the whole EBML
//! layer inline, deliberately kept schema-free "so that it can be promoted to
//! `vaco-format-ebml` unchanged if a Matroska muxer ... wants it" (that
//! crate's own module docs, written before this one existed). `vaco-mux-matroska`
//! is that muxer. `vaco-demux-matroska::ebml` now re-exports the generic
//! pieces from here and keeps only the Matroska schema table and the
//! functions that read it (D19: one definition of the VINT and header
//! grammar, not two).
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`vint`] | VINT encode/decode: element IDs, data sizes, the unknown-size marker, and the signed flavour Matroska lacing borrows |
//! | [`element`] | [`Header`], [`Caps`] (the two length ceilings an `EBML` header may lower), and the depth cap |
//! | [`reader`] | [`reader::Slice`] (in-memory child walker) and [`reader::read_header`] (one header from a stream), plus the RFC 8794 section 7 value accessors |
//! | [`writer`] | Building elements: whole ones into a `Vec<u8>`, or a bare header streamed to an [`vaco_io::IoWriter`] for a body too large to buffer |
//! | [`stack`] | [`stack::Stack`] — the open-element stack that makes RFC 8794 section 6.2 (unknown-size termination) implementable |
//!
//! Every public item is re-exported at the crate root, so `vaco_format_ebml::Slice`
//! and `vaco_format_ebml::reader::Slice` name the same type — callers migrating
//! from the inline copy in `vaco-demux-matroska` do not need to learn a new path
//! shape.
//!
//! # Bounds
//!
//! Everything here is driven by attacker-controlled byte counts:
//!
//! * [`vint::MAX_ID_LEN`] and [`vint::MAX_SIZE_LEN`] cap what [`reader::read_header`]
//!   and the [`reader::Slice`] walker will read regardless of what a header
//!   declares; [`element::Caps::adopt`] rejects a declaration wider than the
//!   cap rather than clamping it.
//! * [`reader::Slice::children`] is a flat iterator; nesting is the caller's
//!   business, recursing with an explicit depth checked against
//!   [`element::MAX_DEPTH`] or a caller-chosen bound.
//! * [`stack::Stack`] has a fixed frame ceiling ([`stack::Stack::MAX_FRAMES`])
//!   and cannot grow past it.
//!
//! # Specification
//!
//! RFC 8794 (EBML) in full; the two RFC 9559 sections that are really just
//! "how Matroska instantiates the generic size-delta VINT" — 10.3.3's lacing
//! delta — live in [`vint::read_signed_vint`]/[`vint::signed_vint`] because
//! they are a use of the generic grammar, not a new one, even though the
//! specific bias only means something to a Matroska lace.
//!
//! # Configuration
//!
//! None — this crate has no options. A caller wanting a narrower depth or
//! frame cap than [`element::MAX_DEPTH`]/[`stack::Stack::MAX_FRAMES`] tracks
//! its own counter alongside these types rather than configuring them.
//!
//! # Dependencies
//!
//! `vaco-core` for [`vaco_core::Error`]/[`vaco_core::Result`], and `vaco-io`
//! for [`vaco_io::IoContext`]/[`vaco_io::IoWriter`] — the two stream types the
//! header reader and the streaming writer helper operate on.

#![forbid(unsafe_code)]

pub mod element;
pub mod reader;
pub mod stack;
pub mod vint;
pub mod writer;

pub use element::{Caps, Header, MAX_DEPTH};
pub use reader::{Child, Children, Slice, as_float, as_int, as_str, as_uint, read_header};
pub use stack::{Frame, Stack};
pub use vint::{
    MAX_ID_LEN, MAX_SIZE_LEN, Size, all_ones, id_bytes, read_id, read_signed_vint, read_size,
    signed_vint, vint, vint_len, vint_min, vint_unknown,
};
pub use writer::{
    binary, element as write_element, element_unknown_size, float as write_float, int as write_int,
    patch_known_size, string as write_string, uint as write_uint, write_header,
    write_header_unknown,
};
