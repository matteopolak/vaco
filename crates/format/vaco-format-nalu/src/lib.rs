//! NAL unit framing shared by H.264, HEVC and VVC.
//!
//! # What this crate is, and what `vaco-bitstream` already is
//!
//! `vaco-bitstream` is layer 0 and owns the two *primitives* a container needs
//! to look inside a parameter set without depending on a codec crate (D14.1):
//! [`annexb::find_start_code`](vaco_bitstream::annexb::find_start_code) with its
//! whole-buffer iterator, and [`to_rbsp`](vaco_bitstream::annexb::to_rbsp) /
//! [`to_ebsp`](vaco_bitstream::annexb::to_ebsp). Nothing here re-implements any
//! of that; every scan and every escape below calls through to it, and a
//! property test asserts the agreement.
//!
//! This crate is the layer above: the part a *parser* needs and layer 0
//! deliberately does not carry.
//!
//! | Here | Why not in `vaco-bitstream` |
//! |---|---|
//! | [`Framing`] and one iterator over **both** framings | layer 0 has two separate iterators, so every caller writes the same match |
//! | [`Nal`] carrying the source **offset** and start-code length | a parser must report how many bytes it consumed, and a demuxer wants `Packet::pos` |
//! | [`RbspBuf`] — de-escape **directly into a padded buffer** | `to_rbsp` yields `&[u8]`; making that readable by the fast path costs a second copy |
//! | [`Scanner`] — incremental, resumable framing across chunk boundaries | layer 0's iterator is whole-buffer only |
//! | [`convert`] — Annex B ↔ length-prefixed, budgeted | layer 0 has no [`Budget`](vaco_limits::Budget) in scope |
//! | [`NalHeader`] — the H.264/HEVC/VVC header layouts | codec knowledge, however small |
//! | [`extradata`] — assemble extradata from in-band parameter sets | the one shared floor `vaco-bsf-generic` and `vaco-format-core` both stand on (D19; CONFORMANCE-FINDINGS 26) |
//!
//! # The one idea worth reading first
//!
//! **De-escaping and padding are the same copy.** Every parser in the H.26x
//! family does the same three things to a NAL unit: strip emulation-prevention
//! bytes, put the result somewhere a [`BitReader`](vaco_bitstream::BitReader)
//! can read fast, and reuse that buffer for the next unit. Done naively that is
//! two copies and two allocations per NAL, tens of thousands of times per file.
//! [`RbspBuf`] does it in one pass into one reusable allocation whose tail is
//! already the 64 zero bytes [`Padded`](vaco_bitstream::Padded) requires, so
//! [`RbspBuf::padded`] is free.
//!
//! # Safety on untrusted input
//!
//! Everything here is driven by attacker-controlled byte counts, so:
//!
//! * **No unbounded loop.** Every iterator advances its cursor by at least one
//!   byte per step, and [`Scanner`] cannot return the same boundary twice.
//! * **No indexing.** `indexing_slicing` is denied workspace-wide; every access
//!   goes through `get`.
//! * **Every allocation is charged.** [`RbspBuf`] and [`convert`] take a
//!   [`Budget`](vaco_limits::Budget), so a declared length cannot amplify into
//!   memory.
//!
//! # Example
//!
//! ```
//! use vaco_format_nalu::{Framing, RbspBuf, units};
//! use vaco_limits::{Budget, Limits};
//!
//! let stream = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 3, 0x01, 0, 0, 1, 0x68, 0xCE];
//! let nals: Vec<_> = units(&stream, Framing::AnnexB).collect();
//! assert_eq!(nals.len(), 2);
//! assert_eq!(nals[0].offset, 4);
//! assert_eq!(nals[0].start_code_len, 4);
//!
//! let mut budget = Budget::new(Limits::strict());
//! let mut rbsp = RbspBuf::new();
//! rbsp.fill(nals[0].data, &mut budget)?;
//! // The `00 00 03 01` escape became `00 00 01`.
//! assert_eq!(rbsp.as_slice(), &[0x67, 0x42, 0, 0, 0x01]);
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Specification
//!
//! ITU-T H.264 (ISO/IEC 14496-10) Annex B for the byte-stream format, §7.3.1
//! and §7.4.1 for the NAL unit syntax and `emulation_prevention_three_byte`,
//! and §7.4.1.2 for the order of units within an access unit. ITU-T H.265
//! §7.3.1.1 and ITU-T H.266 §7.3.1.2 for their two-byte headers. ISO/IEC
//! 14496-15 §5.3.3 for the length-prefixed form. Nothing here was taken from
//! any implementation (D7).
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the scanner and the escape primitives, `vaco-limits` for
//! the budget, `vaco-core` for the error taxonomy. No external runtime
//! dependencies.

#![forbid(unsafe_code)]

pub mod avcc;
pub mod convert;
pub mod extradata;
mod framing;
mod header;
mod rbsp;
mod scan;

pub use avcc::build_h264_avcc;
pub use convert::{annexb_to_length_prefixed, length_prefixed_to_annexb};
pub use extradata::{assemble_extradata, header_kind_for, is_parameter_set, parameter_sets};
pub use framing::{Framing, LengthSize, Nal, NalUnits, units};
pub use header::{HeaderKind, NalHeader};
pub use rbsp::{RbspBuf, escape_into};
pub use scan::{Scanner, StartCode};

// Re-exported so a caller can name the framing primitives without also
// depending on `vaco-bitstream` directly.
pub use vaco_bitstream::annexb::{find_start_code, violates_ebsp_constraint};
