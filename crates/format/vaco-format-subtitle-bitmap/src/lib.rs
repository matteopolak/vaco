//! The shared bitmap-subtitle model: one palette shape, one rectangle shape,
//! one indexed-pixel-buffer shape, used by every demuxer/muxer in
//! `vaco-subtitle-bitmap` (`dvbsub`, `dvbtxt`, `sup`/PGS, `vobsub`).
//!
//! # Why this crate exists separately from the demuxers/muxers
//!
//! Four formats share exactly one fact about their output: whatever finally
//! gets painted on screen is a rectangle of pixels, each an index into a
//! palette of at most 256 colours. They share almost nothing about *how they
//! get there* — DVB's run-length grammar, PGS's, and DVD/VobSub's SPU nibble
//! codes are three unrelated compression schemes, each specific to its own
//! format. This crate is that one shared 20% ([`Rect`], [`Palette`],
//! [`IndexedBitmap`]); `vaco-subtitle-bitmap` is the four separate 80%s.
//!
//! # The demuxer/decoder line (read this before anything else)
//!
//! Per `planning/AGENT-CONSTRAINTS.md`, a demuxer recovers packets and their
//! timing; a decoder turns a packet into pixels. Decompressing a run-length
//! pixel string is decoder work — it lives in `crates/codec/`, a later wave —
//! so **no demuxer in this family constructs an [`IndexedBitmap`] with real
//! pixel data**. [`IndexedBitmap`] is defined here because it is the shape a
//! future decoder's output takes, and because [`Rect`] and [`Palette`] are
//! genuinely useful *before* that: some containers state a rectangle's size
//! or a palette's colours as plain, uncompressed header fields, with no
//! run-length coding in between —
//!
//! * `VobSub`'s `.idx` file states the subtitle canvas size (`size: WxH`) and
//!   a 16-entry palette (`palette: rrggbb, rrggbb, …`) as **plain text**. No
//!   compression, no decoding — reading it is exactly as much container work
//!   as reading a PNG `IHDR` chunk's width/height, and `vaco-subtitle-bitmap`'s
//!   `vobsub` module does it directly into a [`Rect`] and a [`Palette`].
//! * DVB's CLUT-definition and region-composition segments (EN 300 743 §7.2.4,
//!   §7.2.3) state a palette table and a region's bounding box as fixed-width
//!   integer fields, again with no compression. `vaco-subtitle-bitmap`'s
//!   `dvbsub::segments` module parses these as *structural helpers* — not
//!   wired into the registered demuxer's packetisation, which matches the
//!   measured reference behaviour of blind fixed-size chunking (see that
//!   crate's docs) — but real, tested, fuzzed code that a decoder can use
//!   directly, and that exercises the one defensive fact this crate exists to
//!   enforce:
//!
//! **A region or object claiming a 65535×65535 rectangle is exactly the shape
//! of a real finding**, not a hypothetical one (`planning/AGENT-CONSTRAINTS.md`,
//! "Fuzzing"). [`Rect::new`] is the only way to build one, and it checks
//! width/height against [`vaco_limits::Limits::max_dimension`] before anything
//! downstream sizes a pixel buffer from it.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`rect`] | [`Rect`] — a bounds-checked bitmap position and size |
//! | [`palette`] | [`Palette`] and [`Rgba`] — an indexed colour table, ≤256 entries |
//! | [`color`] | DVB's `Y Cr Cb T` colour space and its BT.601 conversion to RGBA |
//! | [`bitmap`] | [`IndexedBitmap`] — a decoded rectangle of palette indices |
//!
//! # Dependencies
//!
//! `vaco-core` for [`vaco_core::Result`]/[`vaco_core::Error`], `vaco-limits`
//! for [`vaco_limits::Limits`] and [`vaco_limits::Budget`]. Nothing else —
//! deliberately not `vaco-codec-core`: this crate carries no `CodecId`, no
//! `CodecParameters`, nothing a demuxer/muxer descriptor needs. Those stay in
//! `vaco-subtitle-bitmap`, the crate that actually registers with
//! `vaco-format-core`.

#![forbid(unsafe_code)]

pub mod bitmap;
pub mod color;
pub mod palette;
pub mod rect;

pub use bitmap::IndexedBitmap;
pub use color::ycbcrt_to_rgba;
pub use palette::{Palette, Rgba};
pub use rect::Rect;
