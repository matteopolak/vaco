//! The coded bitstream layer: read a stream's units, edit them, write them
//! back — **without decoding anything**.
//!
//! # What this is for
//!
//! A bitstream filter changes a stream without re-encoding it. `filter_units`
//! drops SEI messages; `hevc_metadata` rewrites the VUI's colour signalling;
//! `hevc_mp4toannexb` changes the framing; `extract_extradata` lifts the
//! parameter sets out of the first access unit. None of them decodes a picture,
//! and all of them need the same thing: a stream expressed as an ordered list
//! of units, each of which can be read, replaced, inserted, dropped, and
//! written back out.
//!
//! That list is [`CbsFragment`]. This crate owns it, plus the editing
//! operations on it and the typed read/modify/write cycle in [`Cbs`]. It owns
//! no codec syntax at all.
//!
//! # The split of responsibility
//!
//! | This crate | The codec crate |
//! |---|---|
//! | [`CbsFragment`], [`CbsUnit`] — the unit list and its edits | [`CbsCodec::split`] / [`CbsCodec::assemble`] — the framing |
//! | [`Cbs`] — the read → edit → write cycle | [`CbsCodec::read_unit`] / [`CbsCodec::write_unit`] — the syntax |
//! | budget accounting for the whole fragment | the per-element bounds inside one unit |
//!
//! A codec implements [`CbsCodec`] and gets the rest. `vaco-parse-hevc`'s
//! `cbs::HevcCbs` is the worked example.
//!
//! # Two decisions that keep it codec-agnostic
//!
//! 1. **A unit's type is a `u32`, not an enum.** `filter_units` needs to know
//!    "drop type 39", not what type 39 means. Every codec-specific meaning
//!    stays in the codec crate, so this crate never grows a match over codecs
//!    (plan 10 §1.5).
//! 2. **Framing is an associated type.** H.26x's Annex B and length prefixes,
//!    AV1's Annex B and low-overhead, JPEG's markers: no single enum covers
//!    them, and the H.26x one lives in `vaco-format-nalu`, which is *above*
//!    this crate. [`CbsCodec::Framing`] lets the codec bring its own; this
//!    crate only carries the value through.
//!
//! # Units hold escaped bytes, deliberately
//!
//! [`CbsUnit::data`] is the unit as it appears in the bitstream — framing
//! removed, emulation prevention **intact**. De-escaping and re-escaping is not
//! the identity: a conforming encoder may leave a trailing `00 00` unescaped
//! where a re-escape would write `00 00 03`. Storing the de-escaped form would
//! therefore rewrite units a filter was asked to leave alone, and a filter that
//! changes bytes it did not touch is not a filter. See [`unit::CbsUnit`].
//!
//! # Safety on untrusted input
//!
//! Every unit pushed into a fragment is charged against a
//! [`Budget`](vaco_limits::Budget) and costs one unit of fuel, so a buffer that
//! splits into ten million one-byte units runs out of budget rather than
//! memory. Nothing here indexes; `indexing_slicing` is denied workspace-wide.
//! An index past the end appends or returns `None` — never panics.
//!
//! # Example
//!
//! ```ignore
//! // Drop every SEI unit from an access unit, keeping the framing.
//! let mut cbs = Cbs::new(HevcCbs::new());
//! cbs.transform(input, Framing::AnnexB, Framing::AnnexB, &mut out, &mut budget,
//!     |_cbs, fragment, _budget| {
//!         fragment.retain(|u| u.unit_type != 39 && u.unit_type != 40);
//!         Ok(())
//!     })?;
//! ```
//!
//! # What is deliberately not here
//!
//! * **A `trace_headers` sink.** Plan 15 lists one. It needs every codec's
//!   reader to report each syntax element as it is read, which is a change to
//!   the *readers*, not to this crate — and adding the sink before a reader
//!   calls it would be an untested API. Deferred, with the shape recorded in
//!   `docs/signal/vaco-codec-cbs.md`.
//! * **Escaping helpers.** [`vaco_bitstream::annexb`] already owns those at
//!   layer 0 and they are re-exported below, so a caller needs one dependency
//!   rather than two. `vaco-format-nalu`'s `RbspBuf` is the fast path for a
//!   parser that de-escapes every unit of a long stream.
//!
//! # Specification
//!
//! Codec-agnostic; the syntax lives in the codec crates. The framing concepts
//! come from ITU-T H.264 Annex B / ITU-T H.265 Annex B and ISO/IEC 14496-15.
//! Nothing here was taken from any implementation (D7).
//!
//! # Dependencies
//!
//! `vaco-core` for the error taxonomy, `vaco-limits` for the budget,
//! `vaco-bitstream` for the re-exported escaping primitives. `vaco-codec-core`
//! for nothing yet — it is declared because a future `BitstreamFilter` adapter
//! belongs here. No external runtime dependencies.

#![forbid(unsafe_code)]

pub mod codec;
pub mod unit;

pub use codec::{Cbs, CbsCodec};
pub use unit::{CbsFragment, CbsUnit, UnitOrigin};

// Re-exported so a caller that has this crate does not also need
// `vaco-bitstream` just to de-escape a unit it decided to inspect by hand.
pub use vaco_bitstream::annexb::{to_ebsp, to_rbsp, violates_ebsp_constraint};
